use super::*;

/// Backfill missing vector embeddings for a given DB.
pub(super) async fn run_backfill_vectors(
    db_path: &PathBuf,
    vault_db_path: &PathBuf,
    batch_size: usize,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::llm::LlmClient;

    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;

    let store = MemoryStore::open(db_str)?;
    let (total, with_vec) = store.vector_stats()?;
    let missing = total - with_vec;

    println!("DB:      {}", db_path.display());
    println!("Total:   {total}");
    println!("Vectors: {with_vec}");
    println!("Missing: {missing}");

    if missing == 0 {
        println!("\n✅ All entries have vectors!");
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run mode, no changes made)");
        return Ok(());
    }

    let llm = LlmClient::new().map_err(|e| format!("LLM client init failed: {e}"))?;
    match load_keychain_vault_api_key_values(vault_db_path) {
        Ok(secrets) => {
            let loaded = llm.set_provider_secrets(secrets);
            if loaded > 0 {
                println!("Loaded {loaded} API key(s) from Tachi Vault for backfill.");
            }
        }
        Err(err) => {
            eprintln!("  WARN: could not load Vault API keys for backfill: {err}");
            eprintln!("  WARN: falling back to environment/config.env provider keys.");
        }
    }
    let entries = store.entries_missing_vectors()?;

    let batch_size = batch_size.min(128).max(1);
    let total_missing = entries.len();
    let mut processed = 0usize;

    println!("\nBackfilling {total_missing} entries (batch_size={batch_size})...\n");

    // Re-open as mutable for update_enrichment_fields
    drop(store);
    let mut store = MemoryStore::open(db_str)?;

    for chunk in entries.chunks(batch_size) {
        let texts: Vec<String> = chunk
            .iter()
            .map(|(_, text, summary, _)| {
                let t = text.trim();
                let s = if t.len() < 10 { summary.as_str() } else { t };
                if s.len() > 8000 {
                    s.chars().take(8000).collect()
                } else {
                    s.to_string()
                }
            })
            .collect();

        match llm.embed_voyage_batch(&texts, "document").await {
            Ok(vecs) => {
                for (i, (id, _, _, revision)) in chunk.iter().enumerate() {
                    if i < vecs.len() {
                        match store.update_enrichment_fields(id, None, Some(&vecs[i]), *revision) {
                            Ok(true) => {}
                            Ok(false) => eprintln!("  WARN: revision mismatch for {id}, skipped"),
                            Err(e) => eprintln!("  WARN: DB write failed for {id}: {e}"),
                        }
                    }
                }
                processed += chunk.len();
                println!("  [{processed}/{total_missing}] ✓ batch of {}", chunk.len());
            }
            Err(e) => {
                eprintln!("  ERROR: Voyage API failed: {e}");
                eprintln!("  Stopping. {processed} entries saved successfully.");
                break;
            }
        }

        if processed < total_missing {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    let (total, final_vec) = store.vector_stats()?;
    println!("\n✅ Done! Vectors: {with_vec} → {final_vec} / {total}");
    Ok(())
}

fn load_keychain_vault_api_key_values(
    vault_db_path: &PathBuf,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "tachi-vault",
            "-a",
            "default",
            "-w",
        ])
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let password = String::from_utf8(output.stdout)?.trim().to_string();
    if password.is_empty() {
        return Ok(Vec::new());
    }

    let vault_db_str = vault_db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Vault DB path contains invalid UTF-8: {}",
                vault_db_path.display()
            ),
        )
    })?;
    let store = MemoryStore::open_read_only(vault_db_str)?;
    let Some(config) = store.vault_get_config()? else {
        return Ok(Vec::new());
    };

    let salt = B64.decode(&config.salt)?;
    let key = crate::vault_crypto::derive_key(&password, &salt)?;
    if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in store.vault_list_entries()? {
        if entry.secret_type != "api_key"
            || !entry.name.ends_with("_API_KEY")
            || entry.allowed_agents.is_some()
        {
            continue;
        }
        let decrypted = crate::vault_crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)?;
        if !value.trim().is_empty() {
            out.push((entry.name, value));
        }
    }
    Ok(out)
}

/// Backfill missing summaries for a given DB.
pub(super) async fn run_backfill_summaries(
    db_path: &PathBuf,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::llm::LlmClient;

    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;

    let store = MemoryStore::open(db_str)?;
    let total = store.stats(false)?.total;
    let entries = store.entries_missing_summaries()?;
    let missing = entries.len();
    let with_summary = total.saturating_sub(missing as u64);

    println!("DB:        {}", db_path.display());
    println!("Total:     {total}");
    println!("Summaries: {with_summary}");
    println!("Missing:   {missing}");

    if missing == 0 {
        println!("\n✅ All entries have summaries!");
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run mode, no changes made)");
        return Ok(());
    }

    let llm = LlmClient::new().map_err(|e| format!("LLM client init failed: {e}"))?;

    println!("\nBackfilling {missing} entries...\n");

    drop(store);
    let mut store = MemoryStore::open(db_str)?;
    let mut processed = 0usize;

    for (id, text, revision) in &entries {
        let input: String = text.chars().take(8000).collect();
        let summary = llm.generate_summary(&input).await?;

        match store.update_enrichment_fields(id, Some(&summary), None, *revision) {
            Ok(true) => {
                processed += 1;
                println!("  [{processed}/{missing}] ✓ {id}");
            }
            Ok(false) => eprintln!("  WARN: revision mismatch for {id}, skipped"),
            Err(e) => eprintln!("  WARN: DB write failed for {id}: {e}"),
        }
    }

    let final_missing = store.entries_missing_summaries()?.len();
    let final_with_summary = total.saturating_sub(final_missing as u64);
    println!("\n✅ Done! Summaries: {with_summary} → {final_with_summary} / {total}");
    Ok(())
}

/// Rebuild or backfill FTS5 full-text search index for a given DB.
pub(super) async fn run_backfill_fts(
    db_path: &PathBuf,
    full: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;

    let store = MemoryStore::open(db_str)?;
    let (total, with_fts) = store.fts_stats()?;
    let missing = total.saturating_sub(with_fts);

    println!("DB:      {}", db_path.display());
    println!("Total:   {total}");
    println!("FTS:     {with_fts}");
    println!("Missing: {missing}");
    println!(
        "Mode:    {}",
        if full { "full rebuild" } else { "incremental" }
    );

    if !full && missing == 0 {
        println!("\n✅ All entries have FTS index!");
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run mode, no changes made)");
        return Ok(());
    }

    drop(store);
    let mut store = MemoryStore::open(db_str)?;

    if full {
        println!("\nDropping and rebuilding FTS table...");
        let inserted = store.rebuild_fts_full()?;
        println!("\n✅ Full rebuild done! {inserted} entries indexed.");
    } else {
        println!("\nBackfilling {missing} missing FTS entries...");
        let inserted = store.backfill_fts_missing()?;
        let (_, final_fts) = store.fts_stats()?;
        println!("\n✅ Done! FTS: {with_fts} → {final_fts} / {total} (+{inserted})");
    }

    Ok(())
}

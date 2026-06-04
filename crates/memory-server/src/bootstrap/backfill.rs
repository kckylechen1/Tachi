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
    crate::provider_config::materialize_standalone(&llm, vault_db_path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let entries = crate::vector_backfill::list_missing_vector_entries(&store, false, None)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    let batch_size = batch_size.min(128).max(1);
    let total_missing = entries.len();
    let mut processed = 0usize;

    println!("\nBackfilling {total_missing} entries (batch_size={batch_size})...\n");

    drop(store);
    let mut store = MemoryStore::open(db_str)?;

    for chunk in entries.chunks(batch_size) {
        match crate::vector_backfill::embed_and_write_batch(&mut store, &llm, chunk).await {
            Ok(n) => {
                processed += n;
                println!("  [{processed}/{total_missing}] ✓ batch of {}", chunk.len());
            }
            Err(e) => {
                eprintln!("  ERROR: {e}");
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

        match store.update_enrichment_fields(id, Some(&summary), None, None, None, *revision) {
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

/// Backfill missing keywords/entities using the configured extract LLM.
pub(super) async fn run_backfill_metadata(
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
    let (total, with_metadata) = store.metadata_stats()?;
    let entries = store.entries_missing_metadata()?;
    let missing = entries.len();

    println!("DB:       {}", db_path.display());
    println!("Total:    {total}");
    println!("Metadata: {with_metadata}");
    println!("Missing:  {missing}");

    if missing == 0 {
        println!("\n✅ All entries have keywords and entities!");
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run mode, no changes made)");
        return Ok(());
    }

    let llm = LlmClient::new().map_err(|e| format!("LLM client init failed: {e}"))?;

    println!("\nBackfilling metadata for {missing} entries...\n");

    drop(store);
    let mut store = MemoryStore::open(db_str)?;
    let mut processed = 0usize;

    for (id, text, _summary, revision) in &entries {
        let input: String = text.chars().take(8000).collect();
        let (keywords, entities) = llm.extract_metadata(&input).await?;
        let (heur_keywords, heur_entities) =
            memory_core::scorer::heuristic_metadata_from_text(&input);
        let mut keywords = keywords;
        let mut entities = entities;
        for kw in heur_keywords {
            if !keywords.iter().any(|k| k == &kw) {
                keywords.push(kw);
            }
        }
        for ent in heur_entities {
            if !entities.iter().any(|e| e == &ent) {
                entities.push(ent);
            }
        }

        match store.update_enrichment_fields(
            id,
            None,
            None,
            if keywords.is_empty() {
                None
            } else {
                Some(&keywords)
            },
            if entities.is_empty() {
                None
            } else {
                Some(&entities)
            },
            *revision,
        ) {
            Ok(true) => {
                processed += 1;
                println!("  [{processed}/{missing}] ✓ {id}");
            }
            Ok(false) => eprintln!("  WARN: revision mismatch for {id}, skipped"),
            Err(e) => eprintln!("  WARN: DB write failed for {id}: {e}"),
        }
    }

    let (_, final_with_metadata) = store.metadata_stats()?;
    println!("\n✅ Done! Metadata coverage: {with_metadata} → {final_with_metadata} / {total}");
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

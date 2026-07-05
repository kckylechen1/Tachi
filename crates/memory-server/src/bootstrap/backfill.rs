use crate::provider_config::materialize_standalone;
use crate::vector_backfill::{embed_and_write_batch, list_missing_vector_entries};
use futures::{stream, StreamExt};
use memory_core::MemoryStore;
use std::error::Error;
use std::fmt::Display;
use std::io::{Error as IoError, ErrorKind};
use std::path::PathBuf;
use std::time::Duration;
use tachi_llm::LlmClient;

const DEFAULT_BACKFILL_LLM_CONCURRENCY: usize = 4;
const MAX_BACKFILL_LLM_CONCURRENCY: usize = 32;

/// Backfill missing vector embeddings for a given DB.
pub(super) async fn run_backfill_vectors(
    db_path: &PathBuf,
    vault_db_path: &PathBuf,
    batch_size: usize,
    dry_run: bool,
    include_cache: bool,
) -> Result<(), Box<dyn Error>> {
    const FOUNDRY_RECALL_CACHE_SOURCE: &str = "foundry_recall_rerank_cache";

    let db_str = db_path.to_str().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;

    let store = MemoryStore::open(db_str)?;
    let skip_recall_cache = !include_cache;
    let (total, with_vec) = if skip_recall_cache {
        let total: i64 = store.connection().query_row(
            "SELECT COUNT(*) FROM memories WHERE source != ?1",
            [FOUNDRY_RECALL_CACHE_SOURCE],
            |r| r.get(0),
        )?;
        let with_vec: i64 = store.connection().query_row(
            "SELECT COUNT(DISTINCT v.id)
             FROM memories_vec v
             JOIN memories m ON m.id = v.id
             WHERE m.source != ?1",
            [FOUNDRY_RECALL_CACHE_SOURCE],
            |r| r.get(0),
        )?;
        (total, with_vec)
    } else {
        store.vector_stats()?
    };
    let missing = total - with_vec;

    println!("DB:      {}", db_path.display());
    println!("Total:   {total}");
    println!("Vectors: {with_vec}");
    println!("Missing: {missing}");
    if skip_recall_cache {
        println!("Scope:   durable rows (recall cache excluded; pass --include-cache to include)");
    }

    if missing == 0 {
        println!("\n✅ All entries have vectors!");
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run mode, no changes made)");
        return Ok(());
    }

    let llm = LlmClient::new().map_err(|e| format!("LLM client init failed: {e}"))?;
    materialize_standalone(&llm, vault_db_path).map_err(|e| IoError::new(ErrorKind::Other, e))?;
    let entries = list_missing_vector_entries(&store, skip_recall_cache, None)
        .map_err(|e| IoError::new(ErrorKind::Other, e))?;

    let batch_size = batch_size.min(128).max(1);
    let total_missing = entries.len();
    let mut processed = 0usize;

    println!("\nBackfilling {total_missing} entries (batch_size={batch_size})...\n");

    drop(store);
    let mut store = MemoryStore::open(db_str)?;

    for chunk in entries.chunks(batch_size) {
        match embed_and_write_batch(&mut store, &llm, chunk).await {
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
    vault_db_path: &PathBuf,
    dry_run: bool,
) -> Result<(), Box<dyn Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidInput,
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

    let llm = LlmClient::new_with_vault_db(Some(vault_db_path))
        .map_err(|e| format!("LLM client init failed: {e}"))?;
    materialize_standalone(&llm, vault_db_path).map_err(|e| IoError::new(ErrorKind::Other, e))?;
    let concurrency = backfill_llm_concurrency();

    println!("\nBackfilling {missing} entries (concurrency={concurrency})...\n");

    drop(store);
    let mut store = MemoryStore::open(db_str)?;
    let tasks = stream::iter(entries.into_iter().map(|(id, text, revision)| {
        let llm = llm.clone();
        let input: String = text.chars().take(8000).collect();
        async move {
            let result = generate_summary_with_retry(&llm, &input).await;
            (id, revision, result)
        }
    }))
    .buffer_unordered(concurrency);
    tokio::pin!(tasks);

    let mut attempted = 0usize;
    let mut processed = 0usize;
    let mut failed = 0usize;

    while let Some((id, revision, result)) = tasks.next().await {
        attempted += 1;
        let summary = match result {
            Ok(summary) => summary,
            Err(error) => {
                failed += 1;
                eprintln!("  WARN: summary failed for {id}: {error}");
                record_backfill_failure(&store, &id, "summary", &error);
                continue;
            }
        };

        match store.update_enrichment_fields(&id, Some(&summary), None, None, None, revision) {
            Ok(true) => {
                processed += 1;
                println!("  [{attempted}/{missing}] ✓ {id}");
            }
            Ok(false) => eprintln!("  WARN: revision mismatch for {id}, skipped"),
            Err(error) => {
                failed += 1;
                eprintln!("  WARN: DB write failed for {id}: {error}");
                record_backfill_failure(&store, &id, "db_update", &error);
            }
        }
    }

    let final_missing = store.entries_missing_summaries()?.len();
    let final_with_summary = total.saturating_sub(final_missing as u64);
    if failed == 0 {
        println!("\n✅ Done! Summaries: {with_summary} → {final_with_summary} / {total}");
    } else {
        println!(
            "\n⚠ Done with {failed} failure(s). Summaries: {with_summary} → {final_with_summary} / {total}; updated {processed}/{missing}"
        );
    }
    Ok(())
}

/// Backfill missing recall keywords using the configured extract LLM.
pub(super) async fn run_backfill_metadata(
    db_path: &PathBuf,
    vault_db_path: &PathBuf,
    dry_run: bool,
) -> Result<(), Box<dyn Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidInput,
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
        println!("\n✅ All entries have recall keywords!");
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run mode, no changes made)");
        return Ok(());
    }

    let llm = LlmClient::new_with_vault_db(Some(vault_db_path))
        .map_err(|e| format!("LLM client init failed: {e}"))?;
    materialize_standalone(&llm, vault_db_path).map_err(|e| IoError::new(ErrorKind::Other, e))?;
    let concurrency = backfill_llm_concurrency();

    println!("\nBackfilling metadata for {missing} entries (concurrency={concurrency})...\n");

    drop(store);
    let mut store = MemoryStore::open(db_str)?;
    let tasks = stream::iter(entries.into_iter().map(|(id, text, _summary, revision)| {
        let llm = llm.clone();
        let input: String = text.chars().take(8000).collect();
        async move {
            let result = extract_metadata_with_retry(&llm, &input).await;
            (id, revision, result)
        }
    }))
    .buffer_unordered(concurrency);
    tokio::pin!(tasks);

    let mut attempted = 0usize;
    let mut processed = 0usize;
    let mut failed = 0usize;

    while let Some((id, revision, result)) = tasks.next().await {
        attempted += 1;
        let (keywords, entities) = match result {
            Ok(metadata) => metadata,
            Err(error) => {
                failed += 1;
                eprintln!("  WARN: metadata failed for {id}: {error}");
                record_backfill_failure(&store, &id, "metadata", &error);
                continue;
            }
        };
        // Domain-specific deterministic tagging was removed from the generic
        // engine; use the LLM-extracted keywords/entities directly.

        match store.update_enrichment_fields(
            &id,
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
            revision,
        ) {
            Ok(true) => {
                processed += 1;
                println!("  [{attempted}/{missing}] ✓ {id}");
            }
            Ok(false) => eprintln!("  WARN: revision mismatch for {id}, skipped"),
            Err(error) => {
                failed += 1;
                eprintln!("  WARN: DB write failed for {id}: {error}");
                record_backfill_failure(&store, &id, "db_update", &error);
            }
        }
    }

    let (_, final_with_metadata) = store.metadata_stats()?;
    if failed == 0 {
        println!("\n✅ Done! Metadata coverage: {with_metadata} → {final_with_metadata} / {total}");
    } else {
        println!(
            "\n⚠ Done with {failed} failure(s). Metadata coverage: {with_metadata} → {final_with_metadata} / {total}; updated {processed}/{missing}"
        );
    }
    Ok(())
}

fn backfill_llm_concurrency() -> usize {
    std::env::var("TACHI_BACKFILL_LLM_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_BACKFILL_LLM_CONCURRENCY)
        .clamp(1, MAX_BACKFILL_LLM_CONCURRENCY)
}

async fn generate_summary_with_retry(llm: &LlmClient, input: &str) -> Result<String, String> {
    retry_llm_call("summary", || llm.generate_summary(input)).await
}

async fn extract_metadata_with_retry(
    llm: &LlmClient,
    input: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    retry_llm_call("metadata", || llm.extract_metadata(input)).await
}

async fn retry_llm_call<F, Fut, T>(stage: &str, mut call: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    const MAX_ATTEMPTS: usize = 5;
    let mut last_error = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match call().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let delay = retry_delay_for_error(&error, attempt);
                last_error = Some(error);
                if attempt < MAX_ATTEMPTS {
                    eprintln!(
                        "  WARN: {stage} attempt {attempt}/{MAX_ATTEMPTS} failed; retrying in {}s",
                        delay.as_secs_f32()
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| format!("{stage} failed without an error")))
}

fn retry_delay_for_error(error: &str, attempt: usize) -> Duration {
    provider_cooldown_delay(error).unwrap_or_else(|| Duration::from_millis(500 * attempt as u64))
}

fn provider_cooldown_delay(error: &str) -> Option<Duration> {
    let marker = "retry after about ";
    let tail = error.split(marker).nth(1)?;
    let seconds: u64 = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds.saturating_add(1).min(300)))
}

fn record_backfill_failure(store: &MemoryStore, id: &str, stage: &str, error: impl Display) {
    let error = bounded_error(error);
    if let Err(record_error) = store.record_enrichment_failure(id, stage, &error) {
        eprintln!("  WARN: failed to record enrichment failure for {id}: {record_error}");
    }
}

fn bounded_error(error: impl Display) -> String {
    let error = error.to_string();
    const MAX_ERROR_CHARS: usize = 1_000;
    if error.chars().count() <= MAX_ERROR_CHARS {
        return error;
    }
    let mut truncated: String = error.chars().take(MAX_ERROR_CHARS).collect();
    truncated.push_str("...");
    truncated
}

/// Rebuild or backfill FTS5 full-text search index for a given DB.
pub(super) async fn run_backfill_fts(
    db_path: &PathBuf,
    full: bool,
    dry_run: bool,
) -> Result<(), Box<dyn Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidInput,
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

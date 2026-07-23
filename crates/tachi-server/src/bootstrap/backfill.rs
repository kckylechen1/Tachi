use crate::provider_config::materialize_standalone;
use crate::vector_backfill::{
    embed_and_write_batch, list_missing_vector_entries, record_vector_sweep_state,
    VectorSweepStateUpdate,
};
use futures::{stream, StreamExt};
use memcore::{DbOpenContext, MemoryStore, MigrationAuthority, OpenIntent, VectorBackfillScope};
use std::error::Error;
use std::fmt::Display;
use std::io::{Error as IoError, ErrorKind};
use std::path::PathBuf;
use std::time::Duration;
use tachi_llm::LlmClient;

const DEFAULT_BACKFILL_LLM_CONCURRENCY: usize = 4;
const MAX_BACKFILL_LLM_CONCURRENCY: usize = 32;

/// #1279: backfill stays strict — batch jobs fail-fast on skipped aliases.
///
/// The server refresh path tolerates a skipped `vault:` alias (degrade only that
/// lane), but a batch sweep is all-or-nothing: a missing/locked key would let the
/// sweep proceed and then either embed/summarize zero rows or storm per-row LLM
/// failures deep inside the job. So a batch that materialized any skipped alias
/// refuses up front, carrying the shared `vault_unlock`/`vault_set` remediation.
///
/// POLICY (pending owner/upstream ratify, tachi#1279): strict-here vs.
/// tolerant-in-server is a deliberate split, not an oversight — if a batch should
/// instead skip only the affected provider and continue, that is an owner call.
fn ensure_no_skipped_aliases(
    report: &crate::provider_config::MaterializeReport,
) -> Result<(), Box<dyn Error>> {
    if report.skipped_aliases.is_empty() {
        return Ok(());
    }
    Err(IoError::new(
        ErrorKind::Other,
        crate::provider_config::describe_skipped_aliases(&report.skipped_aliases),
    )
    .into())
}

/// #1181: build the write-open [`DbOpenContext`] every `backfill-*` in-process
/// open uses, threading the top-level `--allow-schema-migration` decision
/// (resolved once into a typed [`MigrationAuthority`] at CLI startup,
/// `bootstrap::serve::initialize_startup_context`) instead of silently
/// carrying the fail-closed `Deny` every `MemoryStore::open` default builds.
/// `--help` documents the flag as reaching every in-process DB open; before
/// this fix, `backfill-*` opens ignored it, blocking the exact rehearsal
/// workflow #1168 prescribes (verify migration on a disposable copy of a
/// real legacy DB before deploying).
fn backfill_write_open_context(schema_migration: &MigrationAuthority) -> DbOpenContext {
    DbOpenContext {
        intent: OpenIntent::OpenExisting,
        migration: schema_migration.clone(),
    }
}

/// Total / with-vector counts for `tachi backfill-vectors`'s Total/Missing
/// report, over the portable memcore backfill population. That population
/// preserves status's full recall-cache classification (id/source/topic/path/
/// metadata/cache-key) while also keeping selection and count membership in
/// one implementation.
fn durable_vector_counts(
    store: &MemoryStore,
    skip_recall_cache: bool,
) -> Result<(i64, i64), Box<dyn Error>> {
    let counts = store.vector_backfill_counts(VectorBackfillScope {
        include_cache: !skip_recall_cache,
    })?;
    Ok((counts.total as i64, counts.with_vector as i64))
}

/// Backfill missing vector embeddings for a given DB.
pub(super) async fn run_backfill_vectors(
    db_path: &PathBuf,
    vault_db_path: &PathBuf,
    batch_size: usize,
    dry_run: bool,
    include_cache: bool,
    schema_migration: &MigrationAuthority,
) -> Result<(), Box<dyn Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;
    let open_ctx = backfill_write_open_context(schema_migration);

    let store = if dry_run {
        MemoryStore::open_read_only(db_str)?
    } else {
        MemoryStore::open_with_context(db_str, &open_ctx)?
    };
    let skip_recall_cache = !include_cache;
    let (total, with_vec) = durable_vector_counts(&store, skip_recall_cache)?;
    let missing = total - with_vec;

    println!("DB:      {}", db_path.display());
    println!("Total:   {total}");
    println!("Vectors: {with_vec}");
    println!("Missing: {missing}");
    if skip_recall_cache {
        println!("Scope:   durable rows (recall cache excluded; pass --include-cache to include)");
    }

    if dry_run {
        println!("\n(dry-run mode, opened read-only; no changes made)");
        return Ok(());
    }

    if missing == 0 {
        record_cli_vector_sweep_state(db_path, skip_recall_cache, 0, 0, None);
        println!("\n✅ All entries have vectors!");
        return Ok(());
    }

    let llm = LlmClient::new().map_err(|e| format!("LLM client init failed: {e}"))?;
    let materialize_report = materialize_standalone(&llm, vault_db_path)
        .map_err(|e| IoError::new(ErrorKind::Other, e))?;
    // #1279: backfill stays strict — batch jobs fail-fast on skipped aliases.
    ensure_no_skipped_aliases(&materialize_report)?;
    let entries = list_missing_vector_entries(&store, skip_recall_cache, None)
        .map_err(|e| IoError::new(ErrorKind::Other, e))?;

    let batch_size = batch_size.min(128).max(1);
    let total_missing = entries.len();
    let mut processed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut last_error = None;

    println!("\nBackfilling {total_missing} entries (batch_size={batch_size})...\n");

    drop(store);
    let mut store = MemoryStore::open_with_context(db_str, &open_ctx)?;

    for chunk in entries.chunks(batch_size) {
        match embed_and_write_batch(&mut store, &llm, chunk).await {
            Ok(outcome) => {
                processed += outcome.written_count;
                skipped += outcome.skipped_count;
                failed += outcome.failed_count;
                println!("  [{processed}/{total_missing}] ✓ batch of {}", chunk.len());
            }
            Err(e) => {
                eprintln!("  ERROR: {e}");
                eprintln!("  Stopping. {processed} entries saved successfully.");
                failed += 1;
                last_error = Some(e);
                break;
            }
        }

        if processed + skipped + failed < total_missing {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    let (total, final_vec) = durable_vector_counts(&store, skip_recall_cache)?;
    record_cli_vector_sweep_state(db_path, skip_recall_cache, processed, failed, last_error);
    println!("\n✅ Done! Vectors: {with_vec} → {final_vec} / {total}");
    Ok(())
}

fn record_cli_vector_sweep_state(
    db_path: &PathBuf,
    skip_recall_cache: bool,
    embedded_count: usize,
    failed_count: usize,
    last_error: Option<String>,
) {
    let failed_count = if last_error.is_some() && failed_count == 0 {
        1
    } else {
        failed_count
    };
    let lower_error = last_error.as_deref().map(str::to_ascii_lowercase);
    let last_provider_error = lower_error
        .as_deref()
        .is_some_and(|err| {
            err.contains("provider") || err.contains("voyage") || err.contains("embed")
        })
        .then(|| last_error.clone())
        .flatten();
    if let Err(err) = record_vector_sweep_state(
        db_path,
        VectorSweepStateUpdate {
            enabled: true,
            disabled_reason: None,
            skip_recall_cache,
            embedded_count,
            failed_count,
            last_error,
            last_provider_error,
            interval_secs: None,
            preserve_schedule: true,
            preserve_outcome: false,
        },
    ) {
        eprintln!("  WARN: vector sweep state write failed: {err}");
    }
}

/// Backfill missing summaries for a given DB.
pub(super) async fn run_backfill_summaries(
    db_path: &PathBuf,
    vault_db_path: &PathBuf,
    dry_run: bool,
    schema_migration: &MigrationAuthority,
) -> Result<(), Box<dyn Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;
    let open_ctx = backfill_write_open_context(schema_migration);

    // Codex review (2026-07-17, checkpoint 4, MERGE-BLOCKING): dry-run must
    // never open migration-capable — the CLI help promises "don't generate
    // ... only show stats". Mirror run_backfill_vectors's pattern: read-only
    // when dry_run, migration-capable only for a real write.
    let store = if dry_run {
        MemoryStore::open_read_only(db_str)?
    } else {
        MemoryStore::open_with_context(db_str, &open_ctx)?
    };
    let total = store.stats(false)?.total;
    let entries = store.entries_missing_summaries()?;
    let missing = entries.len();
    let with_summary = total.saturating_sub(missing as u64);

    println!("DB:        {}", db_path.display());
    println!("Total:     {total}");
    println!("Summaries: {with_summary}");
    println!("Missing:   {missing}");

    if dry_run {
        println!("\n(dry-run mode, opened read-only; no changes made)");
        return Ok(());
    }

    if missing == 0 {
        println!("\n✅ All entries have summaries!");
        return Ok(());
    }

    let llm = LlmClient::new_with_vault_db(Some(vault_db_path))
        .map_err(|e| format!("LLM client init failed: {e}"))?;
    let materialize_report = materialize_standalone(&llm, vault_db_path)
        .map_err(|e| IoError::new(ErrorKind::Other, e))?;
    // #1279: backfill stays strict — batch jobs fail-fast on skipped aliases.
    ensure_no_skipped_aliases(&materialize_report)?;
    let concurrency = backfill_llm_concurrency();

    println!("\nBackfilling {missing} entries (concurrency={concurrency})...\n");

    drop(store);
    let mut store = MemoryStore::open_with_context(db_str, &open_ctx)?;
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
    schema_migration: &MigrationAuthority,
) -> Result<(), Box<dyn Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;
    let open_ctx = backfill_write_open_context(schema_migration);

    // Codex review (2026-07-17, checkpoint 4, MERGE-BLOCKING): dry-run must
    // never open migration-capable — the CLI help promises "don't extract
    // ... only show stats". Mirror run_backfill_vectors's pattern: read-only
    // when dry_run, migration-capable only for a real write.
    let store = if dry_run {
        MemoryStore::open_read_only(db_str)?
    } else {
        MemoryStore::open_with_context(db_str, &open_ctx)?
    };
    let (total, with_metadata) = store.metadata_stats()?;
    let entries = store.entries_missing_metadata()?;
    let missing = entries.len();

    println!("DB:       {}", db_path.display());
    println!("Total:    {total}");
    println!("Metadata: {with_metadata}");
    println!("Missing:  {missing}");

    if dry_run {
        println!("\n(dry-run mode, opened read-only; no changes made)");
        return Ok(());
    }

    if missing == 0 {
        println!("\n✅ All entries have recall keywords!");
        return Ok(());
    }

    let llm = LlmClient::new_with_vault_db(Some(vault_db_path))
        .map_err(|e| format!("LLM client init failed: {e}"))?;
    let materialize_report = materialize_standalone(&llm, vault_db_path)
        .map_err(|e| IoError::new(ErrorKind::Other, e))?;
    // #1279: backfill stays strict — batch jobs fail-fast on skipped aliases.
    ensure_no_skipped_aliases(&materialize_report)?;
    let concurrency = backfill_llm_concurrency();

    println!("\nBackfilling metadata for {missing} entries (concurrency={concurrency})...\n");

    drop(store);
    let mut store = MemoryStore::open_with_context(db_str, &open_ctx)?;
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
    schema_migration: &MigrationAuthority,
) -> Result<(), Box<dyn Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;
    let open_ctx = backfill_write_open_context(schema_migration);

    // Codex review (2026-07-17, checkpoint 4, MERGE-BLOCKING): dry-run must
    // never open migration-capable — the CLI help promises "only show
    // stats, don't modify". Mirror run_backfill_vectors's pattern:
    // read-only when dry_run, migration-capable only for a real write.
    let store = if dry_run {
        MemoryStore::open_read_only(db_str)?
    } else {
        MemoryStore::open_with_context(db_str, &open_ctx)?
    };
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

    if dry_run {
        println!("\n(dry-run mode, opened read-only; no changes made)");
        return Ok(());
    }

    if !full && missing == 0 {
        println!("\n✅ All entries have FTS index!");
        return Ok(());
    }

    drop(store);
    let mut store = MemoryStore::open_with_context(db_str, &open_ctx)?;

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

#[cfg(test)]
mod tests {
    use super::{
        record_cli_vector_sweep_state, run_backfill_fts, run_backfill_metadata,
        run_backfill_summaries, run_backfill_vectors,
    };
    use memcore::{MemoryStore, MigrationAuthority, VectorBackfillScope};
    use rusqlite::params;
    use std::path::Path;

    #[derive(Debug, PartialEq, Eq)]
    struct DryRunDbSnapshot {
        schema_version: i64,
        user_version: i64,
        sqlite_master: Vec<(String, String, String)>,
        hard_state: Vec<(String, String, String, i64, String, String)>,
        sibling_files: Vec<String>,
    }

    fn insert_memory(store: &MemoryStore, id: &str, source: &str, topic: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        store
            .connection()
            .execute(
                "INSERT INTO memories (
                    id, path, summary, text, importance, timestamp, category, topic,
                    keywords, entities, source, scope, archived,
                    created_at, updated_at, access_count, revision, metadata
                 ) VALUES (?1, '/p', '', 'body', 0.5, ?2, 'fact', ?3,
                           '[]', '[]', ?4, 'project', 0,
                           ?2, ?2, 0, 1, '{}')",
                params![id, now, topic, source],
            )
            .expect("insert memory");
    }

    fn dry_run_db_snapshot(db_path: &Path) -> DryRunDbSnapshot {
        let conn = rusqlite::Connection::open(db_path).expect("open sqlite snapshot");
        let schema_version = conn
            .query_row("PRAGMA schema_version", [], |row| row.get(0))
            .expect("schema_version");
        let user_version = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version");
        let sqlite_master = query_string_triples(
            &conn,
            "SELECT type, name, COALESCE(sql, '')
             FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        );
        let hard_state = conn
            .prepare(
                "SELECT namespace, key, value_json, version, created_at, updated_at
                 FROM hard_state
                 ORDER BY namespace, key",
            )
            .expect("prepare hard_state snapshot")
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .expect("query hard_state snapshot")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect hard_state snapshot");
        let db_file_name = db_path
            .file_name()
            .expect("db file name")
            .to_string_lossy()
            .to_string();
        let mut sibling_files = std::fs::read_dir(db_path.parent().expect("db parent"))
            .expect("read db parent")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .filter(|name| name == &db_file_name || name.starts_with(&format!("{db_file_name}.")))
            .collect::<Vec<_>>();
        sibling_files.sort();

        DryRunDbSnapshot {
            schema_version,
            user_version,
            sqlite_master,
            hard_state,
            sibling_files,
        }
    }

    fn query_string_triples(
        conn: &rusqlite::Connection,
        sql: &str,
    ) -> Vec<(String, String, String)> {
        conn.prepare(sql)
            .expect("prepare snapshot query")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query snapshot")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect snapshot")
    }

    /// G3 (cross-face counting): status and vector backfill must exclude the
    /// same anchor plumbing rows from their missing-vector population.
    #[test]
    fn g3_vector_health_matches_backfill_anchor_membership() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("g3.db");
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");

        // Durable content without a vector is the sole pending row.
        insert_memory(&store, "durable-1", "manual", "note");
        // Anchors are graph plumbing, not vector-backfill work.
        insert_memory(&store, "anchor:x", "manual", "note");

        let status_missing = crate::status_ops::vector_health(store.connection())
            .expect("vector health")
            .missing;
        let backfill_pending = store
            .vector_backfill_counts(VectorBackfillScope::default())
            .expect("vector backfill counts")
            .pending;

        assert_eq!(
            status_missing, backfill_pending,
            "status and vector backfill must share the exact membership set"
        );
        assert_eq!(status_missing, 1, "only the durable row is missing a vector");
    }

    #[tokio::test]
    async fn dry_run_vector_backfill_does_not_write_sweep_state() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("dry-run.db");
        let vault_path = dir.path().join("vault.db");
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        insert_memory(&store, "durable-1", "manual", "note");
        drop(store);
        let migration_marker = dir.path().join("dry-run.db.migration-marker");
        std::fs::remove_file(&migration_marker).expect("remove migration marker");
        let before = dry_run_db_snapshot(&db_path);

        run_backfill_vectors(
            &db_path,
            &vault_path,
            16,
            true,
            false,
            &MigrationAuthority::Deny,
        )
        .await
        .expect("dry-run vector backfill");

        let after = dry_run_db_snapshot(&db_path);
        assert!(
            !after
                .sqlite_master
                .iter()
                .any(|(_, name, _)| name == "vector_sweep_state"),
            "dry-run must not create vector_sweep_state"
        );
        assert_eq!(after.schema_version, before.schema_version);
        assert_eq!(after.user_version, before.user_version);
        assert_eq!(after.sqlite_master, before.sqlite_master);
        assert_eq!(after.hard_state, before.hard_state);
        assert_eq!(after.sibling_files, before.sibling_files);
    }

    #[test]
    fn cli_sweep_state_does_not_count_benign_skips_as_failures() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("cli-benign-skip-state.db");
        MemoryStore::open(db_path.to_str().unwrap()).expect("open store");

        record_cli_vector_sweep_state(&db_path, true, 3, 0, None);

        let state = crate::vector_backfill::read_vector_sweep_state_for_status(&db_path)
            .expect("read state")
            .expect("state recorded");
        assert_eq!(state.embedded_count, 3);
        assert_eq!(
            state.failed_count, 0,
            "benign skipped CLI rows remain pending, not failed"
        );
        assert_eq!(state.last_error, None);
        assert_eq!(state.last_provider_error, None);
    }

    #[test]
    fn cli_sweep_state_counts_real_row_failures_without_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("cli-row-failure-state.db");
        MemoryStore::open(db_path.to_str().unwrap()).expect("open store");

        record_cli_vector_sweep_state(&db_path, true, 3, 1, None);

        let state = crate::vector_backfill::read_vector_sweep_state_for_status(&db_path)
            .expect("read state")
            .expect("state recorded");
        assert_eq!(state.embedded_count, 3);
        assert_eq!(
            state.failed_count, 1,
            "real CLI row failures are recorded even when the sweep completes"
        );
        assert_eq!(state.last_error, None);
        assert_eq!(state.last_provider_error, None);
    }

    #[tokio::test]
    async fn dry_run_all_vectored_db_does_not_update_existing_sweep_state() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("dry-run-complete.db");
        let vault_path = dir.path().join("vault.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        insert_memory(&store, "durable-1", "manual", "note");
        let dummy_vec = vec![0.0_f32; 1024];
        store
            .update_enrichment_fields("durable-1", None, Some(&dummy_vec), None, None, 1)
            .expect("write vector");
        drop(store);

        crate::vector_backfill::record_vector_sweep_state(
            &db_path,
            crate::vector_backfill::VectorSweepStateUpdate {
                enabled: true,
                disabled_reason: None,
                skip_recall_cache: true,
                embedded_count: 7,
                failed_count: 3,
                last_error: Some("previous provider error".to_string()),
                last_provider_error: Some("previous provider error".to_string()),
                interval_secs: Some(1800),
                preserve_schedule: false,
                preserve_outcome: false,
            },
        )
        .expect("seed existing state");

        run_backfill_vectors(
            &db_path,
            &vault_path,
            16,
            true,
            false,
            &MigrationAuthority::Deny,
        )
        .await
        .expect("dry-run vector backfill");

        let state = crate::vector_backfill::read_vector_sweep_state_for_status(&db_path)
            .expect("read state")
            .expect("existing state remains");
        assert_eq!(state.embedded_count, 7);
        assert_eq!(state.failed_count, 3);
        assert_eq!(state.last_error.as_deref(), Some("previous provider error"));
    }

    #[tokio::test]
    async fn cli_vector_backfill_preserves_daemon_schedule_metadata() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("cli-schedule.db");
        let vault_path = dir.path().join("vault.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        insert_memory(&store, "durable-1", "manual", "note");
        let dummy_vec = vec![0.0_f32; 1024];
        store
            .update_enrichment_fields("durable-1", None, Some(&dummy_vec), None, None, 1)
            .expect("write vector");
        drop(store);

        crate::vector_backfill::record_vector_sweep_state(
            &db_path,
            crate::vector_backfill::VectorSweepStateUpdate {
                enabled: true,
                disabled_reason: None,
                skip_recall_cache: true,
                embedded_count: 0,
                failed_count: 0,
                last_error: None,
                last_provider_error: None,
                interval_secs: Some(1800),
                preserve_schedule: false,
                preserve_outcome: false,
            },
        )
        .expect("seed daemon schedule");

        run_backfill_vectors(
            &db_path,
            &vault_path,
            16,
            false,
            false,
            &MigrationAuthority::Deny,
        )
        .await
        .expect("cli vector backfill");

        let state = crate::vector_backfill::read_vector_sweep_state_for_status(&db_path)
            .expect("read state")
            .expect("state remains");
        assert_eq!(
            state.interval_secs,
            Some(1800),
            "manual CLI backfill must not clear daemon interval_secs"
        );
        assert!(
            state.next_run_after.is_some(),
            "manual CLI backfill must not clear daemon next_run_after"
        );
    }

    // --- #1181: --allow-schema-migration must reach every backfill-*
    // in-process open -----------------------------------------------------
    //
    // Mirrors `bootstrap::serve::tests::
    // remember_cli_requires_flag_to_migrate_stamped_older_db_in_process`
    // (the #1131/#1138 pattern already established for the `remember`
    // fallback): seed a current-schema DB, roll `PRAGMA user_version` back
    // to simulate "this is really a stamped-older DB", then prove Deny
    // refuses (typed `SchemaMigrationOptInRequired`, stamp untouched) and
    // Allow migrates (re-stamped at EXPECTED_SCHEMA_VERSION). Every DB here
    // starts with zero rows, so each function's "nothing to do" early return
    // (which requires the gated open to have already succeeded) fires before
    // any LLM client is constructed — no network dependency.

    fn seed_and_stamp_older_schema_version(db_path: &std::path::Path) {
        MemoryStore::open(db_path.to_str().expect("utf8 db path")).expect("seed current-schema db");
        let conn = rusqlite::Connection::open(db_path).expect("reopen to roll back stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp older schema version");
    }

    fn read_user_version(db_path: &std::path::Path) -> u32 {
        let conn = rusqlite::Connection::open(db_path).expect("open for version read");
        memcore::db::migrations::read_schema_version(&conn).expect("read schema version")
    }

    #[tokio::test]
    async fn backfill_fts_requires_flag_to_migrate_stamped_older_db_in_process() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("backfill-fts-schema.db");
        seed_and_stamp_older_schema_version(&db_path);

        let err = run_backfill_fts(&db_path, false, false, &MigrationAuthority::Deny)
            .await
            .expect_err("backfill-fts without the flag must preserve OpenExisting + Deny");
        assert!(
            err.to_string().contains("refusing to migrate db schema"),
            "unexpected deny error: {err}"
        );
        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "deny must not mutate the old schema stamp"
        );

        run_backfill_fts(
            &db_path,
            false,
            false,
            &MigrationAuthority::Allow {
                approved_by: "test:1181-backfill-fts".to_string(),
            },
        )
        .await
        .expect("backfill-fts with the flag must migrate the isolated DB");
        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
            "allow must re-stamp the DB at the current schema version"
        );
    }

    /// Codex review (2026-07-17, checkpoint 6): every discrimination test
    /// above seeds a zero-row DB, so `run_backfill_*`'s "nothing to do"
    /// early return fires before `drop(store); MemoryStore::open_with_context`
    /// (the write-pass reopen) is ever reached — a future refactor that
    /// reverted just that second call site to a bare `MemoryStore::open`
    /// would escape all of them. `backfill-fts` is the one backfill command
    /// whose write pass needs no LLM client (`backfill_fts_missing` is pure
    /// SQL), so it is the one we can drive through the reopen without a
    /// network dependency. Seeds real FTS-missing rows on a stamped-older DB
    /// and proves the reopen actually completes the backfill under `Allow`.
    #[tokio::test]
    async fn backfill_fts_write_pass_reopen_runs_under_allow_on_stamped_older_db() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("backfill-fts-write-pass.db");
        {
            let store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
                .expect("seed current-schema db");
            insert_memory(&store, "fts-missing-1", "manual", "note");
            insert_memory(&store, "fts-missing-2", "manual", "note");
        }
        let conn = rusqlite::Connection::open(&db_path).expect("reopen to roll back stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp older schema version");
        drop(conn);

        run_backfill_fts(
            &db_path,
            false,
            false,
            &MigrationAuthority::Allow {
                approved_by: "test:1181-backfill-fts-write-pass".to_string(),
            },
        )
        .await
        .expect("backfill-fts with the flag must migrate and backfill the isolated DB");

        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
            "the first (stats-pass) open must have migrated the DB"
        );

        let verify_store =
            MemoryStore::open(db_path.to_str().expect("utf8 db path")).expect("reopen to verify");
        let (total, with_fts) = verify_store.fts_stats().expect("fts_stats");
        assert_eq!(total, 2, "both seeded rows must be present");
        assert_eq!(
            with_fts, 2,
            "the write-pass reopen must have actually run backfill_fts_missing \
             (a bare MemoryStore::open at that call site would still succeed here, \
             since the DB is already migrated by the first open — but a wrong \
             db_str/path regression at the reopen would leave this at 0)"
        );
    }

    #[tokio::test]
    async fn backfill_vectors_requires_flag_to_migrate_stamped_older_db_in_process() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("backfill-vectors-schema.db");
        let vault_path = dir.path().join("vault.db");
        seed_and_stamp_older_schema_version(&db_path);

        let err = run_backfill_vectors(
            &db_path,
            &vault_path,
            16,
            false,
            false,
            &MigrationAuthority::Deny,
        )
        .await
        .expect_err("backfill-vectors without the flag must preserve OpenExisting + Deny");
        assert!(
            err.to_string().contains("refusing to migrate db schema"),
            "unexpected deny error: {err}"
        );
        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "deny must not mutate the old schema stamp"
        );

        run_backfill_vectors(
            &db_path,
            &vault_path,
            16,
            false,
            false,
            &MigrationAuthority::Allow {
                approved_by: "test:1181-backfill-vectors".to_string(),
            },
        )
        .await
        .expect("backfill-vectors with the flag must migrate the isolated DB");
        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
            "allow must re-stamp the DB at the current schema version"
        );
    }

    #[tokio::test]
    async fn backfill_summaries_requires_flag_to_migrate_stamped_older_db_in_process() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("backfill-summaries-schema.db");
        let vault_path = dir.path().join("vault.db");
        seed_and_stamp_older_schema_version(&db_path);

        let err = run_backfill_summaries(&db_path, &vault_path, false, &MigrationAuthority::Deny)
            .await
            .expect_err("backfill-summaries without the flag must preserve OpenExisting + Deny");
        assert!(
            err.to_string().contains("refusing to migrate db schema"),
            "unexpected deny error: {err}"
        );
        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "deny must not mutate the old schema stamp"
        );

        run_backfill_summaries(
            &db_path,
            &vault_path,
            false,
            &MigrationAuthority::Allow {
                approved_by: "test:1181-backfill-summaries".to_string(),
            },
        )
        .await
        .expect("backfill-summaries with the flag must migrate the isolated DB");
        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
            "allow must re-stamp the DB at the current schema version"
        );
    }

    #[tokio::test]
    async fn backfill_metadata_requires_flag_to_migrate_stamped_older_db_in_process() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("backfill-metadata-schema.db");
        let vault_path = dir.path().join("vault.db");
        seed_and_stamp_older_schema_version(&db_path);

        let err = run_backfill_metadata(&db_path, &vault_path, false, &MigrationAuthority::Deny)
            .await
            .expect_err("backfill-metadata without the flag must preserve OpenExisting + Deny");
        assert!(
            err.to_string().contains("refusing to migrate db schema"),
            "unexpected deny error: {err}"
        );
        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "deny must not mutate the old schema stamp"
        );

        run_backfill_metadata(
            &db_path,
            &vault_path,
            false,
            &MigrationAuthority::Allow {
                approved_by: "test:1181-backfill-metadata".to_string(),
            },
        )
        .await
        .expect("backfill-metadata with the flag must migrate the isolated DB");
        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
            "allow must re-stamp the DB at the current schema version"
        );
    }

    // --- #1181 checkpoint 4 (codex review, 2026-07-17, MERGE-BLOCKING) -----
    //
    // Pre-fix, `run_backfill_summaries` / `_metadata` / `_fts` opened
    // migration-capable (`MemoryStore::open_with_context`) BEFORE checking
    // `dry_run`, so a `--dry-run` run against a stamped-older DB either (a)
    // silently migrated it under `Allow`, contradicting the CLI help's
    // "don't generate / don't extract / only show stats" promise, or (b)
    // under `Deny` (the common case — nobody passes `--allow-schema-migration`
    // to a dry-run), incorrectly REFUSED entirely instead of just reporting
    // read-only stats. Both are RED on pre-fix code. Post-fix, dry-run always
    // opens `MemoryStore::open_read_only`, which never runs
    // `check_db_open_context_gate` (memcore/src/store/open.rs) — so dry-run
    // succeeds without the flag and never mutates the stamp.

    #[tokio::test]
    async fn dry_run_summaries_backfill_opens_read_only_on_stamped_older_db() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("dry-run-summaries-schema.db");
        let vault_path = dir.path().join("vault.db");
        seed_and_stamp_older_schema_version(&db_path);

        run_backfill_summaries(&db_path, &vault_path, true, &MigrationAuthority::Deny)
            .await
            .expect(
                "dry-run must succeed read-only even without --allow-schema-migration \
                 (pre-fix: this errored with the migration refusal)",
            );

        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "dry-run must never mutate the schema stamp, even under Allow"
        );
    }

    #[tokio::test]
    async fn dry_run_metadata_backfill_opens_read_only_on_stamped_older_db() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("dry-run-metadata-schema.db");
        let vault_path = dir.path().join("vault.db");
        seed_and_stamp_older_schema_version(&db_path);

        run_backfill_metadata(&db_path, &vault_path, true, &MigrationAuthority::Deny)
            .await
            .expect(
                "dry-run must succeed read-only even without --allow-schema-migration \
                 (pre-fix: this errored with the migration refusal)",
            );

        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "dry-run must never mutate the schema stamp, even under Allow"
        );
    }

    #[tokio::test]
    async fn dry_run_fts_backfill_opens_read_only_on_stamped_older_db() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("dry-run-fts-schema.db");
        seed_and_stamp_older_schema_version(&db_path);

        run_backfill_fts(&db_path, false, true, &MigrationAuthority::Deny)
            .await
            .expect(
                "dry-run must succeed read-only even without --allow-schema-migration \
                 (pre-fix: this errored with the migration refusal)",
            );

        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "dry-run must never mutate the schema stamp, even under Allow"
        );
    }
}

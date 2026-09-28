use crate::provider_config::materialize_standalone;
use crate::vector_backfill::{
    embed_and_write_batch, list_missing_vector_entries, record_vector_sweep_state,
    VectorSweepStateUpdate,
};
use futures::{stream, StreamExt};
use memcore::store::enrichment::EnrichmentInvocationReceipts;
use memcore::store::open::ReadOnlyBackfillOperation;
use memcore::{
    DbOpenContext, MemoryStore, MigrationAuthority, OpenIntent, ProfileRequirement, StoreProfile,
    VectorBackfillScope,
};
use std::collections::{BTreeSet, HashMap};
use std::error::Error;
use std::fmt::Display;
use std::io::{Error as IoError, ErrorKind};
use std::path::{Path, PathBuf};
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
        // #1585 D2: `backfill-*` is a tachi-server operator command; it opens
        // the same product databases `serve` does.
        required_profile: ProfileRequirement::AtLeast(StoreProfile::TachiFull),
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
        MemoryStore::open_read_only_existing_schema_compat(
            db_str,
            ReadOnlyBackfillOperation::Vectors,
        )?
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

/// Operator-selectable scope for `tachi backfill-summaries`.
#[derive(Debug, Default, Clone)]
pub(super) struct SummaryBackfillOptions {
    /// Explicit, repeatable `--id` set (deduplicated deterministically).
    pub ids: Vec<String>,
    /// `--limit N`: schedule at most N selected entries.
    pub limit: Option<usize>,
    /// `--regenerate`: replace existing non-empty summaries for explicit ids.
    pub regenerate: bool,
    /// `--json`: structured plan/result document.
    pub json: bool,
}

/// Unicode-scalar cap for summary LLM inputs. Longer inputs are skipped with
/// reason `input_too_long` instead of silently summarizing a truncated tail
/// (the old `chars().take(8000)` behavior produced partial summaries).
const SUMMARY_INPUT_MAX_CHARS: usize = 8000;

/// Per-document cap for per-item plan/result reporting. Counts stay exact;
/// only the item listing is capped so stdout stays bounded.
const MAX_SUMMARY_REPORT_ITEMS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummarySkipReason {
    Archived,
    AlreadySummarized,
    InputTooLong,
}

impl SummarySkipReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Archived => "archived",
            Self::AlreadySummarized => "already_summarized",
            Self::InputTooLong => "input_too_long",
        }
    }

    fn all() -> [Self; 3] {
        [Self::Archived, Self::AlreadySummarized, Self::InputTooLong]
    }
}

/// Terminal apply outcome for one eligible row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryItemOutcome {
    Summarized,
    EmptyOutput,
    RevisionMismatch,
    FailedSummary,
    FailedDbUpdate,
}

impl SummaryItemOutcome {
    fn status_str(self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::EmptyOutput | Self::RevisionMismatch => "skipped",
            Self::FailedSummary | Self::FailedDbUpdate => "failed",
        }
    }

    fn reason_str(self) -> Option<&'static str> {
        match self {
            Self::Summarized => None,
            Self::EmptyOutput => Some("empty_output"),
            Self::RevisionMismatch => Some("revision_mismatch"),
            Self::FailedSummary => Some("summary"),
            Self::FailedDbUpdate => Some("db_update"),
        }
    }
}

#[derive(Debug, Clone)]
struct PlannedSummaryRow {
    id: String,
    revision: i64,
    /// `None` for skipped rows: a body never travels past the planner.
    text: Option<String>,
    skip: Option<SummarySkipReason>,
}

/// Read-only classification of the whole sweep before any provider call.
#[derive(Debug, Clone)]
struct SummaryBackfillPlan {
    mode: &'static str,
    regenerate: bool,
    limit: Option<usize>,
    /// Pre-limit deduplicated `--id` count (explicit mode only).
    requested_after_dedup: Option<usize>,
    total: u64,
    with_summary: u64,
    missing: usize,
    rows: Vec<PlannedSummaryRow>,
}

impl SummaryBackfillPlan {
    fn counts(&self) -> (usize, usize, usize) {
        let selected = self.rows.len();
        let eligible = self.rows.iter().filter(|row| row.skip.is_none()).count();
        (selected, eligible, selected - eligible)
    }
}

#[derive(Debug, Clone)]
struct SummaryApplyReport {
    attempted: usize,
    processed: usize,
    failed: usize,
    /// Sorted by id so reporting is deterministic despite completion order.
    outcomes: Vec<(String, SummaryItemOutcome)>,
}

/// Reject malformed operator input before any DB open or read.
fn validate_summary_backfill_options(
    options: &SummaryBackfillOptions,
) -> Result<(), Box<dyn Error>> {
    if options.limit == Some(0) {
        return Err("backfill-summaries: --limit must be a positive integer".into());
    }
    if options.regenerate && options.ids.is_empty() {
        return Err(
            "backfill-summaries: --regenerate requires an explicit, nonempty --id set \
             (the default sweep stays missing-only and never overwrites)"
                .into(),
        );
    }
    Ok(())
}

fn summary_input_skip_reason(text: &str) -> Option<SummarySkipReason> {
    (text.chars().count() > SUMMARY_INPUT_MAX_CHARS).then_some(SummarySkipReason::InputTooLong)
}

/// Build the whole plan read-only. Explicit-id mode validates the ENTIRE
/// deduplicated id set against this one DB before `--limit` truncates
/// anything, so a typo'd id fails the run instead of being silently ignored.
fn plan_summary_backfill(
    store: &MemoryStore,
    options: &SummaryBackfillOptions,
) -> Result<SummaryBackfillPlan, Box<dyn Error>> {
    // Include archived rows: the missing-summary population below does not
    // filter them either (default-sweep compat), so Total/Summaries/Missing
    // counts stay on one basis instead of claiming missing rows that a
    // total excluding them cannot cover.
    let total = store.stats(true)?.total;
    let missing_rows = store.entries_missing_summaries()?;
    let missing = missing_rows.len();
    let with_summary = total.saturating_sub(missing as u64);
    let deduped: Vec<String> = options
        .ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let requested_after_dedup = (!deduped.is_empty()).then_some(deduped.len());

    let rows = if deduped.is_empty() {
        // Default sweep stays exactly the historical missing-only population
        // (rowid order, archived rows included — no age-based cleanup).
        missing_rows
            .into_iter()
            .map(|(id, text, revision)| {
                let skip = summary_input_skip_reason(&text);
                PlannedSummaryRow {
                    text: skip.is_none().then_some(text),
                    id,
                    revision,
                    skip,
                }
            })
            .collect::<Vec<_>>()
    } else {
        let found = store.summary_backfill_rows_for_ids(&deduped)?;
        if found.len() != deduped.len() {
            let known: BTreeSet<&str> = found.iter().map(|row| row.id.as_str()).collect();
            let unknown: Vec<&str> = deduped
                .iter()
                .map(String::as_str)
                .filter(|id| !known.contains(id))
                .collect();
            let shown: Vec<&str> = unknown.iter().take(10).copied().collect();
            let mut message = format!(
                "backfill-summaries: {} requested --id value(s) not found in this database \
                 (no cross-DB search; nothing was generated or written): {}",
                unknown.len(),
                shown.join(", ")
            );
            if unknown.len() > shown.len() {
                message.push_str(&format!(", … and {} more", unknown.len() - shown.len()));
            }
            return Err(message.into());
        }
        found
            .into_iter()
            .map(|row| {
                let skip = if row.archived {
                    // Archival status is reported, never bypassed.
                    Some(SummarySkipReason::Archived)
                } else if row.has_summary && !options.regenerate {
                    Some(SummarySkipReason::AlreadySummarized)
                } else {
                    summary_input_skip_reason(&row.text)
                };
                PlannedSummaryRow {
                    text: skip.is_none().then_some(row.text),
                    id: row.id,
                    revision: row.revision,
                    skip,
                }
            })
            .collect::<Vec<_>>()
    };

    let mut plan = SummaryBackfillPlan {
        mode: if options.ids.is_empty() {
            "missing-only"
        } else {
            "explicit-ids"
        },
        regenerate: options.regenerate,
        limit: options.limit,
        requested_after_dedup,
        total,
        with_summary,
        missing,
        rows,
    };
    if let Some(limit) = plan.limit {
        plan.rows.truncate(limit);
    }
    Ok(plan)
}

/// Apply phase core. The `generate` seam returns the accepted summary text
/// plus its serialized invocation receipt; production wires
/// `generate_summary_with_retry` + receipt serialization into it, tests
/// inject a deterministic closure so provider-called counts are provable.
///
/// Row writes go through the existing revision-checked
/// `update_enrichment_fields_with_receipts` path, which keeps the summary,
/// its receipt, and the FTS refresh in one SQLite transaction and never
/// touches raw text, observation timestamps, identity, or vectors.
async fn run_summary_backfill_apply<F, Fut>(
    store: &mut MemoryStore,
    plan: &SummaryBackfillPlan,
    concurrency: usize,
    generate: F,
    emit_progress: bool,
) -> SummaryApplyReport
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<(String, serde_json::Value), String>>,
{
    let eligible: Vec<(String, String, i64)> = plan
        .rows
        .iter()
        .filter(|row| row.skip.is_none())
        .map(|row| {
            (
                row.id.clone(),
                row.text
                    .clone()
                    .expect("eligible planned rows carry their full text"),
                row.revision,
            )
        })
        .collect();
    let total_eligible = eligible.len();

    let tasks = stream::iter(eligible.into_iter().map(|(id, text, revision)| {
        let generate = &generate;
        async move {
            let result = generate(text).await;
            (id, revision, result)
        }
    }))
    .buffer_unordered(concurrency.max(1));
    tokio::pin!(tasks);

    let mut attempted = 0usize;
    let mut processed = 0usize;
    let mut failed = 0usize;
    let mut outcomes = Vec::new();

    while let Some((id, revision, result)) = tasks.next().await {
        attempted += 1;
        let outcome = match result {
            Ok((summary, receipt)) => {
                // Same seam as the store: scrub think-tags before judging
                // emptiness, so a think-only reply is an honest empty_output
                // skip instead of a phantom "processed" row (the store would
                // scrub it to None and no-op).
                let summary = memcore::noise::scrub_think_tags(&summary);
                if summary.trim().is_empty() {
                    eprintln!("  WARN: summary produced no accepted field for {id}, skipped");
                    SummaryItemOutcome::EmptyOutput
                } else {
                    match store.update_enrichment_fields_with_receipts(
                        &id,
                        Some(&summary),
                        None,
                        None,
                        None,
                        revision,
                        EnrichmentInvocationReceipts {
                            summary: Some(&receipt),
                            ..Default::default()
                        },
                    ) {
                        Ok(true) => {
                            processed += 1;
                            if emit_progress {
                                println!("  [{attempted}/{total_eligible}] ✓ {id}");
                            }
                            SummaryItemOutcome::Summarized
                        }
                        Ok(false) => {
                            eprintln!("  WARN: revision mismatch for {id}, skipped");
                            SummaryItemOutcome::RevisionMismatch
                        }
                        Err(error) => {
                            eprintln!("  WARN: DB write failed for {id}: {error}");
                            if record_backfill_failure_if_revision(
                                store,
                                &id,
                                "db_update",
                                &error,
                                revision,
                            ) {
                                failed += 1;
                                SummaryItemOutcome::FailedDbUpdate
                            } else {
                                eprintln!(
                                    "  WARN: revision mismatch for {id} while recording failure, skipped"
                                );
                                SummaryItemOutcome::RevisionMismatch
                            }
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("  WARN: summary failed for {id}: {error}");
                if record_backfill_failure_if_revision(store, &id, "summary", &error, revision) {
                    failed += 1;
                    SummaryItemOutcome::FailedSummary
                } else {
                    eprintln!(
                        "  WARN: revision mismatch for {id} while recording failure, skipped"
                    );
                    SummaryItemOutcome::RevisionMismatch
                }
            }
        };
        outcomes.push((id, outcome));
    }

    outcomes.sort_by(|a, b| a.0.cmp(&b.0));
    SummaryApplyReport {
        attempted,
        processed,
        failed,
        outcomes,
    }
}

/// Failed items must not be papered over by a zero exit status.
fn summary_backfill_exit_error(report: &SummaryApplyReport) -> Option<String> {
    (report.failed > 0).then(|| {
        format!(
            "backfill-summaries: {} of {} attempted item(s) failed ({} succeeded); \
             per-item statuses were reported above",
            report.failed, report.attempted, report.processed
        )
    })
}

/// One structured plan (dry-run) / result (apply) document. Carries ids,
/// revisions, statuses, and counts only — never memory bodies, summaries, or
/// key material. `dry_run` is explicit: an apply run with nothing eligible
/// also passes `report: None` and must still report `dry_run: false`.
fn summary_backfill_json_document(
    db_path: &Path,
    plan: &SummaryBackfillPlan,
    report: Option<&SummaryApplyReport>,
    final_with_summary: u64,
    dry_run: bool,
) -> serde_json::Value {
    use serde_json::json;

    let (selected, eligible, _plan_skipped) = plan.counts();
    let outcome_by_id: HashMap<&str, SummaryItemOutcome> = report
        .map(|report| {
            report
                .outcomes
                .iter()
                .map(|(id, outcome)| (id.as_str(), *outcome))
                .collect()
        })
        .unwrap_or_default();

    let mut skip_reasons = serde_json::Map::new();
    for reason in SummarySkipReason::all() {
        let count = plan
            .rows
            .iter()
            .filter(|row| row.skip == Some(reason))
            .count();
        if count > 0 {
            skip_reasons.insert(reason.as_str().to_string(), json!(count));
        }
    }
    if let Some(report) = report {
        for outcome in [
            SummaryItemOutcome::EmptyOutput,
            SummaryItemOutcome::RevisionMismatch,
        ] {
            let count = report
                .outcomes
                .iter()
                .filter(|(_, item)| *item == outcome)
                .count();
            if count > 0 {
                skip_reasons.insert(
                    outcome
                        .reason_str()
                        .expect("skip-class outcome has a reason")
                        .to_string(),
                    json!(count),
                );
            }
        }
    }

    let mut items_truncated = false;
    let mut items = Vec::new();
    for row in &plan.rows {
        if items.len() >= MAX_SUMMARY_REPORT_ITEMS {
            items_truncated = true;
            break;
        }
        let (status, reason) = match row.skip {
            Some(skip) => ("skipped", Some(skip.as_str())),
            None if dry_run => ("eligible", None),
            None => match outcome_by_id.get(row.id.as_str()) {
                Some(outcome) => (outcome.status_str(), outcome.reason_str()),
                None => ("not_attempted", None),
            },
        };
        let mut item = serde_json::Map::new();
        item.insert("id".to_string(), json!(row.id));
        item.insert("revision".to_string(), json!(row.revision));
        item.insert("status".to_string(), json!(status));
        if let Some(reason) = reason {
            item.insert("reason".to_string(), json!(reason));
        }
        items.push(serde_json::Value::Object(item));
    }

    // counts.skipped aggregates plan-time AND apply-time skips, so the
    // identity selected == processed + failed + skipped always holds for a
    // completed run (attempted == eligible == processed + failed + apply
    // skips).
    let apply_skipped = report
        .map(|report| {
            report
                .outcomes
                .iter()
                .filter(|(_, outcome)| {
                    matches!(
                        outcome,
                        SummaryItemOutcome::EmptyOutput | SummaryItemOutcome::RevisionMismatch
                    )
                })
                .count()
        })
        .unwrap_or(0);
    let mut counts = serde_json::Map::new();
    counts.insert("total".to_string(), json!(plan.total));
    counts.insert("with_summary".to_string(), json!(plan.with_summary));
    counts.insert("missing".to_string(), json!(plan.missing));
    if let Some(requested) = plan.requested_after_dedup {
        // Pre-limit deduplicated request size — never the post-limit
        // selection, so a truncated run does not hide how many ids were
        // actually asked for.
        counts.insert("requested_after_dedup".to_string(), json!(requested));
    }
    counts.insert("selected".to_string(), json!(selected));
    counts.insert("eligible".to_string(), json!(eligible));
    counts.insert("skipped".to_string(), json!(_plan_skipped + apply_skipped));
    counts.insert(
        "attempted".to_string(),
        json!(report.map(|report| report.attempted).unwrap_or(0)),
    );
    counts.insert(
        "processed".to_string(),
        json!(report.map(|report| report.processed).unwrap_or(0)),
    );
    counts.insert(
        "failed".to_string(),
        json!(report.map(|report| report.failed).unwrap_or(0)),
    );
    counts.insert("final_with_summary".to_string(), json!(final_with_summary));

    json!({
        "command": "backfill-summaries",
        "db": db_path.display().to_string(),
        "dry_run": dry_run,
        "mode": plan.mode,
        "regenerate": plan.regenerate,
        "limit": plan.limit,
        "counts": serde_json::Value::Object(counts),
        "skip_reasons": serde_json::Value::Object(skip_reasons),
        "items": items,
        "items_truncated": items_truncated,
    })
}

/// Human-mode plan listing: ids, revisions, and fixed skip reasons only,
/// capped so stdout stays bounded.
fn print_summary_plan_lines(plan: &SummaryBackfillPlan) {
    let (selected, eligible, skipped) = plan.counts();
    println!("\nPlan: {selected} selected, {eligible} eligible, {skipped} skipped");
    let mut printed = 0usize;
    for row in &plan.rows {
        if printed >= MAX_SUMMARY_REPORT_ITEMS {
            break;
        }
        match row.skip {
            Some(reason) => println!(
                "  SKIP {} (revision {}): {}",
                row.id,
                row.revision,
                reason.as_str()
            ),
            None => println!("  OK   {} (revision {}): eligible", row.id, row.revision),
        }
        printed += 1;
    }
    if selected > printed {
        println!(
            "  … and {} more selected rows not listed (counts above remain exact)",
            selected - printed
        );
    }
}

/// Fixed-order, nonzero-only skip-reason counts across plan-time and
/// apply-time skips, for human terminal lines (empty output and stale
/// revisions included).
fn format_skip_reason_counts(
    plan: &SummaryBackfillPlan,
    report: Option<&SummaryApplyReport>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for reason in SummarySkipReason::all() {
        let count = plan
            .rows
            .iter()
            .filter(|row| row.skip == Some(reason))
            .count();
        if count > 0 {
            parts.push(format!("{}: {count}", reason.as_str()));
        }
    }
    if let Some(report) = report {
        for outcome in [
            SummaryItemOutcome::EmptyOutput,
            SummaryItemOutcome::RevisionMismatch,
        ] {
            let count = report
                .outcomes
                .iter()
                .filter(|(_, item)| *item == outcome)
                .count();
            if count > 0 {
                parts.push(format!(
                    "{}: {count}",
                    outcome
                        .reason_str()
                        .expect("skip-class outcome carries a reason")
                ));
            }
        }
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    }
}

/// Human-mode header block (shape preserved from the original command).
fn print_summary_backfill_header(db_path: &Path, plan: &SummaryBackfillPlan) {
    println!("DB:        {}", db_path.display());
    println!("Total:     {}", plan.total);
    println!("Summaries: {}", plan.with_summary);
    println!("Missing:   {}", plan.missing);
    if plan.mode == "explicit-ids" {
        println!("Mode:      explicit-ids");
    }
    if let Some(limit) = plan.limit {
        println!("Limit:     {limit}");
    }
    if plan.regenerate {
        println!("Regenerate: true");
    }
}

/// Backfill missing summaries for a given DB.
pub(super) async fn run_backfill_summaries(
    db_path: &PathBuf,
    vault_db_path: &PathBuf,
    dry_run: bool,
    schema_migration: &MigrationAuthority,
    options: &SummaryBackfillOptions,
) -> Result<(), Box<dyn Error>> {
    validate_summary_backfill_options(options)?;
    let db_str = db_path.to_str().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;
    let open_ctx = backfill_write_open_context(schema_migration);

    // Plan phase — read-only for EVERY mode (dry-run and apply): the
    // read-only compat open never creates the file, never migrates, and
    // never renames a legacy DB, so flag errors and unknown requested ids
    // fail before anything can mutate the target. The complete `--id` set
    // is validated inside `plan_summary_backfill` before `--limit` applies.
    // (Codex review 2026-07-17 checkpoint 4 for the dry-run half of this
    // invariant; Sol review 2026-09-24 extended it to the apply path.)
    let store = MemoryStore::open_read_only_existing_schema_compat(
        db_str,
        ReadOnlyBackfillOperation::Summaries,
    )?;

    let plan = plan_summary_backfill(&store, options)?;
    let (selected, eligible, plan_skipped) = plan.counts();

    if dry_run {
        // Dry-run stops here: no LLM client, no vault materialization, no
        // write-capable reopen.
        if options.json {
            let doc = summary_backfill_json_document(db_path, &plan, None, plan.with_summary, true);
            super::print_pretty_json(&doc)?;
        } else {
            print_summary_backfill_header(db_path, &plan);
            print_summary_plan_lines(&plan);
            println!("\n(dry-run mode, opened read-only; no changes made)");
        }
        return Ok(());
    }

    if !options.json {
        print_summary_backfill_header(db_path, &plan);
    }

    // Apply phase — reopen honoring the operator's explicit authority. This
    // reopen is the only migration point: `Allow` may migrate even when
    // nothing is eligible (preserving the #1181 rehearsal contract: verify a
    // migration on a disposable copy with the flag), and `Deny` refuses a
    // stamped-older DB outright. Nothing has been written yet — the plan ran
    // read-only — so a refusal leaves the DB untouched.
    drop(store);
    let mut store = MemoryStore::open_with_context(db_str, &open_ctx)?;

    if eligible == 0 {
        // Nothing scheduled: no LLM client is constructed and no vault
        // materialization runs; the reopen above was authority-gated only.
        if options.json {
            let doc =
                summary_backfill_json_document(db_path, &plan, None, plan.with_summary, false);
            super::print_pretty_json(&doc)?;
        } else if plan.mode == "missing-only" && plan.missing == 0 {
            println!("\n✅ All entries have summaries!");
        } else {
            print_summary_plan_lines(&plan);
            println!(
                "\n✅ Nothing to summarize: 0 eligible of {selected} selected \
                 (skipped: {})",
                format_skip_reason_counts(&plan, None)
            );
        }
        return Ok(());
    }

    let llm = LlmClient::new_with_vault_db(Some(vault_db_path))
        .map_err(|e| format!("LLM client init failed: {e}"))?;
    let materialize_report = materialize_standalone(&llm, vault_db_path)
        .map_err(|e| IoError::new(ErrorKind::Other, e))?;
    // #1279: backfill stays strict — batch jobs fail-fast on skipped aliases.
    ensure_no_skipped_aliases(&materialize_report)?;
    let concurrency = backfill_llm_concurrency();

    if !options.json {
        println!("\nBackfilling {eligible} entries (concurrency={concurrency})...\n");
    }

    let llm_for_calls = llm.clone();
    let generate = move |input: String| {
        let llm = llm_for_calls.clone();
        async move {
            let generated = generate_summary_with_retry(&llm, &input).await?;
            let receipt = serde_json::to_value(&generated.invocation)
                .map_err(|error| format!("serialize summary invocation receipt: {error}"))?;
            Ok::<(String, serde_json::Value), String>((generated.value, receipt))
        }
    };

    let report =
        run_summary_backfill_apply(&mut store, &plan, concurrency, generate, !options.json).await;

    let final_missing = store.entries_missing_summaries()?.len();
    let final_with_summary = plan.total.saturating_sub(final_missing as u64);
    let apply_skipped = report
        .outcomes
        .iter()
        .filter(|(_, outcome)| {
            matches!(
                outcome,
                SummaryItemOutcome::EmptyOutput | SummaryItemOutcome::RevisionMismatch
            )
        })
        .count();
    let total_skipped = plan_skipped + apply_skipped;

    if options.json {
        let doc = summary_backfill_json_document(
            db_path,
            &plan,
            Some(&report),
            final_with_summary,
            false,
        );
        super::print_pretty_json(&doc)?;
    } else if report.failed == 0 {
        if total_skipped > 0 {
            // Explicit counts: an all-skipped run must not print an
            // unqualified success line.
            println!(
                "\n✅ Done! Summaries: {} → {} / {}; {} processed, {total_skipped} skipped \
                 (skipped: {})",
                plan.with_summary,
                final_with_summary,
                plan.total,
                report.processed,
                format_skip_reason_counts(&plan, Some(&report))
            );
        } else {
            println!(
                "\n✅ Done! Summaries: {} → {} / {}",
                plan.with_summary, final_with_summary, plan.total
            );
        }
    } else {
        println!(
            "\n⚠ Done with {} failure(s). Summaries: {} → {} / {}; {} processed, \
             {total_skipped} skipped (skipped: {})",
            report.failed,
            plan.with_summary,
            final_with_summary,
            plan.total,
            report.processed,
            format_skip_reason_counts(&plan, Some(&report))
        );
    }

    if let Some(message) = summary_backfill_exit_error(&report) {
        return Err(message.into());
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
        MemoryStore::open_read_only_existing_schema_compat(
            db_str,
            ReadOnlyBackfillOperation::Metadata,
        )?
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
        let generated = match result {
            Ok(generated) => generated,
            Err(error) => {
                failed += 1;
                eprintln!("  WARN: metadata failed for {id}: {error}");
                record_backfill_failure(&store, &id, "metadata", &error);
                continue;
            }
        };
        let (keywords, entities) = generated.value;
        // Domain-specific deterministic tagging was removed from the generic
        // engine; use the LLM-extracted keywords/entities directly.
        if keywords.is_empty() && entities.is_empty() {
            eprintln!("  WARN: metadata produced no accepted field for {id}, skipped");
            continue;
        }
        let receipt = match serde_json::to_value(&generated.invocation) {
            Ok(receipt) => receipt,
            Err(error) => {
                failed += 1;
                let error = format!("serialize metadata invocation receipt: {error}");
                eprintln!("  WARN: metadata receipt failed for {id}: {error}");
                record_backfill_failure(&store, &id, "metadata", &error);
                continue;
            }
        };

        match store.update_enrichment_fields_with_receipts(
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
            EnrichmentInvocationReceipts {
                metadata: Some(&receipt),
                ..Default::default()
            },
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

async fn generate_summary_with_retry(
    llm: &LlmClient,
    input: &str,
) -> Result<tachi_llm::Generated<String>, String> {
    retry_llm_call("summary", || llm.generate_summary_with_receipt(input)).await
}

async fn extract_metadata_with_retry(
    llm: &LlmClient,
    input: &str,
) -> Result<tachi_llm::Generated<(Vec<String>, Vec<String>)>, String> {
    retry_llm_call("metadata", || llm.extract_metadata_with_receipt(input)).await
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

/// Record an apply-phase failure only when the row is still at the planned
/// revision. A concurrent revision bump returns false — the new revision's
/// metadata is left untouched and the caller reports a revision_mismatch
/// skip instead of counting a failure against a row it no longer observes.
fn record_backfill_failure_if_revision(
    store: &MemoryStore,
    id: &str,
    stage: &str,
    error: impl Display,
    expected_revision: i64,
) -> bool {
    let error = bounded_error(error);
    match store.record_enrichment_failure_if_revision(id, stage, &error, expected_revision) {
        Ok(applied) => applied,
        Err(record_error) => {
            eprintln!("  WARN: failed to record enrichment failure for {id}: {record_error}");
            // The item itself failed; an inability to record does not erase
            // that, so keep the failure classification.
            true
        }
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
        MemoryStore::open_read_only_existing_schema_compat(db_str, ReadOnlyBackfillOperation::Fts)?
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
        plan_summary_backfill, record_cli_vector_sweep_state, run_backfill_fts,
        run_backfill_metadata, run_backfill_summaries, run_backfill_vectors,
        run_summary_backfill_apply, summary_backfill_exit_error, summary_backfill_json_document,
        validate_summary_backfill_options, SummaryBackfillOptions, SummaryItemOutcome,
        SummarySkipReason, SUMMARY_INPUT_MAX_CHARS,
    };
    use memcore::{AnchorKind, MemoryEntry, MemoryStore, MigrationAuthority, VectorBackfillScope};
    use serde_json::json;
    use std::path::Path;

    #[derive(Debug, PartialEq, Eq)]
    struct DryRunDbSnapshot {
        schema_version: i64,
        user_version: i64,
        sqlite_master: Vec<(String, String, String)>,
        hard_state: Vec<(String, String, String, i64, String, String)>,
        sibling_files: Vec<String>,
    }

    fn insert_memory(store: &mut MemoryStore, id: &str, source: &str, topic: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        store
            .upsert(&MemoryEntry {
                id: id.to_string(),
                path: "/p".to_string(),
                summary: String::new(),
                text: "body".to_string(),
                importance: 0.5,
                timestamp: now.clone(),
                valid_from: now,
                valid_until: None,
                category: "fact".to_string(),
                topic: topic.to_string(),
                keywords: Vec::new(),
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                source: source.to_string(),
                scope: "project".to_string(),
                archived: false,
                access_count: 0,
                scored_count: 0,
                last_access: None,
                last_use_at: None,
                revision: 1,
                metadata: json!({}),
                vector: None,
                retention_policy: None,
                domain: None,
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            })
            .expect("insert typed memory fixture");
    }

    fn write_fixture_vector(store: &mut MemoryStore, id: &str, vector: Vec<f32>) {
        let mut entry = store
            .get(id)
            .expect("read typed memory fixture")
            .expect("typed memory fixture exists");
        entry.vector = Some(vector);
        store.upsert(&entry).expect("write typed vector fixture");
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
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");

        // Durable content without a vector is the sole pending row.
        insert_memory(&mut store, "durable-1", "manual", "note");
        // Anchors are graph plumbing, not vector-backfill work.
        store
            .ensure_anchor(AnchorKind::Issue, "x")
            .expect("create typed anchor fixture");

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
        assert_eq!(
            status_missing, 1,
            "only the durable row is missing a vector"
        );
    }

    #[tokio::test]
    async fn dry_run_vector_backfill_does_not_write_sweep_state() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("dry-run.db");
        let vault_path = dir.path().join("vault.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        insert_memory(&mut store, "durable-1", "manual", "note");
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
        insert_memory(&mut store, "durable-1", "manual", "note");
        let dummy_vec = vec![0.0_f32; 1024];
        write_fixture_vector(&mut store, "durable-1", dummy_vec);
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
        insert_memory(&mut store, "durable-1", "manual", "note");
        let dummy_vec = vec![0.0_f32; 1024];
        write_fixture_vector(&mut store, "durable-1", dummy_vec);
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
            let mut store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
                .expect("seed current-schema db");
            insert_memory(&mut store, "fts-missing-1", "manual", "note");
            insert_memory(&mut store, "fts-missing-2", "manual", "note");
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

        let err = run_backfill_summaries(
            &db_path,
            &vault_path,
            false,
            &MigrationAuthority::Deny,
            &SummaryBackfillOptions::default(),
        )
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
            &SummaryBackfillOptions::default(),
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

        run_backfill_summaries(
            &db_path,
            &vault_path,
            true,
            &MigrationAuthority::Deny,
            &SummaryBackfillOptions::default(),
        )
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
    async fn dry_run_operations_require_only_their_v22_optional_capabilities() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("dry-run-optional-vector.db");
        let vault_path = dir.path().join("vault.db");
        seed_and_stamp_older_schema_version(&db_path);

        let conn = rusqlite::Connection::open(&db_path).expect("open fixture DB");
        conn.execute("DROP TABLE memories_vec", [])
            .expect("remove optional vector capability");
        drop(conn);
        let before = dry_run_db_snapshot(&db_path);

        run_backfill_summaries(
            &db_path,
            &vault_path,
            true,
            &MigrationAuthority::Deny,
            &SummaryBackfillOptions::default(),
        )
        .await
        .expect("summary dry-run must not require memories_vec");
        run_backfill_metadata(&db_path, &vault_path, true, &MigrationAuthority::Deny)
            .await
            .expect("metadata dry-run must not require memories_vec");
        run_backfill_fts(&db_path, false, true, &MigrationAuthority::Deny)
            .await
            .expect("FTS dry-run must require memories_fts, not memories_vec");
        assert_eq!(
            dry_run_db_snapshot(&db_path),
            before,
            "summary/metadata/FTS dry-runs must remain exactly read-only"
        );

        let error = run_backfill_vectors(
            &db_path,
            &vault_path,
            16,
            true,
            false,
            &MigrationAuthority::Deny,
        )
        .await
        .expect_err("vector dry-run must refuse a DB without memories_vec at open");
        assert!(
            error
                .to_string()
                .contains("vector dry-run requires virtual table 'memories_vec'"),
            "unexpected vector capability refusal: {error}"
        );
        assert_eq!(
            dry_run_db_snapshot(&db_path),
            before,
            "refused vector dry-run must not mutate the older DB"
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

    // --- summary backfill slice: flags, deterministic planning, and the
    // provider-injectable apply seam ---------------------------------------
    //
    // The apply seam (`run_summary_backfill_apply`) takes a closure that
    // returns (accepted summary, serialized invocation receipt). Production
    // wires `generate_summary_with_retry` + receipt serialization into it;
    // tests inject a deterministic closure so "provider called 0 times" and
    // exact-input claims are provable without keys or network.

    fn seed_summary_fixture(
        store: &mut MemoryStore,
        id: &str,
        summary: &str,
        text: &str,
    ) -> MemoryEntry {
        let now = chrono::Utc::now().to_rfc3339();
        let entry = MemoryEntry {
            id: id.to_string(),
            path: "/p".to_string(),
            summary: summary.to_string(),
            text: text.to_string(),
            importance: 0.5,
            timestamp: now.clone(),
            valid_from: now,
            valid_until: None,
            category: "fact".to_string(),
            topic: "note".to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "manual".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        store.upsert(&entry).expect("seed summary backfill fixture");
        entry
    }

    fn set_archived(store: &mut MemoryStore, id: &str) {
        let mut entry = store
            .get(id)
            .expect("read fixture")
            .expect("fixture exists");
        entry.archived = true;
        store.upsert(&entry).expect("archive fixture");
    }

    /// Full semantic row snapshot for preservation assertions: summary, text,
    /// revision, observation timestamps, updated_at, and the raw vector blob.
    fn row_snapshot(store: &MemoryStore, id: &str) -> String {
        store
            .connection()
            .query_row(
                "SELECT summary, text, revision, timestamp, valid_from, valid_until,
                        updated_at, archived,
                        COALESCE((SELECT LENGTH(embedding) FROM memories_vec WHERE id = memories.id), -1)
                 FROM memories WHERE id = ?1",
                [id],
                |row| {
                    Ok(format!(
                        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?
                    ))
                },
            )
            .expect("snapshot row")
    }

    fn test_receipt() -> serde_json::Value {
        json!({
            "schema": "model-invocation-v1",
            "lane": "summary",
            "engine_kind": "provider_http",
            "completion_status": "complete",
            "effective_model": "test-summary-model"
        })
    }

    #[test]
    fn summary_backfill_flags_validation() {
        assert!(
            validate_summary_backfill_options(&SummaryBackfillOptions {
                limit: Some(0),
                ..Default::default()
            })
            .expect_err("--limit 0 must be invalid before anything is opened")
            .to_string()
            .contains("positive"),
            "limit 0 error should explain the positive requirement"
        );

        assert!(
            validate_summary_backfill_options(&SummaryBackfillOptions {
                regenerate: true,
                ..Default::default()
            })
            .expect_err("--regenerate without ids must be invalid")
            .to_string()
            .contains("requires an explicit"),
            "regenerate error should name the missing explicit --id set"
        );

        validate_summary_backfill_options(&SummaryBackfillOptions {
            ids: vec!["m".to_string()],
            regenerate: true,
            limit: Some(3),
            json: true,
        })
        .expect("explicit regenerate form with a positive limit is valid");
    }

    #[test]
    fn summary_backfill_plan_dedups_and_classifies_deterministically() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("plan.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "b-missing", "", "short body");
        seed_summary_fixture(&mut store, "a-summarized", "existing", "short body");
        seed_summary_fixture(&mut store, "c-archived", "existing", "short body");
        set_archived(&mut store, "c-archived");
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");

        // Duplicates collapse; the plan order is the deduplicated BTreeSet
        // order, so the same input set always yields the same plan.
        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec![
                    "b-missing".to_string(),
                    "c-archived".to_string(),
                    "a-summarized".to_string(),
                    "b-missing".to_string(),
                ],
                ..Default::default()
            },
        )
        .expect("plan explicit selection");

        assert_eq!(plan.mode, "explicit-ids");
        assert_eq!(
            plan.rows
                .iter()
                .map(|row| (row.id.as_str(), row.skip))
                .collect::<Vec<_>>(),
            vec![
                ("a-summarized", Some(SummarySkipReason::AlreadySummarized)),
                ("b-missing", None),
                ("c-archived", Some(SummarySkipReason::Archived)),
            ],
            "deterministic dedup order; nonempty summary without --regenerate is skipped, \
             archived ids report archival status instead of a hidden bypass"
        );
        assert!(
            plan.rows[0].text.is_none() && plan.rows[2].text.is_none(),
            "skipped rows must not carry their bodies past the planner"
        );

        let regenerated = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec!["a-summarized".to_string(), "c-archived".to_string()],
                regenerate: true,
                ..Default::default()
            },
        )
        .expect("plan regenerate selection");
        assert_eq!(
            regenerated.rows[0].skip, None,
            "--regenerate targets the nonempty summary"
        );
        assert_eq!(
            regenerated.rows[1].skip,
            Some(SummarySkipReason::Archived),
            "--regenerate still refuses archived rows"
        );

        // The whole id set is validated before --limit truncates anything.
        let error = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec!["b-missing".to_string(), "typo-id".to_string()],
                limit: Some(1),
                ..Default::default()
            },
        )
        .expect_err("an unknown requested id must fail the run");
        let message = error.to_string();
        assert!(
            message.contains("typo-id") && message.contains("not found"),
            "unknown-id error should name the id and the failure: {message}"
        );
        assert!(
            message.contains("nothing was generated or written"),
            "unknown-id error should state that no provider ran and no write happened: {message}"
        );

        // Default sweep: missing-only, rowid order, existing rows untouched.
        let default_plan = plan_summary_backfill(&store, &SummaryBackfillOptions::default())
            .expect("plan default sweep");
        assert_eq!(default_plan.mode, "missing-only");
        assert_eq!(
            default_plan
                .rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b-missing"],
            "the default sweep stays missing-only; nonempty summaries are not replaced"
        );
    }

    #[test]
    fn summary_backfill_plan_applies_limit_after_validation() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("limit.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "row-1", "", "body one");
        seed_summary_fixture(&mut store, "row-2", "", "body two");
        seed_summary_fixture(&mut store, "row-3", "", "body three");
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");

        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                limit: Some(2),
                ..Default::default()
            },
        )
        .expect("plan limited sweep");
        let (selected, eligible, skipped) = plan.counts();
        assert_eq!((selected, eligible, skipped), (2, 2, 0));
        assert_eq!(
            plan.rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["row-1", "row-2"],
            "limit truncates the deterministic rowid-ordered selection"
        );
    }

    #[test]
    fn summary_backfill_plan_unicode_input_boundary() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("boundary.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        // Multi-byte text at exactly the cap (24 KB of UTF-8, 8000 chars).
        let at_cap: String = "漢".repeat(SUMMARY_INPUT_MAX_CHARS);
        let over_cap: String = "漢".repeat(SUMMARY_INPUT_MAX_CHARS + 1);
        seed_summary_fixture(&mut store, "at-cap", "", &at_cap);
        seed_summary_fixture(&mut store, "over-cap", "", &over_cap);
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");

        let plan = plan_summary_backfill(&store, &SummaryBackfillOptions::default())
            .expect("plan boundary selection");
        let by_id: std::collections::HashMap<&str, _> = plan
            .rows
            .iter()
            .map(|row| (row.id.as_str(), row.skip))
            .collect();
        assert_eq!(
            by_id["at-cap"], None,
            "exactly 8000 Unicode chars stays eligible"
        );
        assert_eq!(
            by_id["over-cap"],
            Some(SummarySkipReason::InputTooLong),
            "8001 Unicode chars is skipped as input_too_long, never tail-truncated"
        );
    }

    async fn apply_with_recording_provider(
        store: &mut MemoryStore,
        plan: &super::SummaryBackfillPlan,
        result: Result<(String, serde_json::Value), String>,
    ) -> (
        super::SummaryApplyReport,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let calls: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_fn = calls.clone();
        let generate = move |input: String| {
            let calls = calls_for_fn.clone();
            let result = result.clone();
            async move {
                calls.lock().expect("provider call log").push(input);
                result.clone()
            }
        };
        let report = run_summary_backfill_apply(store, plan, 2, generate, false).await;
        (report, calls)
    }

    #[tokio::test]
    async fn missing_only_apply_never_replaces_existing_summaries() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("missing-only.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "has-summary", "keep me", "body");
        seed_summary_fixture(&mut store, "needs-summary", "", "body");
        let untouched = row_snapshot(&store, "has-summary");
        drop(store);

        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let plan = plan_summary_backfill(&store, &SummaryBackfillOptions::default()).expect("plan");
        let (report, calls) = apply_with_recording_provider(
            &mut store,
            &plan,
            Ok(("fresh summary".to_string(), test_receipt())),
        )
        .await;

        assert_eq!(report.attempted, 1);
        assert_eq!(report.processed, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(
            calls.lock().expect("call log").as_slice(),
            ["body".to_string()].as_slice(),
            "only the missing-summary row reaches the provider"
        );
        assert_eq!(
            store
                .get("needs-summary")
                .expect("read")
                .expect("exists")
                .summary,
            "fresh summary"
        );
        assert_eq!(
            row_snapshot(&store, "has-summary"),
            untouched,
            "an out-of-selection nonempty summary must stay byte-for-byte unchanged"
        );
    }

    #[tokio::test]
    async fn regenerate_apply_replaces_only_the_explicit_id() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("regen.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "target", "stale summary", "target body");
        seed_summary_fixture(
            &mut store,
            "bystander",
            "bystander summary",
            "bystander body",
        );
        let bystander_before = row_snapshot(&store, "bystander");
        drop(store);

        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec!["target".to_string()],
                regenerate: true,
                ..Default::default()
            },
        )
        .expect("plan");
        let (report, calls) = apply_with_recording_provider(
            &mut store,
            &plan,
            Ok(("regenerated summary".to_string(), test_receipt())),
        )
        .await;

        assert_eq!(report.processed, 1);
        assert_eq!(
            calls.lock().expect("call log").as_slice(),
            ["target body".to_string()].as_slice()
        );
        assert_eq!(
            store.get("target").expect("read").expect("exists").summary,
            "regenerated summary"
        );
        assert_eq!(
            row_snapshot(&store, "bystander"),
            bystander_before,
            "out-of-selection rows must remain semantically unchanged"
        );
    }

    #[tokio::test]
    async fn over_cap_input_is_skipped_and_capped_input_passes_in_full() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("cap.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        let at_cap: String = "漢".repeat(SUMMARY_INPUT_MAX_CHARS);
        let over_cap: String = "漢".repeat(SUMMARY_INPUT_MAX_CHARS + 1);
        seed_summary_fixture(&mut store, "at-cap", "", &at_cap);
        seed_summary_fixture(&mut store, "over-cap", "", &over_cap);
        drop(store);

        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let plan = plan_summary_backfill(&store, &SummaryBackfillOptions::default()).expect("plan");
        let (report, calls) = apply_with_recording_provider(
            &mut store,
            &plan,
            Ok(("summary".to_string(), test_receipt())),
        )
        .await;

        let calls = calls.lock().expect("call log").clone();
        assert_eq!(
            calls,
            vec![at_cap.clone()],
            "exactly the full 8000-char multi-byte text is sent — no take/tail truncation"
        );
        assert_eq!(report.attempted, 1);
        assert_eq!(report.processed, 1);
        assert_eq!(
            report.outcomes,
            vec![("at-cap".to_string(), SummaryItemOutcome::Summarized)],
            "the over-cap row never reaches the provider (no LLM call for skipped rows)"
        );
        assert_eq!(
            store
                .get("over-cap")
                .expect("read")
                .expect("exists")
                .summary,
            "",
            "the over-cap row keeps no partial summary"
        );
    }

    #[tokio::test]
    async fn stale_revision_cas_skip_preserves_body_vector_and_receipt() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("cas.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "contended", "", "contended body");
        write_fixture_vector(&mut store, "contended", vec![0.25_f32; 1024]);
        drop(store);

        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec!["contended".to_string()],
                ..Default::default()
            },
        )
        .expect("plan");

        // A concurrent writer bumps the revision after the plan was read.
        let mut entry = store.get("contended").expect("read").expect("exists");
        entry.importance = 0.9;
        store.upsert(&entry).expect("concurrent write");
        let before = row_snapshot(&store, "contended");

        let (report, _calls) = apply_with_recording_provider(
            &mut store,
            &plan,
            Ok(("racing summary".to_string(), test_receipt())),
        )
        .await;

        assert_eq!(report.attempted, 1);
        assert_eq!(report.processed, 0);
        assert_eq!(report.failed, 0);
        assert_eq!(
            report.outcomes,
            vec![(
                "contended".to_string(),
                SummaryItemOutcome::RevisionMismatch
            )]
        );
        assert_eq!(
            row_snapshot(&store, "contended"),
            before,
            "a stale CAS must leave summary, body, vector, and receipts untouched"
        );
    }

    #[tokio::test]
    async fn successful_summary_preserves_observation_fields_and_updates_fts_receipt() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("success.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(
            &mut store,
            "target",
            "",
            "PRIVATE-BODY-MARKER raw observation",
        );
        write_fixture_vector(&mut store, "target", vec![0.5_f32; 1024]);
        // Baseline comes from the stored row: write-time normalization may
        // rewrite the constructed entry's timestamp formatting.
        let baseline = store.get("target").expect("read").expect("exists");
        seed_summary_fixture(
            &mut store,
            "bystander",
            "bystander summary",
            "bystander body",
        );
        let bystander_before = row_snapshot(&store, "bystander");
        drop(store);

        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec!["target".to_string()],
                ..Default::default()
            },
        )
        .expect("plan");
        let receipt = test_receipt();
        let (report, _calls) = apply_with_recording_provider(
            &mut store,
            &plan,
            Ok(("new summary".to_string(), receipt.clone())),
        )
        .await;

        assert_eq!(report.processed, 1);

        let stored = store.get("target").expect("read").expect("exists");
        assert_eq!(stored.summary, "new summary");
        assert_eq!(stored.text, baseline.text, "raw text is unchanged");
        assert_eq!(
            stored.timestamp, baseline.timestamp,
            "observation timestamp is unchanged"
        );
        assert_eq!(
            stored.valid_from, baseline.valid_from,
            "valid_from is unchanged"
        );
        assert_eq!(stored.valid_until, None);
        assert_eq!(
            stored.vector.as_deref(),
            Some(vec![0.5_f32; 1024].as_slice()),
            "the existing vector is preserved"
        );

        let fts_summary: String = store
            .connection()
            .query_row(
                "SELECT summary FROM memories_fts WHERE id = 'target'",
                [],
                |row| row.get(0),
            )
            .expect("FTS row must exist after the summary write");
        assert_eq!(
            fts_summary, "new summary",
            "FTS must reflect the new summary"
        );

        let stored_receipt: String = store
            .connection()
            .query_row(
                "SELECT json_extract(metadata, '$.enrichment.invocations.summary') \
                 FROM memories WHERE id = 'target'",
                [],
                |row| row.get(0),
            )
            .expect("receipt must be persisted beside the summary");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored_receipt).expect("parse receipt"),
            receipt,
            "the exact invocation receipt lands in the same revision-checked transaction"
        );
        assert_eq!(
            row_snapshot(&store, "bystander"),
            bystander_before,
            "out-of-selection rows stay unchanged"
        );
    }

    #[tokio::test]
    async fn provider_failure_is_counted_and_maps_to_a_nonzero_exit() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("failure.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "doomed", "", "body");
        drop(store);

        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let plan = plan_summary_backfill(&store, &SummaryBackfillOptions::default()).expect("plan");
        let (report, _calls) =
            apply_with_recording_provider(&mut store, &plan, Err("provider 503".to_string())).await;

        assert_eq!(report.attempted, 1);
        assert_eq!(report.processed, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(
            report.outcomes,
            vec![("doomed".to_string(), SummaryItemOutcome::FailedSummary)]
        );
        assert!(
            summary_backfill_exit_error(&report).is_some(),
            "failed items must produce a nonzero exit status"
        );
        let failure_stage: String = store
            .connection()
            .query_row(
                "SELECT json_extract(metadata, '$.enrichment.failed_stage') \
                 FROM memories WHERE id = 'doomed'",
                [],
                |row| row.get(0),
            )
            .expect("failure must be recorded on the row");
        assert_eq!(failure_stage, "summary");

        let success = super::SummaryApplyReport {
            attempted: 2,
            processed: 2,
            failed: 0,
            outcomes: Vec::new(),
        };
        assert!(
            summary_backfill_exit_error(&success).is_none(),
            "a clean run must not be reported as a failure"
        );
    }

    #[tokio::test]
    async fn empty_provider_output_is_an_honest_skip_not_a_failure() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("empty.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "blank", "", "body");
        drop(store);

        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let plan = plan_summary_backfill(&store, &SummaryBackfillOptions::default()).expect("plan");
        let (report, _calls) = apply_with_recording_provider(
            &mut store,
            &plan,
            Ok(("   ".to_string(), test_receipt())),
        )
        .await;

        assert_eq!(report.failed, 0, "empty output is a skip, not a failure");
        assert_eq!(report.processed, 0);
        assert_eq!(
            report.outcomes,
            vec![("blank".to_string(), SummaryItemOutcome::EmptyOutput)]
        );
    }

    #[tokio::test]
    async fn unknown_requested_id_fails_entry_before_provider_and_write() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("unknown-id.db");
        let vault_path = dir.path().join("vault.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "real", "", "body");
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen for state");
        let before = row_snapshot(&store, "real");
        drop(store);

        let error = run_backfill_summaries(
            &db_path,
            &vault_path,
            false,
            &MigrationAuthority::Deny,
            &SummaryBackfillOptions {
                ids: vec!["real".to_string(), "no-such-id".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect_err("an unknown requested id must fail the run");

        let message = error.to_string();
        assert!(
            message.contains("no-such-id"),
            "error must name the unknown id: {message}"
        );
        assert!(
            message.contains("nothing was generated or written"),
            "error must state no provider ran and no write happened: {message}"
        );

        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("verify store");
        assert_eq!(
            row_snapshot(&store, "real"),
            before,
            "unknown-id failure must not mutate any row"
        );
    }

    #[tokio::test]
    async fn all_skipped_selection_returns_without_llm_init() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("all-skipped.db");
        let vault_path = dir.path().join("vault.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        let over_cap: String = "字".repeat(SUMMARY_INPUT_MAX_CHARS + 1);
        seed_summary_fixture(&mut store, "too-long", "", &over_cap);
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("snapshot store");
        let before = row_snapshot(&store, "too-long");
        drop(store);

        // No provider env is configured in tests, so this succeeds only if
        // the runner returns before LLM client construction and vault
        // materialization (a stray init would fail on skipped aliases).
        run_backfill_summaries(
            &db_path,
            &vault_path,
            false,
            &MigrationAuthority::Deny,
            &SummaryBackfillOptions::default(),
        )
        .await
        .expect("a selection whose rows are all skipped must complete without an LLM");

        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("verify store");
        assert_eq!(row_snapshot(&store, "too-long"), before);
    }

    #[tokio::test]
    async fn dry_run_explicit_ids_on_stamped_older_db_stays_read_only() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("dry-run-ids-schema.db");
        let vault_path = dir.path().join("vault.db");
        {
            let mut store =
                MemoryStore::open(db_path.to_str().expect("utf8 db path")).expect("seed db");
            seed_summary_fixture(&mut store, "older-row", "", "body");
        }
        let conn = rusqlite::Connection::open(&db_path).expect("reopen to roll back stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp older schema version");
        drop(conn);
        let before = dry_run_db_snapshot(&db_path);

        run_backfill_summaries(
            &db_path,
            &vault_path,
            true,
            &MigrationAuthority::Deny,
            &SummaryBackfillOptions {
                ids: vec!["older-row".to_string()],
                limit: Some(1),
                json: true,
                ..Default::default()
            },
        )
        .await
        .expect("explicit-id dry-run must open read-only without the migration flag");

        assert_eq!(
            read_user_version(&db_path),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "dry-run must never mutate the schema stamp"
        );
        assert_eq!(
            dry_run_db_snapshot(&db_path),
            before,
            "explicit-id dry-run must remain exactly read-only"
        );
    }

    #[test]
    fn summary_backfill_json_document_reports_counts_without_bodies() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("json-doc.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(
            &mut store,
            "json-target",
            "",
            "PRIVATE-BODY-MARKER must not leak",
        );
        seed_summary_fixture(&mut store, "json-summarized", "existing", "other body");
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");

        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec!["json-target".to_string(), "json-summarized".to_string()],
                limit: Some(2),
                ..Default::default()
            },
        )
        .expect("plan");

        let report = super::SummaryApplyReport {
            attempted: 1,
            processed: 1,
            failed: 0,
            outcomes: vec![("json-target".to_string(), SummaryItemOutcome::Summarized)],
        };
        let doc = summary_backfill_json_document(&db_path, &plan, Some(&report), 1, false);
        let rendered = serde_json::to_string(&doc).expect("render json document");

        assert!(
            !rendered.contains("PRIVATE-BODY-MARKER"),
            "the JSON document must never carry memory bodies: {rendered}"
        );
        assert!(
            rendered.contains(r#""id":"json-target""#)
                && rendered.contains(r#""status":"summarized""#),
            "per-item ids and statuses must be reported: {rendered}"
        );
        assert!(
            rendered.contains(r#""reason":"already_summarized""#),
            "skip reasons must be reported: {rendered}"
        );
        assert_eq!(
            doc["counts"]["selected"].as_u64(),
            Some(2),
            "counts stay exact and un-capped: {rendered}"
        );
        assert_eq!(doc["counts"]["processed"].as_u64(), Some(1));
        assert_eq!(doc["items_truncated"].as_bool(), Some(false));
    }

    #[test]
    fn summary_backfill_json_document_caps_item_listing_but_not_counts() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("json-cap.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        for index in 0..super::MAX_SUMMARY_REPORT_ITEMS + 50 {
            insert_memory(&mut store, &format!("cap-{index}"), "manual", "note");
        }
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");

        let plan = plan_summary_backfill(&store, &SummaryBackfillOptions::default()).expect("plan");
        assert_eq!(plan.rows.len(), super::MAX_SUMMARY_REPORT_ITEMS + 50);

        let doc = summary_backfill_json_document(&db_path, &plan, None, 0, true);
        assert_eq!(
            doc["counts"]["selected"].as_u64(),
            Some((super::MAX_SUMMARY_REPORT_ITEMS + 50) as u64),
            "counts must stay exact"
        );
        assert_eq!(
            doc["items"].as_array().expect("items array").len(),
            super::MAX_SUMMARY_REPORT_ITEMS,
            "the item listing is bounded so stdout stays bounded"
        );
        assert_eq!(doc["items_truncated"].as_bool(), Some(true));
    }

    // --- Sol rereview (2026-09-24): consolidated repair discrimination
    // tests --------------------------------------------------------------

    /// #1: the plan phase is read-only for apply runs too — an unknown id
    /// under explicit `Allow` on a stamped-older DB must fail before the
    /// authority-gated write reopen, leaving DB, schema, markers, and
    /// sibling files untouched; a missing path is never created.
    #[tokio::test]
    async fn apply_plan_phase_never_creates_or_migrates_on_invalid_input() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("plan-first.db");
        let vault_path = dir.path().join("vault.db");
        {
            let mut store =
                MemoryStore::open(db_path.to_str().expect("utf8 db path")).expect("seed db");
            seed_summary_fixture(&mut store, "real-row", "", "body");
        }
        let conn = rusqlite::Connection::open(&db_path).expect("reopen to roll back stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp older schema version");
        drop(conn);
        let before = dry_run_db_snapshot(&db_path);

        let error = run_backfill_summaries(
            &db_path,
            &vault_path,
            false,
            &MigrationAuthority::Allow {
                approved_by: "test:plan-first-allow".to_string(),
            },
            &SummaryBackfillOptions {
                ids: vec!["real-row".to_string(), "typo-row".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect_err("an invalid id set must fail even under explicit Allow");
        assert!(
            error.to_string().contains("typo-row"),
            "error must name the unknown id: {error}"
        );
        assert_eq!(
            dry_run_db_snapshot(&db_path),
            before,
            "the failed plan must leave schema, stamp, markers, and files unchanged"
        );

        let missing = dir.path().join("never-created.db");
        let error = run_backfill_summaries(
            &missing,
            &vault_path,
            false,
            &MigrationAuthority::Allow {
                approved_by: "test:plan-first-missing".to_string(),
            },
            &SummaryBackfillOptions {
                ids: vec!["any-id".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect_err("a missing DB must not be created by the read-only plan phase");
        assert!(
            !missing.exists(),
            "no file may be created while the request is still invalid: {error}"
        );
    }

    /// #2: apply-phase failure recording is revision-guarded on both stages.
    /// A provider error or DB-write error observed at a stale revision is an
    /// honest revision_mismatch skip — the concurrently-moved row's metadata
    /// and updated_at stay untouched. The same DB-write error at the still
    /// current revision is recorded and counted as a failure.
    #[tokio::test]
    async fn apply_failure_recording_is_revision_guarded() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("guarded.db");

        // (a) provider error at a stale revision -> revision_mismatch skip.
        {
            let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
            seed_summary_fixture(&mut store, "stale-provider", "", "body");
            drop(store);
            let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
            let plan = plan_summary_backfill(
                &store,
                &SummaryBackfillOptions {
                    ids: vec!["stale-provider".to_string()],
                    ..Default::default()
                },
            )
            .expect("plan");
            let mut moved = store.get("stale-provider").expect("read").expect("exists");
            moved.importance = 0.9;
            store.upsert(&moved).expect("concurrent revision bump");
            let before = row_snapshot(&store, "stale-provider");

            let (report, _calls) =
                apply_with_recording_provider(&mut store, &plan, Err("provider 503".to_string()))
                    .await;

            assert_eq!(
                report.failed, 0,
                "a stale observation is a skip, not a failure"
            );
            assert_eq!(
                report.outcomes,
                vec![(
                    "stale-provider".to_string(),
                    SummaryItemOutcome::RevisionMismatch
                )]
            );
            assert_eq!(
                row_snapshot(&store, "stale-provider"),
                before,
                "the moved row's metadata and updated_at stay unpolluted"
            );
        }

        // (b) DB-write error at a stale revision -> revision_mismatch skip.
        // A non-object receipt deterministically fails receipt validation
        // inside the enrichment write.
        {
            let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
            seed_summary_fixture(&mut store, "stale-db-error", "", "body");
            drop(store);
            let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
            let plan = plan_summary_backfill(
                &store,
                &SummaryBackfillOptions {
                    ids: vec!["stale-db-error".to_string()],
                    ..Default::default()
                },
            )
            .expect("plan");
            let mut moved = store.get("stale-db-error").expect("read").expect("exists");
            moved.importance = 0.9;
            store.upsert(&moved).expect("concurrent revision bump");
            let before = row_snapshot(&store, "stale-db-error");

            let (report, _calls) = apply_with_recording_provider(
                &mut store,
                &plan,
                Ok((
                    "summary".to_string(),
                    serde_json::Value::String("not-a-receipt-object".to_string()),
                )),
            )
            .await;

            assert_eq!(
                report.failed, 0,
                "the guarded record refuses the stale revision"
            );
            assert_eq!(
                report.outcomes,
                vec![(
                    "stale-db-error".to_string(),
                    SummaryItemOutcome::RevisionMismatch
                )]
            );
            assert_eq!(
                row_snapshot(&store, "stale-db-error"),
                before,
                "the moved row stays untouched"
            );
        }

        // (c) the same DB-write error at the still-current revision is
        // recorded and counted as a failure.
        {
            let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
            seed_summary_fixture(&mut store, "fresh-db-error", "", "body");
            drop(store);
            let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
            let plan = plan_summary_backfill(
                &store,
                &SummaryBackfillOptions {
                    ids: vec!["fresh-db-error".to_string()],
                    ..Default::default()
                },
            )
            .expect("plan");

            let (report, _calls) = apply_with_recording_provider(
                &mut store,
                &plan,
                Ok((
                    "summary".to_string(),
                    serde_json::Value::String("not-a-receipt-object".to_string()),
                )),
            )
            .await;

            assert_eq!(report.failed, 1);
            assert_eq!(
                report.outcomes,
                vec![(
                    "fresh-db-error".to_string(),
                    SummaryItemOutcome::FailedDbUpdate
                )]
            );
            let failed_stage: String = store
                .connection()
                .query_row(
                    "SELECT json_extract(metadata, '$.enrichment.failed_stage') \
                     FROM memories WHERE id = 'fresh-db-error'",
                    [],
                    |row| row.get(0),
                )
                .expect("failure recorded at the matching revision");
            assert_eq!(failed_stage, "db_update");
        }
    }

    /// #3: Total/Missing/Summaries must share one population basis — an
    /// archived-only DB reports total 1 / missing 1, not 0/0.
    #[test]
    fn archived_only_db_reports_consistent_counts() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("archived-only.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "archived-missing", "", "body");
        set_archived(&mut store, "archived-missing");
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");

        let plan = plan_summary_backfill(&store, &SummaryBackfillOptions::default())
            .expect("plan archived-only DB");
        assert_eq!(plan.total, 1, "the total must include archived rows");
        assert_eq!(plan.missing, 1, "the archived row is missing a summary");
        assert_eq!(plan.with_summary, 0);
    }

    /// #5 + #6 + #4: requested_after_dedup stays pre-limit, counts.skipped
    /// aggregates plan and apply skips, the selected == processed + failed +
    /// skipped identity holds, and dry_run is never inferred from a missing
    /// report.
    #[tokio::test]
    async fn json_counts_identity_and_pre_limit_request_size() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("identity.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "plain", "", "body");
        seed_summary_fixture(&mut store, "kept", "existing", "body");
        seed_summary_fixture(&mut store, "blank-reply", "", "body");
        drop(store);

        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec![
                    "plain".to_string(),
                    "kept".to_string(),
                    "blank-reply".to_string(),
                    "plain".to_string(),
                ],
                ..Default::default()
            },
        )
        .expect("plan");
        assert_eq!(
            plan.requested_after_dedup,
            Some(3),
            "duplicate id collapses pre-limit"
        );

        // Both eligible rows get the blank provider output; nothing is
        // processed and both land as apply-time empty_output skips.
        let (report, _calls) = apply_with_recording_provider(
            &mut store,
            &plan,
            Ok(("   ".to_string(), test_receipt())),
        )
        .await;
        assert_eq!(report.processed, 0);
        assert_eq!(report.attempted, 2);

        let doc = summary_backfill_json_document(&db_path, &plan, Some(&report), 1, false);
        let counts = &doc["counts"];
        assert_eq!(
            counts["requested_after_dedup"].as_u64(),
            Some(3),
            "requested_after_dedup is the pre-limit dedup size, not selected"
        );
        assert_eq!(counts["selected"].as_u64(), Some(3));
        assert_eq!(counts["processed"].as_u64(), Some(0));
        assert_eq!(counts["failed"].as_u64(), Some(0));
        assert_eq!(
            counts["skipped"].as_u64(),
            Some(3),
            "skipped aggregates plan skips (already_summarized) and apply skips \
             (empty_output) plus nothing else here"
        );
        let selected = counts["selected"].as_u64().expect("selected");
        let processed = counts["processed"].as_u64().expect("processed");
        let failed = counts["failed"].as_u64().expect("failed");
        let skipped = counts["skipped"].as_u64().expect("skipped");
        assert_eq!(
            selected,
            processed + failed + skipped,
            "the count identity must hold for a completed run"
        );
        assert_eq!(
            doc["skip_reasons"]["already_summarized"].as_u64(),
            Some(1),
            "skip_reasons keeps the per-reason breakdown"
        );
        assert_eq!(doc["skip_reasons"]["empty_output"].as_u64(), Some(2));
        assert_eq!(
            doc["dry_run"].as_bool(),
            Some(false),
            "an apply run is never mislabeled dry_run"
        );

        // A dry-run document over the same plan is labeled dry_run: true.
        let dry = summary_backfill_json_document(&db_path, &plan, None, plan.with_summary, true);
        assert_eq!(dry["dry_run"].as_bool(), Some(true));
        // #4: an apply run with nothing eligible passes report None and must
        // still report dry_run: false.
        let zero_eligible =
            summary_backfill_json_document(&db_path, &plan, None, plan.with_summary, false);
        assert_eq!(zero_eligible["dry_run"].as_bool(), Some(false));
    }

    /// #5: --limit truncates the selection without shrinking the
    /// pre-limit request size.
    #[test]
    fn json_requested_after_dedup_survives_limit_truncation() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("prelimit.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "one", "", "body");
        seed_summary_fixture(&mut store, "two", "", "body");
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");

        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec!["one".to_string(), "two".to_string(), "one".to_string()],
                limit: Some(1),
                ..Default::default()
            },
        )
        .expect("plan");
        let (selected, _eligible, _skipped) = plan.counts();
        assert_eq!(selected, 1, "limit truncates the scheduled selection");
        assert_eq!(
            plan.requested_after_dedup,
            Some(2),
            "the deduplicated request size stays pre-limit"
        );

        let doc = summary_backfill_json_document(&db_path, &plan, None, 0, true);
        assert_eq!(doc["counts"]["requested_after_dedup"].as_u64(), Some(2));
        assert_eq!(doc["counts"]["selected"].as_u64(), Some(1));
    }

    /// #7: a think-tag-only provider output is an honest empty_output skip —
    /// the row is preserved instead of becoming a phantom processed write.
    #[tokio::test]
    async fn think_only_provider_output_is_an_empty_output_skip() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("think-only.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        seed_summary_fixture(&mut store, "thinky", "", "body");
        drop(store);

        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec!["thinky".to_string()],
                ..Default::default()
            },
        )
        .expect("plan");
        let (report, _calls) = apply_with_recording_provider(
            &mut store,
            &plan,
            Ok((
                "<think>private chain of thought</think>".to_string(),
                test_receipt(),
            )),
        )
        .await;

        assert_eq!(
            report.processed, 0,
            "a scrubbed-empty output is not processed"
        );
        assert_eq!(report.failed, 0);
        assert_eq!(
            report.outcomes,
            vec![("thinky".to_string(), SummaryItemOutcome::EmptyOutput)]
        );
        let stored = store.get("thinky").expect("read").expect("exists");
        assert_eq!(
            stored.summary, "",
            "no partial or think-tag summary is stored"
        );
        let fts_rows: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memories_fts WHERE id = 'thinky'",
                [],
                |row| row.get(0),
            )
            .expect("fts check");
        assert_eq!(fts_rows, 1, "the FTS row keeps the original row content");
        let receipt_json: Option<String> = store
            .connection()
            .query_row(
                "SELECT json_extract(metadata, '$.enrichment.invocations.summary') \
                 FROM memories WHERE id = 'thinky'",
                [],
                |row| row.get(0),
            )
            .expect("receipt check");
        assert!(
            receipt_json.is_none(),
            "no receipt is persisted for a skipped output"
        );
    }

    /// #8: the human terminal formatter names fixed reasons including
    /// apply-time empty/stale skips.
    #[test]
    fn skip_reason_formatter_names_plan_and_apply_reasons_in_fixed_order() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("formatter.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        let over_cap: String = "长".repeat(super::SUMMARY_INPUT_MAX_CHARS + 1);
        seed_summary_fixture(&mut store, "too-long", "", &over_cap);
        seed_summary_fixture(&mut store, "summarized", "existing", "body");
        drop(store);
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");

        let plan = plan_summary_backfill(
            &store,
            &SummaryBackfillOptions {
                ids: vec!["too-long".to_string(), "summarized".to_string()],
                ..Default::default()
            },
        )
        .expect("plan");
        let report = super::SummaryApplyReport {
            attempted: 2,
            processed: 1,
            failed: 0,
            outcomes: vec![
                ("blank".to_string(), SummaryItemOutcome::EmptyOutput),
                ("moved".to_string(), SummaryItemOutcome::RevisionMismatch),
            ],
        };
        assert_eq!(
            super::format_skip_reason_counts(&plan, Some(&report)),
            "already_summarized: 1, input_too_long: 1, empty_output: 1, revision_mismatch: 1",
            "fixed reason order covering plan and apply skips"
        );
        assert_eq!(
            super::format_skip_reason_counts(&plan, None),
            "already_summarized: 1, input_too_long: 1"
        );
    }
}

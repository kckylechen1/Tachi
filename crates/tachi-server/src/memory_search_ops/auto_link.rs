use crate::memory_search_ops::confidence_reinforce::{
    confidence_increment, vector_similarity_between,
};
use crate::{DbScope, MemoryServer};
use memcore::{
    ExpectedMemoryState, LayerAvailability, MemoryEntry, MemoryStore, SupersessionCommitResult,
    SupersessionExpectedState,
};
use serde_json::json;
use std::collections::HashSet;
use std::time::{Duration, Instant};

const REINFORCEMENT_MIN_SIMILARITY: f64 = 0.75;
const REINFORCEMENT_DUPLICATE_SIMILARITY: f64 = 0.95;
const AUTO_LINK_SUPERSESSION_ROUTE: &str = "auto_link_supersession_v1";
const AUTO_LINK_SUPERSESSION_POLICY_VERSION: &str = "auto-link-semantic-supersession-v1";

// ---------------------------------------------------------------------------
// tachi#1097 PERF-T3 S1 — Auto-link phase-attribution receipt.
//
// Pure observation: the four skip points in `run_auto_linking`'s per-hit loop
// (self-id, training-seed, no-shared-entities,
// neither-supersede-nor-reinforce) are NOT changed — we only count them. The
// edge write is NOT changed — we only time it. Read time = the per-entity
// `with_*_store_read` searches; write time = the per-edge `with_*_store`
// writes. The mechanism that keeps this observation-only: the instrumentation
// issues no store call of its own, so it adds no DB I/O; it touches no
// scoring, gating or relation-selection input. (Whether the added
// `Instant::now` reads and counter increments are cheap relative to the DB
// work they bracket is NOT claimed here — nothing in this leaf measures that.)
//
// Per #1097 D2 this receipt carries ONLY the existing `entry.id` — no
// query/trace id is minted, none exists in the codebase. Per D5 it contains no
// entity names (entities are the search queries here, so naming them would log
// query text), no memory content, no DB path.
//
// #1097 r1 codex review ① (D5 hard line): `entry.id` is NOT necessarily a
// save-generated UUID — `save_memory` accepts a caller-supplied
// `params.id` verbatim (handler.rs:46-49 only generates one when `id` is
// MISSING), the wire field has no content constraint
// (tachi-params/src/memory.rs:104), and validation only checks presence
// (save_memory/validation.rs:13). A caller can therefore push query text,
// memory content, entity names, or credentials INTO `id`, and the unredacted
// value used to flow straight into this receipt and the `tracing::info!`
// line. UUID shape is not provenance: a caller can supply UUID-looking text
// too. The `entry_id` field and log point always carry the fixed
// [`REDACTED_ENTRY_ID`] marker, never the raw string.
//
// The receipt is emitted from INSIDE the spawned task because the save
// handler returns `"auto_link": "pending"` (handler.rs:261-262) without
// awaiting it — the task is the only place the finished counts exist. The
// task's body is `run_auto_linking`, a plain synchronous function, so the
// receipt (timers included) is assertable from a test without a runtime;
// `spawn_auto_linking` is only the spawn + emit shell around it.
// ---------------------------------------------------------------------------

/// Fixed marker placed in [`AutoLinkReceipt::entry_id`] and the log line.
/// UUID syntax is caller-controllable, so no raw id is safe telemetry.
pub const REDACTED_ENTRY_ID: &str = "<redacted-entry-id>";

/// Redact every entry id. Syntax validation cannot establish provenance: the
/// save API accepts a caller-supplied UUID verbatim, so UUID-shaped ids are
/// just as unsafe to emit as arbitrary text.
///
/// Owned `String` return because the receipt field is `String`; this keeps
/// the redaction unit test free of lifetime gymnastics.
pub(crate) fn redacted_entry_id(_id: &str) -> String {
    REDACTED_ENTRY_ID.to_string()
}

/// Per-call receipt for one `spawn_auto_linking` invocation, emitted via
/// `tracing::info!` from inside the spawned task. See the module-level
/// honesty rules above for what is and is not populated.
#[derive(Debug, Clone)]
pub struct AutoLinkReceipt {
    /// The id of the entry whose entities drove this auto-link pass, AFTER
    /// [`redacted_entry_id`] redaction. Carried so a log reader can pair
    /// this receipt with its save response without minting a new trace id
    /// (none exists codebase-wide, per #1097 D2). Per #1097 r1 codex
    /// review ① this is always the fixed [`REDACTED_ENTRY_ID`] marker — never
    /// raw caller-controlled input.
    pub entry_id: String,
    /// `entry.entities.iter().cloned().collect::<HashSet>().len()` —
    /// deduplicated entity count driving the per-entity search loop.
    pub entity_count: usize,
    /// Number of per-entity searches that ACTUALLY EXECUTED — i.e. the
    /// store search returned a result set (possibly empty). #1097 r1
    /// codex review ③-A: this was previously incremented before the
    /// result was checked, so a named-project resolution failure
    /// (server_methods/db.rs:280) counted as "executed". It no longer
    /// does — see [`SearchOutcome::Failed`] / `searches_failed`.
    pub searches_executed: usize,
    /// Number of per-entity searches that FAILED before producing a
    /// result set (named-project resolution error, store error). #1097
    /// r1 codex review ③-A: separated from `searches_executed` so the
    /// receipt no longer conflates attempts with executions.
    pub searches_failed: usize,
    /// Number of search results examined across all per-entity searches
    /// (the inner `for result in results` loop at auto_link.rs:142).
    pub candidates_examined: usize,
    /// Number of edges for which `save_edge_action` was actually invoked
    /// (auto_link.rs:230-234) — every write attempt.
    pub edges_attempted: usize,
    /// Number of edge writes whose INSERT landed (the edge row is
    /// persisted). #1097 r1 codex review ③-B: the edge insert commits
    /// under its OWN savepoint (db/graph.rs:144, RELEASEd at :158)
    /// BEFORE the post-insert supersede/reinforce update runs, so an
    /// insert that landed must count here EVEN WHEN the update fails —
    /// the edge is in the DB either way. Previously this only counted
    /// insert+update both-Ok, hiding a real persisted edge behind a zero.
    pub edges_written: usize,
    /// Number of edge writes where the INSERT landed but the post-insert
    /// update (supersede / reinforce) failed. #1097 r1 codex review ③-B:
    /// separated from `edges_written` so the partial-write case is
    /// visible instead of erased.
    pub post_write_failures: usize,
    /// Skip-counter breakdown covering the four `continue` points in the
    /// per-hit loop (auto_link.rs:143/146/153/175). Their sum plus
    /// `edges_attempted` equals `candidates_examined`.
    pub skipped_self_id: usize,
    pub skipped_training_seed: usize,
    pub skipped_no_shared_entities: usize,
    pub skipped_no_supersede_or_reinforce: usize,
    /// Wall time inside per-entity `with_*_store_read` search calls
    /// (auto_link.rs:136/:138). This is the TOTAL read-call wall time —
    /// `pool_checkout_wait` below, when [`LayerAvailability::Measured`], is a
    /// SUBSET of this value (the portion spent waiting for a read-pool slot,
    /// not a separate span), so the actual search-execution time is
    /// `read_elapsed - pool_checkout_wait`.
    pub read_elapsed: Duration,
    /// tachi#1145: the pool-checkout-wait portion of `read_elapsed` —
    /// kckylechen1/tachi#1125's `ReadPoolCheckoutReceipt::pool_checkout_wait`,
    /// carried down via a recording read call, mirroring how
    /// `memcore::SearchPhaseReceipt::pool_wait` carries it into the recall
    /// path (`search_memory/rows.rs`). [`LayerAvailability::Measured`] only
    /// on the `DbScope::Global` + no-named-project path (the only read call
    /// with a recording twin today — `DbRuntime::with_global_store_read_recording`,
    /// #1125); [`LayerAvailability::Unavailable`] on the named-project /
    /// per-path pool, which has no recording twin yet (a future leaf's, not
    /// invented here); [`LayerAvailability::NotSampled`] when the whole
    /// receipt is unsampled. Never a bare `0` standing in for "not measured"
    /// (#1097 D1) — that is exactly the honesty rule `LayerAvailability`
    /// exists to enforce.
    pub pool_checkout_wait: LayerAvailability,
    /// Wall time inside per-edge `with_*_store` write calls
    /// (auto_link.rs:231/:233).
    pub write_elapsed: Duration,
    /// tachi#1145: wall time between [`spawn_auto_linking`] requesting a
    /// `tokio::spawn` and this task's body actually starting to run — the
    /// spawn-queue delay every timer above is blind to, because they all
    /// start only once the task is already executing. Always a real
    /// measurement (never `Unavailable`) when the receipt exists: unlike
    /// pool-checkout wait, capturing `Instant::now()` immediately before
    /// `tokio::spawn` needs no layer this receipt cannot reach.
    /// `Duration::ZERO` on the unsampled path — but the receipt itself is
    /// `None` there, so this value is never read in that case.
    pub spawn_queue_wait: Duration,
    /// Wall time of the whole spawned task end-to-end (from
    /// [`run_auto_linking`]'s own `task_start`, NOT from the pre-spawn
    /// instant — see `spawn_queue_wait` for the piece before that).
    pub total_elapsed: Duration,
}

/// Receipt-counter classification for one per-entity search. #1097 r1 codex
/// review ③-A: `searches_executed` must count searches that ACTUALLY
/// produced a result set, not mere attempts — a named-project resolution
/// failure (`server_methods/db.rs:280`) never reaches the store's `search`
/// call, so it goes to [`SearchOutcome::Failed`] / `searches_failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchOutcome {
    /// The store search executed and returned a result set (possibly empty).
    Executed,
    /// Store resolution or the search itself returned `Err` — no result set
    /// was produced.
    Failed,
}

/// Receipt-counter classification for one edge write. #1097 r1 codex review
/// ③-B: the edge insert commits under its OWN savepoint (`db/graph.rs:144`,
/// `RELEASE`d at :158) before the post-insert supersede/reinforce update
/// runs, so an insert that landed is a persisted edge regardless of the
/// later update's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeWriteOutcome {
    /// Edge insert landed AND the dependent update (supersede/reinforce)
    /// landed too.
    InsertAndPostWriteOk,
    /// Edge insert landed, post-insert update FAILED. The edge IS persisted
    /// (its savepoint released), so `edges_written` must still count it —
    /// but `post_write_failures` also increments so the partial write is
    /// visible instead of erased.
    #[cfg(test)]
    InsertOkPostWriteFailed,
    /// No edge landed. This covers an insert/store error and a semantic
    /// supersession replay that correctly performed no new mutation.
    NoWrite,
}

impl AutoLinkReceipt {
    /// Apply one per-entity search outcome to the receipt counters. This is
    /// the SINGLE source of truth for the #1097 r1 codex review ③-A
    /// "attempts vs executions" semantics — the spawn block routes every
    /// search result through here so the counting cannot drift from the
    /// tested contract.
    pub(crate) fn apply_search_outcome(&mut self, outcome: SearchOutcome) {
        match outcome {
            SearchOutcome::Executed => self.searches_executed += 1,
            SearchOutcome::Failed => self.searches_failed += 1,
        }
    }

    /// Apply one edge-write outcome to the receipt counters. This is the
    /// SINGLE source of truth for the #1097 r1 codex review ③-B
    /// "insert-landed vs full-success" semantics: `edges_attempted` always
    /// increments (the loop reached the write), `edges_written` increments
    /// whenever the INSERT landed (even if the post-insert update failed —
    /// the edge is persisted either way), and `post_write_failures`
    /// increments only on the partial-write case.
    pub(crate) fn apply_edge_outcome(&mut self, outcome: EdgeWriteOutcome) {
        self.edges_attempted += 1;
        match outcome {
            EdgeWriteOutcome::InsertAndPostWriteOk => self.edges_written += 1,
            #[cfg(test)]
            EdgeWriteOutcome::InsertOkPostWriteFailed => {
                self.edges_written += 1;
                self.post_write_failures += 1;
            }
            EdgeWriteOutcome::NoWrite => {}
        }
    }
}

/// Finalize an accumulated [`AutoLinkReceipt`]: apply the redacted
/// `entry_id` (#1097 r1 codex review ① — always the fixed
/// [`REDACTED_ENTRY_ID`] marker) and the end-to-end `total_elapsed`.
///
/// This is the pure tail of [`run_auto_linking`]: it needs no store, no
/// runtime and no subscriber, so the two post-loop assignments
/// (`receipt.entry_id = redacted_entry_id(...)` and `receipt.total_elapsed =
/// <the total it is handed>`) are a unit-tested contract. [`run_auto_linking`]
/// returns its accumulated receipt through here, so reverting the redaction
/// (echoing the raw id) or dropping the total-elapsed assignment turns the
/// `finalize_*` tests red.
///
/// Note the split of duties, because it is easy to over-read: these tests
/// prove `finalize` stamps the total it is *given*. They cannot see the
/// `task_start.elapsed()` argument at the call site — that is covered
/// separately by `run_auto_linking_reports_live_read_write_and_total_timers`,
/// which drives [`run_auto_linking`] against a live store.
pub(crate) fn finalize_auto_link_receipt(
    mut receipt: AutoLinkReceipt,
    raw_entry_id: &str,
    total_elapsed: Duration,
) -> AutoLinkReceipt {
    receipt.entry_id = redacted_entry_id(raw_entry_id);
    receipt.total_elapsed = total_elapsed;
    receipt
}

pub(crate) fn path_root(path: &str) -> &str {
    path.trim_matches('/').split('/').next().unwrap_or("")
}

pub(crate) fn is_training_seed(entry: &MemoryEntry) -> bool {
    entry.path == "/sft"
        || entry.path.starts_with("/sft/")
        || entry.topic.eq_ignore_ascii_case("sft-memory")
        || entry.source.eq_ignore_ascii_case("sft_seed")
        || entry
            .metadata
            .get("training_sample")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
}

pub(crate) fn is_newer_than(new_ts: &str, old_ts: &str) -> bool {
    let new = chrono::DateTime::parse_from_rfc3339(new_ts);
    let old = chrono::DateTime::parse_from_rfc3339(old_ts);
    match (new, old) {
        (Ok(new), Ok(old)) => new > old,
        _ => new_ts > old_ts,
    }
}

pub(crate) fn should_supersede(
    new_entry: &MemoryEntry,
    old_entry: &MemoryEntry,
    shared_count: usize,
    _symbolic_score: f64,
) -> bool {
    let same_path = new_entry.path == old_entry.path;
    let same_non_empty_topic =
        !new_entry.topic.trim().is_empty() && new_entry.topic == old_entry.topic;

    matches!(new_entry.category.as_str(), "fact" | "preference")
        && matches!(old_entry.category.as_str(), "fact" | "preference")
        && is_newer_than(&new_entry.timestamp, &old_entry.timestamp)
        && shared_count >= 2
        && (same_path || same_non_empty_topic)
        && path_root(&new_entry.path) == path_root(&old_entry.path)
}

pub(crate) fn should_reinforce(
    new_entry: &MemoryEntry,
    old_entry: &MemoryEntry,
    shared_count: usize,
    similarity: f64,
    supersedes: bool,
) -> bool {
    !supersedes
        && shared_count > 0
        && matches!(new_entry.category.as_str(), "fact" | "preference")
        && matches!(old_entry.category.as_str(), "fact" | "preference")
        && path_root(&new_entry.path) == path_root(&old_entry.path)
        && (REINFORCEMENT_MIN_SIMILARITY..REINFORCEMENT_DUPLICATE_SIMILARITY).contains(&similarity)
}

/// Unique entities present in both lists (order-independent). Used so duplicate
/// labels in either entry cannot inflate `shared_count` past the fog floor.
pub(crate) fn unique_shared_entities(a: &[String], b: &[String]) -> Vec<String> {
    let set_a: HashSet<&str> = a.iter().map(String::as_str).collect();
    let mut out: HashSet<String> = HashSet::new();
    for e in b {
        if set_a.contains(e.as_str()) {
            out.insert(e.clone());
        }
    }
    out.into_iter().collect()
}

pub(crate) fn numbers_in_text(text: &str) -> HashSet<String> {
    static NUMBER_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = NUMBER_RE.get_or_init(|| {
        regex::Regex::new(r"\b\d{1,3}(?:,\d{3})*(?:\.\d+)?%?\b|\b\d+(?:\.\d+)?%?\b").unwrap()
    });
    re.find_iter(text)
        .map(|m| m.as_str().replace(',', "").to_ascii_lowercase())
        .collect()
}

pub(crate) fn has_numeric_mismatch(new_entry: &MemoryEntry, old_entry: &MemoryEntry) -> bool {
    let new_numbers = numbers_in_text(&new_entry.text);
    let old_numbers = numbers_in_text(&old_entry.text);
    !new_numbers.is_empty() && !old_numbers.is_empty() && new_numbers != old_numbers
}

fn auto_link_receipt_sampling_enabled() -> bool {
    auto_link_receipt_sampling_enabled_from(std::env::var("TACHI_AUTO_LINK_PHASE_RECEIPTS").ok())
}

/// tachi#1185 fix-round (codex review checkpoint 4, CRITICAL): pure,
/// directly-testable aggregation for [`AutoLinkReceipt::pool_checkout_wait`]
/// across the per-entity search loop in [`run_auto_linking`]. `Unavailable`
/// is STICKY: once any per-entity read in the loop reports `Unavailable`,
/// the aggregate must stay `Unavailable` for the rest of the loop — the
/// prior version's `_ => next` fallback let a LATER `Measured` entity
/// re-promote the field back to `Measured`, silently erasing an EARLIER
/// entity's unmeasured read (order-dependent fail-open: `HashSet` entity
/// iteration order — `auto_link.rs:381-382` — made which entity landed last
/// nondeterministic). `Measured` values accumulate as summed wall time,
/// matching the pre-existing `read_elapsed` accumulation pattern; the
/// initial `NotSampled` seed is replaced outright by the first real
/// classification. See `pool_checkout_wait_aggregation_*` tests below for
/// the exact scenario this closes.
fn accumulate_pool_checkout_wait(
    acc: LayerAvailability,
    next: LayerAvailability,
) -> LayerAvailability {
    match (acc, next) {
        (LayerAvailability::Measured(prior), LayerAvailability::Measured(this)) => {
            LayerAvailability::Measured(prior + this)
        }
        (LayerAvailability::Unavailable, _) | (_, LayerAvailability::Unavailable) => {
            LayerAvailability::Unavailable
        }
        (_, next) => next,
    }
}

fn auto_link_receipt_sampling_enabled_from(value: Option<String>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

/// Shared spawn-and-run core for `spawn_auto_linking` and its test-only
/// correlation-token twin `spawn_auto_linking_for_test` (tachi#1145): does
/// the dedup + sampling-gate + `tokio::spawn` + `spawn_queue_wait` timing
/// once, and calls [`run_auto_linking`] — [`AutoLinkReceipt`]'s single
/// source of truth — exactly once. `on_complete` is the ONLY thing that
/// differs between the two callers (production logs; the test twin threads
/// a correlation token through a channel), so the actual auto-link
/// algorithm, the pooling/locking it goes through, and the
/// spawn-queue-wait timer cannot drift between what production runs and
/// what the test measures. Bare-name references above (not `[`...`]`
/// links) because `spawn_auto_linking_for_test` is `#[cfg(test)]`-gated and
/// does not exist in a non-test build, where an intra-doc link to it would
/// be broken.
fn spawn_and_run_auto_linking(
    server: &MemoryServer,
    entry: &MemoryEntry,
    target_db: DbScope,
    named_project: Option<String>,
    on_complete: impl FnOnce(AutoLinkReceipt) + Send + 'static,
) {
    if is_training_seed(entry) {
        return;
    }

    let auto_link_server = server.clone();
    let auto_link_entry = entry.clone();
    // Dedup source entities so duplicate labels cannot inflate shared_count
    // past the #773 related_to fog floor (Gemini #905 review).
    let auto_link_entities: HashSet<String> = entry.entities.iter().cloned().collect();
    let auto_link_entity_list: Vec<String> = auto_link_entities.iter().cloned().collect();
    let sample_receipt = auto_link_receipt_sampling_enabled();
    // tachi#1145: captured BEFORE `tokio::spawn`, so the elapsed time taken
    // inside the spawned task below is the spawn-queue delay itself — every
    // timer inside `run_auto_linking` starts only once the task is already
    // executing and is blind to this. `sample_receipt`-gated like every
    // other timer in this module: no `Instant::now()` call on the unsampled
    // path.
    let spawn_requested_at = sample_receipt.then(Instant::now);

    tokio::spawn(async move {
        let spawn_queue_wait = spawn_requested_at.map(|started| started.elapsed());
        let receipt = run_auto_linking(
            &auto_link_server,
            &auto_link_entry,
            &auto_link_entity_list,
            target_db,
            named_project.as_deref(),
            sample_receipt,
        );
        let Some(mut receipt) = receipt else {
            return;
        };
        receipt.spawn_queue_wait = spawn_queue_wait.unwrap_or(Duration::ZERO);
        on_complete(receipt);
    });
}

/// Spawn the background auto-link task for a freshly-saved `entry`. The save
/// handler returns `"auto_link": "pending"` without awaiting it
/// (handler.rs:261-262), so nothing downstream can observe the finished
/// counts — [`run_auto_linking`] produces the [`AutoLinkReceipt`] and this
/// function emits it via `tracing::info!` from inside the spawned task.
///
/// # Measured vs. unmeasured (tachi#1097 r4/r5/r6 codex review)
///
/// This section used to carry a per-field enumeration of which receipt
/// fields are covered by which test. That enumeration was wrong in three
/// consecutive revisions of this doc comment — first an overclaim, then,
/// while correcting the overclaim, an underclaim in the opposite direction
/// (falsely naming several counters as unasserted when tests in `mod tests`
/// below already covered them). Per #1097 r6 codex review it has been
/// removed rather than corrected a fourth time: a hand-maintained coverage
/// inventory in a doc comment is a claim that rots every time a test is
/// added, renamed, or moved. What is asserted against which field is not
/// duplicated here — read `mod tests` below, where it cannot drift from the
/// code it describes.
///
/// What follows is mechanism, not coverage bookkeeping — true regardless of
/// what tests get added later. The `tracing::info!` call at the foot of the
/// spawned task is the one step no test asserts. That is a value judgement,
/// not a blocker, and two things should be stated plainly rather than left
/// implied:
///
/// * Capturing it is technically available — contrary to what r2/r3 asserted
///   here. `tracing-subscriber` is already a production dependency of this
///   crate (`Cargo.toml:83`, default features on), and a scoped subscriber CAN
///   cross a `tokio::spawn` boundary via
///   `tracing::instrument::WithSubscriber::with_current_subscriber`
///   (tracing-0.1.44 `instrument.rs:136`/`:228`, whose own doc example is
///   `tokio::spawn(future.with_current_subscriber())`). Neither a new
///   dependency nor `set_global_default` would be required. What capturing it
///   would add is confirmation that the block below reads each field off the
///   `receipt` it was handed (rather than, say, a stale local) — a
///   plumbing check, not a check on any value new to this block.
/// * The log line renders each timer through `as_micros() as u64`, so a phase
///   whose real duration is under one microsecond prints as `0`. The tests
///   assert the `Duration` fields on the receipt, not these rendered integers
///   — a `0` in a log line is therefore not by itself evidence of a dead
///   timer.
pub(crate) fn spawn_auto_linking(
    server: &MemoryServer,
    entry: &MemoryEntry,
    target_db: DbScope,
    named_project: Option<String>,
) {
    spawn_and_run_auto_linking(server, entry, target_db, named_project, |receipt| {
        // Emit from inside the spawned task — the save handler returned
        // `"auto_link": "pending"` (handler.rs:261-262) long before this
        // point, so this is the only place the finished counts exist. Fields
        // are whitelisted per #1097 D5: no entity names (entities ARE the
        // search queries here), no memory content, no DB path. `entry_id`
        // arrives already redacted from `finalize_auto_link_receipt` (#1097 r1
        // codex review ①), so the raw — possibly caller-hostile — id cannot
        // reach telemetry through here.
        tracing::info!(
            entry_id = %receipt.entry_id,
            entity_count = receipt.entity_count,
            searches_executed = receipt.searches_executed,
            searches_failed = receipt.searches_failed,
            candidates_examined = receipt.candidates_examined,
            edges_attempted = receipt.edges_attempted,
            edges_written = receipt.edges_written,
            post_write_failures = receipt.post_write_failures,
            skipped_self_id = receipt.skipped_self_id,
            skipped_training_seed = receipt.skipped_training_seed,
            skipped_no_shared_entities = receipt.skipped_no_shared_entities,
            skipped_no_supersede_or_reinforce = receipt.skipped_no_supersede_or_reinforce,
            read_elapsed_us = receipt.read_elapsed.as_micros() as u64,
            pool_checkout_wait = ?receipt.pool_checkout_wait,
            write_elapsed_us = receipt.write_elapsed.as_micros() as u64,
            spawn_queue_wait_us = receipt.spawn_queue_wait.as_micros() as u64,
            total_elapsed_us = receipt.total_elapsed.as_micros() as u64,
            "auto_link phase receipt (tachi#1097 S1, tachi#1145 spawn/pool visibility)"
        );
    });
}

/// Test-only correlation-token seam (tachi#1145). `AutoLinkReceipt::entry_id`
/// is always the fixed [`REDACTED_ENTRY_ID`] marker (#1097 r1 codex review ①)
/// and stays that way — this seam does NOT touch that redaction, does not
/// widen any visibility beyond `pub(crate)`, and does not exist in a
/// production binary (`#[cfg(test)]`). Under concurrent saves, a redacted
/// receipt alone cannot be paired back to the specific save that triggered
/// it; this gives a test an opaque, test-minted `correlation_token` — NEVER
/// the entry id, NEVER logged, NEVER visible to production — carried
/// alongside the receipt through a channel once the spawned task completes.
/// Delegates to [`spawn_and_run_auto_linking`], so it exercises the exact
/// same dedup, sampling gate, `tokio::spawn`, `run_auto_linking` call, and
/// `spawn_queue_wait` timer as [`spawn_auto_linking`] — the only difference
/// is what happens to a finished receipt.
#[cfg(test)]
pub(crate) fn spawn_auto_linking_for_test(
    server: &MemoryServer,
    entry: &MemoryEntry,
    target_db: DbScope,
    named_project: Option<String>,
    correlation_token: u64,
    on_complete: std::sync::mpsc::Sender<(u64, AutoLinkReceipt)>,
) {
    spawn_and_run_auto_linking(server, entry, target_db, named_project, move |receipt| {
        let _ = on_complete.send((correlation_token, receipt));
    });
}

fn commit_auto_link_supersession(
    store: &mut MemoryStore,
    edge: &memcore::MemoryEdge,
    source_snapshot: &MemoryEntry,
    target_snapshot: &MemoryEntry,
) -> Result<SupersessionCommitResult, String> {
    let expected =
        SupersessionExpectedState::active_unsuperseded(source_snapshot, Some(target_snapshot));
    store
        .with_immutable_supersession_transaction(|replacement| {
            let claim_result = replacement.claim_checked_immutable_supersession(
                &source_snapshot.id,
                &target_snapshot.id,
                &expected,
                AUTO_LINK_SUPERSESSION_ROUTE,
                AUTO_LINK_SUPERSESSION_POLICY_VERSION,
                true,
            )?;
            if claim_result == SupersessionCommitResult::Applied {
                replacement.add_canonical_supersession_edge(
                    edge,
                    &memcore::db::EdgeProvenance {
                        authority: Some(memcore::db::EdgeAuthority::DerivedHeuristic),
                        ..Default::default()
                    },
                )?;
            }
            Ok(claim_result)
        })
        .map_err(|e| e.to_string())
}

/// Run one auto-link pass to completion, optionally returning a finalized
/// [`AutoLinkReceipt`].
///
/// This is the entire body of the task [`spawn_auto_linking`] spawns, lifted
/// out of the `tokio::spawn` closure (#1097 r4 codex review ③) so the receipt
/// — including its `read_elapsed` / `write_elapsed` / `total_elapsed` timers —
/// is directly assertable from a test instead of being filed as a gap.
///
/// It is deliberately NOT `async`: this path contains no `.await`. Every store
/// call it makes is synchronous (`server_methods/db.rs:69`/`:280`), so the
/// blocking work is exactly what the spawned task already did — just reachable
/// without a runtime. [`spawn_auto_linking`]'s signature is unchanged, so its
/// only caller (`save_memory/handler.rs:261`) is untouched.
///
/// Pure observation (#1097 S1): the four skip points (self-id, training-seed,
/// no-shared-entities, neither-supersede-nor-reinforce) and the edge write are
/// not changed — only counted and timed when `sample` is true. `entry_id`
/// redaction and the `total_elapsed` stamp are delegated to
/// [`finalize_auto_link_receipt`].
///
/// This is the only production loop for both sampling modes. With `sample`
/// false it constructs no receipt, every timer start is guarded by
/// `sample.then(Instant::now)`, and the caller receives `None`, so it emits no
/// receipt log. The shared plumbing still performs `Option` checks around
/// instrumentation sites, so this is deliberately not described as a
/// zero-overhead path.
///
/// `entity_list` is the caller's already-deduplicated entity set;
/// [`spawn_auto_linking`] does that dedup so duplicate labels cannot inflate
/// `shared_count` past the #773 fog floor.
pub(crate) fn run_auto_linking(
    server: &MemoryServer,
    entry: &MemoryEntry,
    entity_list: &[String],
    target_db: DbScope,
    named_project: Option<&str>,
    sample: bool,
) -> Option<AutoLinkReceipt> {
    // #1097 S1 phase-attribution receipt (pure observation — no behavioral
    // change to the four skip points or the edge write). The receipt is
    // accumulated in place; the outcome→counter mapping for searches and edge
    // writes is routed THROUGH `AutoLinkReceipt::apply_search_outcome` /
    // `apply_edge_outcome` so the #1097 r1 codex review ③ semantics
    // (attempts-vs-executions, insert-landed-vs-full-success) are the TESTED
    // contract, not ad-hoc inline arithmetic — reverting either method turns
    // its discrimination test red and this call site follows.
    //
    // #1097 r1 codex review ①: `entry_id` is filled in at the end via
    // `finalize_auto_link_receipt` so the raw (possibly caller-hostile) id is
    // never stored on the receipt before redaction.
    let task_start = sample.then(Instant::now);
    // tachi#1435 slice 5 / #2059 codex round 3, tooth A (BUG fix): auto-link's
    // supersede write (`mark_superseded_closing_validity` below) closes an
    // old memory's validity the same way a contradiction-detection supersede
    // does — it changes what a subsequent search surfaces, so a stale cached
    // search result showing the now-superseded row must not survive it.
    // Tracked once across this whole run (both loops below, not per edge)
    // and invalidated once at the end, gated on the row having ACTUALLY been
    // closed (`mark_superseded_closing_validity`'s affected-row count > 0) —
    // re-superseding an already-superseded row is a no-op that changed
    // nothing search-visible and must not pay for a cache bust.
    let mut any_superseded = false;
    let mut receipt = sample.then(|| AutoLinkReceipt {
        entry_id: String::new(),
        entity_count: entity_list.len(),
        searches_executed: 0,
        searches_failed: 0,
        candidates_examined: 0,
        edges_attempted: 0,
        edges_written: 0,
        post_write_failures: 0,
        skipped_self_id: 0,
        skipped_training_seed: 0,
        skipped_no_shared_entities: 0,
        skipped_no_supersede_or_reinforce: 0,
        read_elapsed: Duration::ZERO,
        pool_checkout_wait: LayerAvailability::NotSampled,
        write_elapsed: Duration::ZERO,
        spawn_queue_wait: Duration::ZERO,
        total_elapsed: Duration::ZERO,
    });

    for entity in entity_list {
        let query = entity.clone();
        let search_action = |store: &mut MemoryStore| {
            store
                .search(
                    &query,
                    Some(memcore::SearchOptions {
                        top_k: 5,
                        // Auto-link is a write-side side effect that probes related memories.
                        // It must not bias ACT-R access stats for entries the user never read.
                        record_access: false,
                        ..Default::default()
                    }),
                )
                .map_err(|e| e.to_string())
        };

        let read_timer = sample.then(Instant::now);
        // tachi#1145: separate the pool-checkout-wait PORTION of this read
        // from its total wall time (still bracketed above by `read_timer`,
        // unchanged). Only the `DbScope::Global` + no-named-project path has
        // a recording read call today (`with_global_store_read_recording`,
        // #1125) — every other path keeps calling the plain read exactly as
        // before (zero behavior/perf change there) and honestly reports
        // `Unavailable` rather than guessing.
        let (search_res, pool_checkout_wait) =
            if sample && named_project.is_none() && matches!(target_db, DbScope::Global) {
                match server.with_global_store_read_recording(search_action) {
                    Ok((value, pool_receipt)) => (
                        Ok(value),
                        LayerAvailability::Measured(pool_receipt.pool_checkout_wait),
                    ),
                    Err(e) => (Err(e), LayerAvailability::Unavailable),
                }
            } else {
                let result = if let Some(p) = named_project {
                    server.with_named_project_store_read(p, search_action)
                } else {
                    server.with_store_for_scope_read(target_db, search_action)
                };
                let availability = if sample {
                    LayerAvailability::Unavailable
                } else {
                    LayerAvailability::NotSampled
                };
                (result, availability)
            };
        if let (Some(receipt), Some(read_timer)) = (receipt.as_mut(), read_timer) {
            receipt.read_elapsed += read_timer.elapsed();
            // Accumulate across the per-entity loop exactly like
            // `read_elapsed` does above — one entity list can drive several
            // searches, each with its own pool checkout. The branch this
            // block is reached from is loop-invariant (`sample`,
            // `named_project`, `target_db` never change mid-loop), but the
            // Ok/Err OUTCOME of each individual recording call is NOT
            // loop-invariant — one entity's read can error while another's
            // succeeds. tachi#1185 fix-round (checkpoint 4): route through
            // `accumulate_pool_checkout_wait`, which keeps `Unavailable`
            // sticky across that mix instead of letting a later `Measured`
            // entity re-promote an earlier failed-to-measure entity's
            // classification.
            receipt.pool_checkout_wait =
                accumulate_pool_checkout_wait(receipt.pool_checkout_wait, pool_checkout_wait);
        }
        // #1097 r1 codex review ③-A: count ONLY searches that produced
        // a result set. A named-project resolution failure
        // (server_methods/db.rs:280) returns Err WITHOUT ever invoking
        // the store's `search` call, so it is a FAILED attempt, not an
        // executed one — previously it inflated `searches_executed`.
        if let Some(receipt) = receipt.as_mut() {
            let search_outcome = match &search_res {
                Ok(_) => SearchOutcome::Executed,
                Err(_) => SearchOutcome::Failed,
            };
            receipt.apply_search_outcome(search_outcome);
        }

        if let Ok(results) = search_res {
            for result in results {
                if let Some(receipt) = receipt.as_mut() {
                    receipt.candidates_examined += 1;
                }
                if result.entry.id == entry.id {
                    if let Some(receipt) = receipt.as_mut() {
                        receipt.skipped_self_id += 1;
                    }
                    continue;
                }
                if is_training_seed(&result.entry) {
                    if let Some(receipt) = receipt.as_mut() {
                        receipt.skipped_training_seed += 1;
                    }
                    continue;
                }
                // Unique shared entities only — duplicate entity labels must not
                // count as multi-entity agreement for related_to/supersede.
                let shared = unique_shared_entities(entity_list, &result.entry.entities);
                if shared.is_empty() {
                    if let Some(receipt) = receipt.as_mut() {
                        receipt.skipped_no_shared_entities += 1;
                    }
                    continue;
                }

                let now = memcore::now_utc_iso();
                let vector_similarity = vector_similarity_between(entry, &result.entry);
                let supersedes =
                    should_supersede(entry, &result.entry, shared.len(), result.score.symbolic);
                let reinforces = vector_similarity.is_some_and(|similarity| {
                    should_reinforce(entry, &result.entry, shared.len(), similarity, supersedes)
                });
                if !supersedes && !reinforces {
                    // tachi#773 item 2: auto_link no longer emits `related_to` at
                    // all. Entity co-occurrence without a supersede/reinforce
                    // signal is query-time recoverable (shared-entity search)
                    // and isn't worth a persisted fog edge — and the memcore
                    // edge-write choke point (relation_ontology) would reject
                    // `related_to` on new writes anyway (item 1).
                    if let Some(receipt) = receipt.as_mut() {
                        receipt.skipped_no_supersede_or_reinforce += 1;
                    }
                    continue;
                }
                // #1097 r1 codex review ③-B: classify the write outcome inside
                // the closure so the receipt can distinguish "edge landed" from
                // "full success". Supersede writes no longer use an edge
                // savepoint followed by a separate lifecycle UPDATE: edge,
                // checked immutable claim, lifecycle close, and durable
                // supersession receipt share one transaction. A failure at any
                // point therefore lands no edge, and a same-edge semantic replay
                // is a typed no-write result.
                let mut edge_outcome = EdgeWriteOutcome::NoWrite;
                let mut superseded_rows: usize = 0;
                let save_edge_action = |store: &mut MemoryStore| -> Result<(), String> {
                    // `entry` is the save handler's pre-persistence value.
                    // Re-read the admitted target on the write store, then
                    // recompute every decision and edge field from that exact
                    // committed row. The outer calculation is only a cheap
                    // candidate prefilter; it grants no write authority.
                    let committed_target = store
                        .get(&entry.id)
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| format!("auto-link target disappeared: {}", entry.id))?;
                    let committed_source = store
                        .get(&result.entry.id)
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| {
                            format!("auto-link source disappeared: {}", result.entry.id)
                        })?;
                    let committed_shared = unique_shared_entities(
                        &committed_target.entities,
                        &committed_source.entities,
                    );
                    let committed_similarity =
                        vector_similarity_between(&committed_target, &committed_source);
                    let committed_symbolic = memcore::scorer::symbolic_score(
                        &query,
                        &committed_source.text,
                        &committed_source.keywords,
                        &committed_source.entities,
                    );
                    let committed_supersedes = should_supersede(
                        &committed_target,
                        &committed_source,
                        committed_shared.len(),
                        committed_symbolic,
                    );
                    let committed_reinforces = committed_similarity.is_some_and(|similarity| {
                        should_reinforce(
                            &committed_target,
                            &committed_source,
                            committed_shared.len(),
                            similarity,
                            committed_supersedes,
                        )
                    });
                    if !committed_supersedes && !committed_reinforces {
                        return Ok(());
                    }
                    let relation = if committed_supersedes {
                        "supersedes"
                    } else {
                        "reinforces"
                    };
                    let weight = if committed_supersedes {
                        0.9
                    } else {
                        committed_similarity.unwrap_or(0.0)
                    };
                    let committed_edge = memcore::MemoryEdge {
                        source_id: committed_target.id.clone(),
                        target_id: committed_source.id.clone(),
                        relation: relation.to_string(),
                        weight,
                        metadata: json!({
                            "auto_link": true,
                            "shared_entities": committed_shared,
                            "similarity": committed_similarity,
                            "confidence_increment": committed_reinforces
                                .then(|| confidence_increment(weight)),
                        }),
                        created_at: now.clone(),
                        valid_from: String::new(),
                        // Edges are only closed/expired when supersession is explicitly reversed.
                        valid_to: None,
                    };
                    if committed_supersedes {
                        // tachi#1646: auto-link `supersedes` edges are a
                        // vector-similarity heuristic Tachi computed itself;
                        // the edge now commits only with the checked semantic
                        // claim in one transaction.
                        let claim_result = commit_auto_link_supersession(
                            store,
                            &committed_edge,
                            &committed_source,
                            &committed_target,
                        )?;
                        if claim_result == SupersessionCommitResult::Applied {
                            superseded_rows = 1;
                            edge_outcome = EdgeWriteOutcome::InsertAndPostWriteOk;
                        }
                    } else {
                        // tachi#1646: auto-link `reinforces` edges are a
                        // vector-similarity heuristic Tachi computed itself.
                        let source_superseded_by = store
                            .supersession_target(&committed_target.id)
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| {
                                format!("auto-link target disappeared: {}", committed_target.id)
                            })?;
                        let target_superseded_by = store
                            .supersession_target(&committed_source.id)
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| {
                                format!("auto-link source disappeared: {}", committed_source.id)
                            })?;
                        let committed = store
                            .commit_confidence_reinforcement(
                                &committed_edge,
                                confidence_increment(weight),
                                &now,
                                &ExpectedMemoryState::from_entry(
                                    &committed_target,
                                    source_superseded_by.as_deref(),
                                ),
                                &ExpectedMemoryState::from_entry(
                                    &committed_source,
                                    target_superseded_by.as_deref(),
                                ),
                            )
                            .map_err(|error| error.to_string())?;
                        if committed {
                            edge_outcome = EdgeWriteOutcome::InsertAndPostWriteOk;
                        }
                    }
                    Ok(())
                };
                let write_timer = sample.then(Instant::now);
                let _ = if let Some(p) = named_project {
                    server.with_named_project_store(p, save_edge_action)
                } else {
                    server.with_store_for_scope(target_db, save_edge_action)
                };
                if superseded_rows > 0 {
                    any_superseded = true;
                }
                if let (Some(receipt), Some(write_timer)) = (receipt.as_mut(), write_timer) {
                    receipt.write_elapsed += write_timer.elapsed();
                }
                // Outer Err = store resolution failed or the atomic write
                // refused. Either way no edge landed, so the attempt is counted
                // but neither `edges_written` nor `post_write_failures` moves.
                if let Some(receipt) = receipt.as_mut() {
                    receipt.apply_edge_outcome(edge_outcome);
                }
            }
        }
    }

    // tachi#1435 slice 5 / #2059 codex round 3, tooth A: run-granularity
    // recall-cache bust (not per edge — see `any_superseded`'s doc above),
    // sharing the same choke point + epoch bump as `save_memory`'s,
    // enrichment's, and contradiction's invalidation.
    if any_superseded {
        crate::memory_search_ops::invalidate_recall_cache_after_write(
            server,
            "auto_link_supersede",
        );
    }

    // #1097 r1 codex review ① + r4 ③: the post-loop finalization (redacted
    // `entry_id` + `total_elapsed`) is routed through
    // `finalize_auto_link_receipt` so it is the SAME tested contract the unit
    // tests assert against; the `task_start.elapsed()` argument is covered by
    // `run_auto_linking_reports_live_read_write_and_total_timers` (stub it to
    // `Duration::ZERO` and that test's `total_elapsed` assertion goes red).
    match (receipt, task_start) {
        (Some(receipt), Some(task_start)) => Some(finalize_auto_link_receipt(
            receipt,
            &entry.id,
            task_start.elapsed(),
        )),
        (None, None) => None,
        _ => unreachable!("receipt and timer sampling must stay paired"),
    }
}

#[cfg(test)]
mod tests;

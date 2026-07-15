use crate::memory_search_ops::confidence_reinforce::{
    apply_confidence_reinforcement, confidence_increment, vector_similarity_between,
};
use crate::{DbScope, MemoryServer};
use memcore::{MemoryEntry, MemoryStore};
use serde_json::json;
use std::collections::HashSet;
use std::time::{Duration, Instant};

const REINFORCEMENT_MIN_SIMILARITY: f64 = 0.75;
const REINFORCEMENT_DUPLICATE_SIMILARITY: f64 = 0.95;

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
// line. The `entry_id` field + log point now carry the raw id ONLY when it
// parses as a legal UUID (`uuid::Uuid::parse_str`); otherwise they carry
// the fixed [`REDACTED_NON_UUID_ID`] marker — never the raw string.
//
// The receipt is emitted from INSIDE the spawned task because the save
// handler returns `"auto_link": "pending"` (handler.rs:261-262) without
// awaiting it — the task is the only place the finished counts exist. The
// task's body is `run_auto_linking`, a plain synchronous function, so the
// receipt (timers included) is assertable from a test without a runtime;
// `spawn_auto_linking` is only the spawn + emit shell around it.
// ---------------------------------------------------------------------------

/// Fixed marker placed in [`AutoLinkReceipt::entry_id`] (and the log line)
/// when the underlying `entry.id` does not parse as a legal UUID. See the
/// #1097 r1 codex review ① note above — a caller can push arbitrary text
/// into `params.id`, so the receipt must never echo the raw string.
pub const REDACTED_NON_UUID_ID: &str = "<non-uuid-id>";

/// Return `id` verbatim when it parses as a legal UUID (`uuid::Uuid::parse_str`,
/// same validator the rest of the codebase uses — e.g. candidates.rs:147),
/// otherwise the fixed [`REDACTED_NON_UUID_ID`] marker. This is the single
/// choke point for the #1097 r1 codex review ① redaction: every place the
/// auto-link receipt or its log line touches `entry.id` routes through here,
/// so the raw (possibly caller-hostile) string can never reach telemetry.
///
/// Owned `String` return because the receipt field is `String`; this keeps
/// the redaction unit test free of lifetime gymnastics.
pub(crate) fn redacted_entry_id(id: &str) -> String {
    if uuid::Uuid::parse_str(id).is_ok() {
        id.to_string()
    } else {
        REDACTED_NON_UUID_ID.to_string()
    }
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
    /// review ① this is the RAW id only when it parses as a legal UUID;
    /// otherwise it is the fixed [`REDACTED_NON_UUID_ID`] marker — never
    /// the raw (possibly caller-hostile) string.
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
    /// (auto_link.rs:136/:138).
    pub read_elapsed: Duration,
    /// Wall time inside per-edge `with_*_store` write calls
    /// (auto_link.rs:231/:233).
    pub write_elapsed: Duration,
    /// Wall time of the whole spawned task end-to-end.
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
    /// Edge insert landed AND the post-insert update (supersede/reinforce)
    /// landed too.
    InsertAndPostWriteOk,
    /// Edge insert landed, post-insert update FAILED. The edge IS persisted
    /// (its savepoint released), so `edges_written` must still count it —
    /// but `post_write_failures` also increments so the partial write is
    /// visible instead of erased.
    InsertOkPostWriteFailed,
    /// Edge insert itself failed (ontology rejection, DB error). Nothing
    /// persisted. Neither `edges_written` nor `post_write_failures` moves.
    InsertFailed,
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
            EdgeWriteOutcome::InsertOkPostWriteFailed => {
                self.edges_written += 1;
                self.post_write_failures += 1;
            }
            EdgeWriteOutcome::InsertFailed => {}
        }
    }
}

/// Finalize an accumulated [`AutoLinkReceipt`]: apply the redacted
/// `entry_id` (#1097 r1 codex review ① — the raw id reaches the receipt
/// ONLY when it parses as a legal UUID, otherwise the fixed
/// [`REDACTED_NON_UUID_ID`] marker) and the end-to-end `total_elapsed`.
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
/// Spawn the background auto-link task for a freshly-saved `entry`. The save
/// handler returns `"auto_link": "pending"` without awaiting it
/// (handler.rs:261-262), so nothing downstream can observe the finished
/// counts — [`run_auto_linking`] produces the [`AutoLinkReceipt`] and this
/// function emits it via `tracing::info!` from inside the spawned task.
///
/// # Measured vs. unmeasured (tachi#1097 r4/r5 codex review ③/④)
///
/// What is asserted, precisely, so this section does not overstate its own
/// coverage (#1097 r5 codex review ⑤ flagged an earlier "every value...is
/// asserted" phrasing here as overstated — it conflated "the mutation logic
/// is unit-tested in isolation" with "every field's live-path value is
/// asserted end-to-end", which are not the same claim):
///
/// * Counter *mutation semantics* — [`AutoLinkReceipt::apply_search_outcome`]
///   / [`AutoLinkReceipt::apply_edge_outcome`] (the attempts-vs-executions and
///   insert-landed-vs-full-success contracts) — are unit-tested in isolation
///   against a bare receipt, independent of any store or runtime.
/// * Post-loop finalization (redacted `entry_id`, `total_elapsed` stamping) —
///   [`finalize_auto_link_receipt`] — is unit-tested the same way.
/// * The three production timers (`read_elapsed`, `write_elapsed`,
///   `total_elapsed`), plus `searches_executed`, `edges_attempted`,
///   `edges_written`, `entry_id` and `entity_count`, are asserted
///   END-TO-END against a live store by
///   `run_auto_linking_reports_live_read_write_and_total_timers`, which calls
///   [`run_auto_linking`] directly. Rounds r2/r3 of #1097 filed the three
///   timers as an accepted gap, on the premise that the `tokio::spawn`
///   boundary put them out of a test's reach and that the S2 workload
///   measurement would catch a dead one. Both halves were wrong (a prose
///   promise is not a mechanism, and a single dead timer does not produce an
///   all-zero phase); lifting the task body into [`run_auto_linking`] removes
///   the boundary, so the timers are asserted rather than documented.
/// * `candidates_examined`, `post_write_failures`, and the four
///   `skipped_*` counters are NOT individually asserted to a specific value
///   anywhere in this module's tests — their production increments run on
///   every call (the loop that ticks them is not conditional on anything
///   this leaf added), and other auto-link tests
///   (`save_policy/auto_link.rs`) exercise paths that would move them, but no
///   test here pins a value on any of the four. That is a real, narrower gap
///   than the three timers were; it is stated rather than folded into a
///   blanket "asserted" claim.
///
/// The `tracing::info!` call below is the one step no test asserts at all —
/// not the counters, not the timers, not the redaction. That is a value
/// judgement, not a blocker, and two things should be stated plainly rather
/// than left implied:
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
    if is_training_seed(entry) {
        return;
    }

    let auto_link_server = server.clone();
    let auto_link_entry = entry.clone();
    // Dedup source entities so duplicate labels cannot inflate shared_count
    // past the #773 related_to fog floor (Gemini #905 review).
    let auto_link_entities: HashSet<String> = entry.entities.iter().cloned().collect();
    let auto_link_entity_list: Vec<String> = auto_link_entities.iter().cloned().collect();

    tokio::spawn(async move {
        let receipt = run_auto_linking(
            &auto_link_server,
            &auto_link_entry,
            &auto_link_entity_list,
            target_db,
            named_project.as_deref(),
        );
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
            write_elapsed_us = receipt.write_elapsed.as_micros() as u64,
            total_elapsed_us = receipt.total_elapsed.as_micros() as u64,
            "auto_link phase receipt (tachi#1097 S1)"
        );
    });
}

/// Run one auto-link pass to completion and return its finalized
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
/// not changed — only counted and timed. `entry_id` redaction and the
/// `total_elapsed` stamp are delegated to [`finalize_auto_link_receipt`].
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
) -> AutoLinkReceipt {
    let auto_link_id = entry.id.clone();
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
    let task_start = Instant::now();
    let mut receipt = AutoLinkReceipt {
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
        write_elapsed: Duration::ZERO,
        total_elapsed: Duration::ZERO,
    };

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
                .map_err(|e| format!("{}", e))
        };

        let read_timer = Instant::now();
        let search_res = if let Some(p) = named_project {
            server.with_named_project_store_read(p, search_action)
        } else {
            server.with_store_for_scope_read(target_db, search_action)
        };
        receipt.read_elapsed += read_timer.elapsed();
        // #1097 r1 codex review ③-A: count ONLY searches that produced
        // a result set. A named-project resolution failure
        // (server_methods/db.rs:280) returns Err WITHOUT ever invoking
        // the store's `search` call, so it is a FAILED attempt, not an
        // executed one — previously it inflated `searches_executed`.
        let search_outcome = match &search_res {
            Ok(_) => SearchOutcome::Executed,
            Err(_) => SearchOutcome::Failed,
        };
        receipt.apply_search_outcome(search_outcome);

        if let Ok(results) = search_res {
            for result in results {
                receipt.candidates_examined += 1;
                if result.entry.id == auto_link_id {
                    receipt.skipped_self_id += 1;
                    continue;
                }
                if is_training_seed(&result.entry) {
                    receipt.skipped_training_seed += 1;
                    continue;
                }
                // Unique shared entities only — duplicate entity labels must not
                // count as multi-entity agreement for related_to/supersede.
                let shared = unique_shared_entities(entity_list, &result.entry.entities);
                if shared.is_empty() {
                    receipt.skipped_no_shared_entities += 1;
                    continue;
                }

                let now = chrono::Utc::now().to_rfc3339();
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
                    receipt.skipped_no_supersede_or_reinforce += 1;
                    continue;
                }
                let relation = if supersedes {
                    "supersedes"
                } else {
                    "reinforces"
                };
                let weight = if supersedes {
                    0.9
                } else {
                    vector_similarity.unwrap_or(0.0)
                };
                let edge = memcore::MemoryEdge {
                    source_id: auto_link_id.clone(),
                    target_id: result.entry.id.clone(),
                    relation: relation.to_string(),
                    weight,
                    metadata: json!({
                        "auto_link": true,
                        "shared_entities": shared,
                        "similarity": vector_similarity,
                        "confidence_increment": reinforces.then(|| confidence_increment(weight)),
                    }),
                    created_at: now.clone(),
                    valid_from: String::new(),
                    // Edges are only closed/expired when supersession is explicitly reversed.
                    valid_to: None,
                };
                // #1097 r1 codex review ③-B: classify the write outcome
                // INSIDE the closure so the receipt can distinguish
                // "insert landed" (edge persisted under its own
                // savepoint at db/graph.rs:144, RELEASEd at :158) from
                // "full success". The closure returns Ok(classification);
                // an outer Err means store resolution failed and the
                // closure never ran (insert never attempted) — folded
                // to `InsertFailed` below since both leave nothing
                // persisted.
                let save_edge_action =
                    |store: &mut MemoryStore| -> Result<EdgeWriteOutcome, String> {
                        if store.add_edge(&edge).is_err() {
                            return Ok(EdgeWriteOutcome::InsertFailed);
                        }
                        let post_ok = if supersedes {
                            store
                                .mark_superseded_closing_validity(
                                    &result.entry.id,
                                    &auto_link_id,
                                    &now,
                                )
                                .is_ok()
                        } else if reinforces {
                            apply_confidence_reinforcement(
                                store,
                                &result.entry.id,
                                confidence_increment(weight),
                                &now,
                            )
                            .is_ok()
                        } else {
                            true
                        };
                        Ok(if post_ok {
                            EdgeWriteOutcome::InsertAndPostWriteOk
                        } else {
                            EdgeWriteOutcome::InsertOkPostWriteFailed
                        })
                    };
                let write_timer = Instant::now();
                let write_res = if let Some(p) = named_project {
                    server.with_named_project_store(p, save_edge_action)
                } else {
                    server.with_store_for_scope(target_db, save_edge_action)
                };
                receipt.write_elapsed += write_timer.elapsed();
                // Outer Err = store resolution failed (closure never ran,
                // nothing persisted); fold to `InsertFailed` so the
                // attempt is counted but neither `edges_written` nor
                // `post_write_failures` moves.
                let edge_outcome = write_res.unwrap_or(EdgeWriteOutcome::InsertFailed);
                receipt.apply_edge_outcome(edge_outcome);
            }
        }
    }
    // #1097 r1 codex review ① + r4 ③: the post-loop finalization (redacted
    // `entry_id` + `total_elapsed`) is routed through
    // `finalize_auto_link_receipt` so it is the SAME tested contract the unit
    // tests assert against; the `task_start.elapsed()` argument is covered by
    // `run_auto_linking_reports_live_read_write_and_total_timers` (stub it to
    // `Duration::ZERO` and that test's `total_elapsed` assertion goes red).
    finalize_auto_link_receipt(receipt, &auto_link_id, task_start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            path: "/test".into(),
            summary: text[..text.len().min(30)].into(),
            text: text.into(),
            importance: 0.7,
            timestamp: chrono::Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: "".into(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".into(),
            source: "test".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn path_root_extracts_first_segment() {
        assert_eq!(path_root("/project/alpha"), "project");
        assert_eq!(path_root("/wiki/entry"), "wiki");
        assert_eq!(path_root("no-slash"), "no-slash");
        assert_eq!(path_root("/"), "");
    }

    #[test]
    fn is_training_seed_detects_sft_boundaries() {
        let mut by_path = test_entry("sft-path", "training sample");
        by_path.path = "/sft/v4/strict/engineering/1".to_string();
        assert!(is_training_seed(&by_path));

        let mut by_source = test_entry("sft-source", "training sample");
        by_source.source = "sft_seed".to_string();
        assert!(is_training_seed(&by_source));

        let mut by_metadata = test_entry("sft-metadata", "training sample");
        by_metadata.metadata = json!({"training_sample": true});
        assert!(is_training_seed(&by_metadata));

        let live = test_entry("live", "current operational memory");
        assert!(!is_training_seed(&live));
    }

    #[test]
    fn is_newer_than_compares_timestamps() {
        assert!(is_newer_than(
            "2025-01-02T00:00:00Z",
            "2025-01-01T00:00:00Z"
        ));
        assert!(!is_newer_than(
            "2025-01-01T00:00:00Z",
            "2025-01-02T00:00:00Z"
        ));
    }

    #[test]
    fn should_supersede_rejects_cross_path_empty_topic_facts() {
        let mut new_entry = test_entry("new", "release prep summary mentions cleanup");
        let mut old_entry = test_entry("old", "clean-cli dry-run implementation fact");
        new_entry.path = "/scratch/tachi/v1.5-release-prep".to_string();
        old_entry.path = "/scratch/tachi/clean-cli-integration".to_string();
        new_entry.timestamp = "2026-06-06T18:53:45Z".to_string();
        old_entry.timestamp = "2026-06-06T18:47:28Z".to_string();
        new_entry.entities = vec!["Sigil".to_string(), "tachi-server".to_string()];
        old_entry.entities = new_entry.entities.clone();

        assert!(!should_supersede(&new_entry, &old_entry, 2, 1.0));
    }

    #[test]
    fn should_supersede_allows_same_path_or_non_empty_topic() {
        let mut new_entry = test_entry("new", "new canonical fact");
        let mut old_entry = test_entry("old", "old canonical fact");
        new_entry.timestamp = "2026-06-06T18:53:45Z".to_string();
        old_entry.timestamp = "2026-06-06T18:47:28Z".to_string();
        new_entry.path = "/scratch/tachi/same".to_string();
        old_entry.path = "/scratch/tachi/same".to_string();
        assert!(should_supersede(&new_entry, &old_entry, 2, 0.0));

        new_entry.path = "/scratch/tachi/new".to_string();
        old_entry.path = "/scratch/tachi/old".to_string();
        new_entry.topic = "release-fact".to_string();
        old_entry.topic = "release-fact".to_string();
        assert!(should_supersede(&new_entry, &old_entry, 2, 0.0));
    }

    #[test]
    fn should_reinforce_requires_vector_similarity_gray_zone() {
        let mut new_entry = test_entry("new", "canonical preference");
        let mut old_entry = test_entry("old", "nearby preference");
        new_entry.path = "/project/a".to_string();
        old_entry.path = "/project/b".to_string();
        new_entry.category = "preference".to_string();
        old_entry.category = "preference".to_string();

        assert!(should_reinforce(&new_entry, &old_entry, 1, 0.82, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 0, 0.82, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 1, 0.60, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 1, 0.97, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 1, 0.82, true));
    }

    #[test]
    fn unique_shared_entities_dedups_duplicate_labels() {
        // Discrimination: ["sigil","sigil"] ∩ ["sigil"] must count as 1,
        // not 2 — otherwise the fog floor is bypassed by noisy entity lists.
        let a = vec!["sigil".into(), "sigil".into()];
        let b = vec!["sigil".into(), "sigil".into(), "sigil".into()];
        let shared = unique_shared_entities(&a, &b);
        assert_eq!(shared.len(), 1);

        let a2 = vec!["sigil".into(), "tachi-server".into(), "sigil".into()];
        let b2 = vec!["tachi-server".into(), "sigil".into()];
        let shared2 = unique_shared_entities(&a2, &b2);
        assert_eq!(shared2.len(), 2);
    }

    /// tachi#773 item 2: auto_link must never select `related_to` as the
    /// emitted relation string. This mirrors [`run_auto_linking`]'s decision
    /// logic (supersedes -> "supersedes", reinforces -> "reinforces",
    /// otherwise -> skip entirely) without the store plumbing — it opens no
    /// DB and issues no query (that is the mechanism; how long it takes is not
    /// claimed or measured here). Red pre-#773-item-2: this same shape of
    /// decision used to fall through to `Some("related_to")` whenever
    /// entities were shared without a supersede/reinforce signal.
    ///
    /// Being a mirror, it can drift from the real decision logic; the
    /// authoritative check that a live pass emits `supersedes` is
    /// `run_auto_linking_reports_live_read_write_and_total_timers`.
    fn auto_link_emitted_relation(supersedes: bool, reinforces: bool) -> Option<&'static str> {
        if supersedes {
            Some("supersedes")
        } else if reinforces {
            Some("reinforces")
        } else {
            None
        }
    }

    #[test]
    fn auto_link_never_emits_related_to() {
        for supersedes in [true, false] {
            for reinforces in [true, false] {
                let relation = auto_link_emitted_relation(supersedes, reinforces);
                assert_ne!(
                    relation,
                    Some("related_to"),
                    "auto_link must never select related_to (supersedes={supersedes}, reinforces={reinforces})"
                );
            }
        }
        // Neither signal fires -> no edge at all (query-time recoverable via
        // shared-entity search instead of a persisted fog edge).
        assert_eq!(auto_link_emitted_relation(false, false), None);
    }

    // -----------------------------------------------------------------------
    // #1097 r1 codex review ① — entry_id redaction. `params.id` is
    // caller-controlled and unconstrained (handler.rs:46-49 only generates a
    // UUID when MISSING; wire field has no content constraint; validation
    // only checks presence). A caller can push query text / memory content /
    // entity names / credentials into `id`, and the un-redacted value used
    // to flow straight into the receipt + tracing line. These tests lock the
    // choke point (`redacted_entry_id`): the raw id reaches the receipt ONLY
    // when it parses as a legal UUID; otherwise the fixed marker.
    // -----------------------------------------------------------------------

    #[test]
    fn redacted_entry_id_passes_a_legal_uuid_verbatim() {
        let legal = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(redacted_entry_id(legal), legal);
        // Also accept uppercase hex — `uuid::Uuid::parse_str` is case-insensitive.
        let legal_upper = "550E8400-E29B-41D4-A716-446655440000";
        assert_eq!(redacted_entry_id(legal_upper), legal_upper);
    }

    #[test]
    fn redacted_entry_id_replaces_non_uuid_input_with_the_marker() {
        // Not a UUID at all — the redaction must kick in.
        assert_eq!(redacted_entry_id("not-a-uuid"), REDACTED_NON_UUID_ID);
        // Almost-UUID but malformed (wrong group length) — must still redact.
        assert_eq!(
            redacted_entry_id("550e8400-e29b-41d4-a716"),
            REDACTED_NON_UUID_ID
        );
        // Empty string is not a UUID.
        assert_eq!(redacted_entry_id(""), REDACTED_NON_UUID_ID);
    }

    #[test]
    fn redacted_entry_id_never_echoes_caller_hostile_text() {
        // #1097 r1 codex review ①'s actual threat model: a caller pushing
        // query text / SQL / credentials into `params.id`. The redacted
        // receipt must NOT contain the raw string anywhere — assert neither
        // the marker equality NOR a substring match on the hostile text.
        let hostile = "select * from memories where secret='pwn'";
        let redacted = redacted_entry_id(hostile);
        assert_eq!(redacted, REDACTED_NON_UUID_ID);
        assert!(
            !redacted.contains(hostile),
            "redacted id must never echo the raw hostile input"
        );
        assert!(
            !redacted.contains("secret"),
            "redacted id must not leak any fragment of the hostile input"
        );

        // Entity-name-shaped and memory-content-shaped ids are the same risk
        // (entities ARE the search queries here, per the module D5 note) —
        // redaction is content-blind, so all non-UUID shapes collapse to
        // the marker.
        let entity_like = "Sigil";
        assert_eq!(redacted_entry_id(entity_like), REDACTED_NON_UUID_ID);
        let content_like = "the user prefers dark mode";
        let redacted2 = redacted_entry_id(content_like);
        assert_eq!(redacted2, REDACTED_NON_UUID_ID);
        assert!(
            !redacted2.contains("dark mode"),
            "memory-content-shaped id must be fully redacted"
        );
    }

    // -----------------------------------------------------------------------
    // #1097 r3 codex review ④ gap-2 — `finalize_auto_link_receipt` is the
    // pure, testable tail of `spawn_auto_linking`. It owns the two post-loop
    // assignments that used to live as bare lines inside the `tokio::spawn`
    // block (the redacted `entry_id` and the end-to-end `total_elapsed`),
    // which no unit test could reach. These tests feed it counter state +
    // an id and assert the redaction + counter-preservation contract
    // directly — reverting the redaction to echo the raw id, or dropping the
    // total-elapsed assignment, turns these red.
    // -----------------------------------------------------------------------

    #[test]
    fn finalize_auto_link_receipt_redacts_hostile_id_and_preserves_counters() {
        // A receipt as it would exist at the foot of the spawn block: counters
        // accumulated via apply_*_outcome, but entry_id still the placeholder
        // and total_elapsed still ZERO.
        let mut r = zeroed_receipt();
        r.entity_count = 4;
        r.searches_executed = 2;
        r.searches_failed = 1;
        r.candidates_examined = 5;
        r.edges_attempted = 3;
        r.edges_written = 2;
        r.post_write_failures = 1;
        r.skipped_self_id = 1;
        let total = Duration::from_micros(1234);
        // A caller-hostile id (the #1097 r1 ① threat model): query text /
        // SQL pushed into params.id.
        let hostile = "select * from memories where secret='pwn'";

        let finalized = finalize_auto_link_receipt(r, hostile, total);

        // Redaction: the marker, and the raw hostile text must not appear.
        assert_eq!(finalized.entry_id, REDACTED_NON_UUID_ID);
        assert!(
            !finalized.entry_id.contains(hostile),
            "finalized entry_id must not echo the raw hostile input"
        );
        assert!(
            !finalized.entry_id.contains("secret"),
            "finalized entry_id must not leak any fragment of the hostile input"
        );
        // Counter preservation: every accumulated field passes through
        // untouched.
        assert_eq!(finalized.entity_count, 4);
        assert_eq!(finalized.searches_executed, 2);
        assert_eq!(finalized.searches_failed, 1);
        assert_eq!(finalized.candidates_examined, 5);
        assert_eq!(finalized.edges_attempted, 3);
        assert_eq!(finalized.edges_written, 2);
        assert_eq!(finalized.post_write_failures, 1);
        assert_eq!(finalized.skipped_self_id, 1);
        // total_elapsed assignment: a bare `receipt` that left total_elapsed
        // at ZERO would fail this (revert the assignment → red).
        assert_eq!(
            finalized.total_elapsed, total,
            "finalize must stamp the end-to-end total_elapsed"
        );
    }

    #[test]
    fn finalize_auto_link_receipt_passes_legal_uuid_verbatim() {
        // A legal UUID is NOT redacted — the raw value is what a log reader
        // pairs with the save response (no trace id exists codebase-wide).
        let legal = "550e8400-e29b-41d4-a716-446655440000";
        let finalized =
            finalize_auto_link_receipt(zeroed_receipt(), legal, Duration::from_micros(10));
        assert_eq!(finalized.entry_id, legal);
        assert_eq!(finalized.total_elapsed, Duration::from_micros(10));
    }

    #[test]
    fn finalize_auto_link_receipt_entity_shaped_id_is_redacted() {
        // Entity-name-shaped and memory-content-shaped ids collapse to the
        // marker too (entities ARE the search queries here, per the module
        // D5 note) — redaction is content-blind.
        let finalized =
            finalize_auto_link_receipt(zeroed_receipt(), "Sigil", Duration::from_micros(10));
        assert_eq!(finalized.entry_id, REDACTED_NON_UUID_ID);
    }

    // -----------------------------------------------------------------------
    // #1097 r1 codex review ③ — honest auto-link counters. The
    // `apply_search_outcome` / `apply_edge_outcome` methods are the single
    // source of truth the spawn block routes through; these tests revert one
    // outcome classification at a time and assert the counters move per the
    // reviewed contract.
    // -----------------------------------------------------------------------

    fn zeroed_receipt() -> AutoLinkReceipt {
        AutoLinkReceipt {
            entry_id: REDACTED_NON_UUID_ID.to_string(),
            entity_count: 0,
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
            write_elapsed: Duration::ZERO,
            total_elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn apply_search_outcome_executed_increments_only_searches_executed() {
        let mut r = zeroed_receipt();
        r.apply_search_outcome(SearchOutcome::Executed);
        assert_eq!(r.searches_executed, 1);
        assert_eq!(
            r.searches_failed, 0,
            "a successful search must not tick the failure counter"
        );
    }

    #[test]
    fn apply_search_outcome_failed_increments_only_searches_failed() {
        // #1097 r1 codex review ③-A: a named-project resolution failure
        // (server_methods/db.rs:280) used to tick `searches_executed`
        // before the result was even checked. `SearchOutcome::Failed` now
        // routes it to `searches_failed` instead — reverting
        // `apply_search_outcome` to always-increment-`searches_executed`
        // turns this red.
        let mut r = zeroed_receipt();
        r.apply_search_outcome(SearchOutcome::Failed);
        assert_eq!(r.searches_failed, 1);
        assert_eq!(
            r.searches_executed, 0,
            "a failed search attempt must NOT count as executed"
        );
    }

    #[test]
    fn apply_edge_outcome_full_success_counts_one_written_no_failures() {
        let mut r = zeroed_receipt();
        r.apply_edge_outcome(EdgeWriteOutcome::InsertAndPostWriteOk);
        assert_eq!(r.edges_attempted, 1);
        assert_eq!(r.edges_written, 1);
        assert_eq!(r.post_write_failures, 0);
    }

    #[test]
    fn apply_edge_outcome_insert_landed_but_post_write_failed_still_counts_written() {
        // #1097 r1 codex review ③-B: the edge insert commits under its OWN
        // savepoint (db/graph.rs:144 RELEASEd at :158) BEFORE the
        // supersede/reinforce update runs. When the update fails, the edge
        // is ALREADY persisted — so `edges_written` must still count it,
        // AND `post_write_failures` must tick so the partial write is
        // visible. Reverting `apply_edge_outcome(InsertOkPostWriteFailed)`
        // to NOT tick `edges_written` (the round-1 bug) turns this red.
        let mut r = zeroed_receipt();
        r.apply_edge_outcome(EdgeWriteOutcome::InsertOkPostWriteFailed);
        assert_eq!(
            r.edges_attempted, 1,
            "the write was attempted (closure ran)"
        );
        assert_eq!(
            r.edges_written, 1,
            "insert landed → edge persisted → must count as written"
        );
        assert_eq!(
            r.post_write_failures, 1,
            "the post-insert update failed → must be counted separately"
        );
    }

    #[test]
    fn apply_edge_outcome_insert_failed_counts_attempt_only() {
        let mut r = zeroed_receipt();
        r.apply_edge_outcome(EdgeWriteOutcome::InsertFailed);
        assert_eq!(r.edges_attempted, 1);
        assert_eq!(
            r.edges_written, 0,
            "insert itself failed → nothing persisted → not written"
        );
        assert_eq!(
            r.post_write_failures, 0,
            "no partial write — insert never landed, so no post-write failure either"
        );
    }

    #[test]
    fn apply_edge_outcome_mix_sums_consistently() {
        // A small fold covering every outcome once: one full success, one
        // partial write, one insert failure. `edges_attempted` must equal
        // the number of outcomes; `edges_written` must equal full-success
        // PLUS partial-write (both persisted the edge); `post_write_failures`
        // must equal only the partial-write outcome.
        let mut r = zeroed_receipt();
        r.apply_edge_outcome(EdgeWriteOutcome::InsertAndPostWriteOk);
        r.apply_edge_outcome(EdgeWriteOutcome::InsertOkPostWriteFailed);
        r.apply_edge_outcome(EdgeWriteOutcome::InsertFailed);
        assert_eq!(r.edges_attempted, 3);
        assert_eq!(r.edges_written, 2);
        assert_eq!(r.post_write_failures, 1);
    }

    // -----------------------------------------------------------------------
    // #1097 r4 codex review ③ — the three production timers (`read_timer`,
    // `write_timer`, `task_start`) against a live store.
    //
    // Rounds r2/r3 filed these as an accepted gap and nominated the #1097 S2
    // workload measurement as their "discriminator of record". That was a
    // prose promise rather than a mechanism, and its supporting reasoning did
    // not hold up: a single dead timer does not make the auto-link phase read
    // all-zero (the other two still report), `write_elapsed == 0` is
    // *legitimate* whenever no edge is attempted, and the log line's
    // `as_micros() as u64` rendering prints a real sub-microsecond duration as
    // `0` regardless. Lifting the task body into `run_auto_linking` (a plain
    // sync fn) removes the `tokio::spawn` boundary that made the gap look
    // unavoidable, so the timers are asserted here instead.
    // -----------------------------------------------------------------------

    /// Drive a real auto-link pass against a live store and assert all three
    /// production timers are wired.
    ///
    /// Acceptance bar (#1097 r4 ③) — stub any ONE of the three assignments to
    /// `Duration::ZERO` and exactly the matching assertion below goes red:
    ///
    /// * `receipt.read_elapsed += read_timer.elapsed()` → the `read_elapsed`
    ///   assertion.
    /// * `receipt.write_elapsed += write_timer.elapsed()` → the
    ///   `write_elapsed` assertion.
    /// * the `task_start.elapsed()` argument at the
    ///   `finalize_auto_link_receipt` call site → the `total_elapsed`
    ///   assertion. (`finalize`'s own tests cannot catch this one: they prove
    ///   it stamps the total it is *given*.)
    ///
    /// On the `> Duration::ZERO` form: `Instant` is only documented as
    /// nondecreasing, so "executed ⟹ elapsed > 0" is NOT an API guarantee and
    /// is not claimed as one. It is a practical assertion, and it holds not
    /// because a specific duration is measured or claimed here, but because
    /// each of these three spans brackets real SQLite work — an FTS search,
    /// and an edge INSERT plus a supersede UPDATE — not a no-op. A zero
    /// therefore indicates a timer that never started, not a run that was
    /// too fast to measure. Absolute upper bounds are deliberately not
    /// asserted (they would be flaky); `>= ZERO` is deliberately not used (it
    /// is a tautology).
    #[test]
    fn run_auto_linking_reports_live_read_write_and_total_timers() {
        let server = crate::tests::make_server();

        // Seed a target that `should_supersede` will fire on, so an edge write
        // is actually attempted and `write_elapsed` has real work to bracket.
        // Its gate: both categories "fact" (test_entry's default), 2 shared
        // entities, same path (hence same path root), and an older timestamp
        // than the new entry.
        let seeded_id = format!("auto-link-timer-target-{}", uuid::Uuid::new_v4());
        let mut seeded = test_entry(
            &seeded_id,
            "Original notes about sigil tachi-server internals",
        );
        seeded.entities = vec!["sigil".to_string(), "tachi-server".to_string()];
        seeded.path = "/alpha".to_string();
        seeded.timestamp = "2026-01-01T00:00:00Z".to_string();
        server
            .with_global_store(|store| store.upsert(&seeded).map_err(|e| format!("seed: {e}")))
            .expect("seed entry");

        // The freshly-"saved" entry, mirroring what `save_memory` upserts
        // before it calls `spawn_auto_linking`.
        let fresh_id = uuid::Uuid::new_v4().to_string();
        let mut fresh = test_entry(&fresh_id, "New observation about sigil rotation");
        fresh.entities = vec!["sigil".to_string(), "tachi-server".to_string()];
        fresh.path = "/alpha".to_string();
        fresh.timestamp = "2026-01-02T00:00:00Z".to_string();
        server
            .with_global_store(|store| store.upsert(&fresh).map_err(|e| format!("save: {e}")))
            .expect("save entry");

        let receipt = run_auto_linking(&server, &fresh, &fresh.entities, DbScope::Global, None);

        // Preconditions. These are asserted first and separately from the
        // timers so that a scenario which silently stops producing an edge
        // reports itself as a broken fixture rather than masquerading as a
        // dead `write_timer`.
        assert_eq!(
            receipt.searches_executed, 2,
            "both entities must have been searched, got executed={} failed={}",
            receipt.searches_executed, receipt.searches_failed
        );
        assert!(
            receipt.edges_attempted >= 1,
            "fixture must trigger a supersede edge write or `write_elapsed` \
             would be legitimately zero; got attempted={} examined={} \
             skipped(self={}, seed={}, no_shared={}, no_signal={})",
            receipt.edges_attempted,
            receipt.candidates_examined,
            receipt.skipped_self_id,
            receipt.skipped_training_seed,
            receipt.skipped_no_shared_entities,
            receipt.skipped_no_supersede_or_reinforce
        );
        assert!(
            receipt.edges_written >= 1,
            "the supersede insert must have landed, got written={} post_write_failures={}",
            receipt.edges_written,
            receipt.post_write_failures
        );

        // Timer 1 — `read_timer` (brackets the per-entity store search).
        assert!(
            receipt.read_elapsed > Duration::ZERO,
            "read_elapsed must be > ZERO: {} searches executed against a live \
             store, so a zero means `read_timer` never started",
            receipt.searches_executed
        );
        // Timer 2 — `write_timer` (brackets the per-edge store write).
        assert!(
            receipt.write_elapsed > Duration::ZERO,
            "write_elapsed must be > ZERO: {} edge write(s) attempted against a \
             live store, so a zero means `write_timer` never started",
            receipt.edges_attempted
        );
        // Timer 3 — `task_start`, via the `finalize_auto_link_receipt`
        // argument. It spans both of the above, so it is also the containment
        // check: a `total_elapsed` smaller than a phase it encloses would mean
        // the timer is pointed at the wrong span.
        assert!(
            receipt.total_elapsed > Duration::ZERO,
            "total_elapsed must be > ZERO (the `task_start.elapsed()` argument \
             at the finalize call site is live)"
        );
        assert!(
            receipt.total_elapsed >= receipt.read_elapsed + receipt.write_elapsed,
            "total_elapsed ({:?}) must contain read ({:?}) + write ({:?}) — \
             both accumulate strictly inside the task_start..finalize window",
            receipt.total_elapsed,
            receipt.read_elapsed,
            receipt.write_elapsed
        );

        // The redaction choke point is live on this path too: `fresh_id` is a
        // legal UUID, so it passes through verbatim.
        assert_eq!(receipt.entry_id, fresh_id);
        assert_eq!(receipt.entity_count, 2);
    }
}

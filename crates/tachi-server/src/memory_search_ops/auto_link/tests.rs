use super::*;
use crate::test_support::EnvRestore;
use crate::tool_params::SearchMemoryParams;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;

/// tachi#1432: same canonical-shape idiom pinned in memcore's
/// `db/common.rs` (`canonical_shape()`/`assert_canonical`) — millisecond
/// precision, `Z` suffix, no numeric offset. Reused here as a
/// writer-output discriminator for this crate's `memcore::now_utc_iso()`
/// call site in `run_auto_linking`.
fn assert_canonical_timestamp_shape(ts: &str) {
    let canonical = regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$").unwrap();
    assert!(
        canonical.is_match(ts),
        "not canonical Millis+Z shape: {ts:?}"
    );
}

fn test_entry(id: &str, text: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: "/notes".into(),
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
    }
}

#[test]
fn auto_link_receipt_sampling_is_default_off_and_requires_explicit_opt_in() {
    assert!(!auto_link_receipt_sampling_enabled_from(None));
    assert!(!auto_link_receipt_sampling_enabled_from(Some(
        "false".to_string()
    )));
    assert!(!auto_link_receipt_sampling_enabled_from(Some(
        "0".to_string()
    )));
    assert!(auto_link_receipt_sampling_enabled_from(Some(
        "true".to_string()
    )));
    assert!(auto_link_receipt_sampling_enabled_from(Some(
        " YES ".to_string()
    )));
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
// choke point (`redacted_entry_id`): no caller-provided id may reach a
// receipt, including UUID-shaped input.
// -----------------------------------------------------------------------

#[test]
fn redacted_entry_id_redacts_legal_uuid_too() {
    let legal = "550e8400-e29b-41d4-a716-446655440000";
    assert_eq!(redacted_entry_id(legal), REDACTED_ENTRY_ID);
    let legal_upper = "550E8400-E29B-41D4-A716-446655440000";
    assert_eq!(redacted_entry_id(legal_upper), REDACTED_ENTRY_ID);
}

#[test]
fn redacted_entry_id_replaces_non_uuid_input_with_the_marker() {
    // Not a UUID at all — the redaction must kick in.
    assert_eq!(redacted_entry_id("not-a-uuid"), REDACTED_ENTRY_ID);
    // Almost-UUID but malformed (wrong group length) — must still redact.
    assert_eq!(
        redacted_entry_id("550e8400-e29b-41d4-a716"),
        REDACTED_ENTRY_ID
    );
    // Empty string is not a UUID.
    assert_eq!(redacted_entry_id(""), REDACTED_ENTRY_ID);
}

#[test]
fn redacted_entry_id_never_echoes_caller_hostile_text() {
    // #1097 r1 codex review ①'s actual threat model: a caller pushing
    // query text / SQL / credentials into `params.id`. The redacted
    // receipt must NOT contain the raw string anywhere — assert neither
    // the marker equality NOR a substring match on the hostile text.
    let hostile = "select * from memories where secret='pwn'";
    let redacted = redacted_entry_id(hostile);
    assert_eq!(redacted, REDACTED_ENTRY_ID);
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
    assert_eq!(redacted_entry_id(entity_like), REDACTED_ENTRY_ID);
    let content_like = "the user prefers dark mode";
    let redacted2 = redacted_entry_id(content_like);
    assert_eq!(redacted2, REDACTED_ENTRY_ID);
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
    assert_eq!(finalized.entry_id, REDACTED_ENTRY_ID);
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
fn finalize_auto_link_receipt_redacts_legal_uuid_too() {
    let legal = "550e8400-e29b-41d4-a716-446655440000";
    let finalized = finalize_auto_link_receipt(zeroed_receipt(), legal, Duration::from_micros(10));
    assert_eq!(finalized.entry_id, REDACTED_ENTRY_ID);
    assert_eq!(finalized.total_elapsed, Duration::from_micros(10));
}

#[test]
fn finalize_auto_link_receipt_entity_shaped_id_is_redacted() {
    // Entity-name-shaped and memory-content-shaped ids collapse to the
    // marker too (entities ARE the search queries here, per the module
    // D5 note) — redaction is content-blind.
    let finalized =
        finalize_auto_link_receipt(zeroed_receipt(), "Sigil", Duration::from_micros(10));
    assert_eq!(finalized.entry_id, REDACTED_ENTRY_ID);
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
        entry_id: REDACTED_ENTRY_ID.to_string(),
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
        pool_checkout_wait: LayerAvailability::NotSampled,
        write_elapsed: Duration::ZERO,
        spawn_queue_wait: Duration::ZERO,
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
fn apply_edge_outcome_no_write_counts_attempt_only() {
    let mut r = zeroed_receipt();
    r.apply_edge_outcome(EdgeWriteOutcome::NoWrite);
    assert_eq!(r.edges_attempted, 1);
    assert_eq!(
        r.edges_written, 0,
        "no edge landed → nothing persisted → not written"
    );
    assert_eq!(
        r.post_write_failures, 0,
        "no partial write — the atomic operation landed nothing"
    );
}

#[test]
fn apply_edge_outcome_mix_sums_consistently() {
    // A small fold covering every outcome once: one full success, one
    // partial write, one no-write refusal. `edges_attempted` must equal
    // the number of outcomes; `edges_written` must equal full-success
    // PLUS partial-write (both persisted the edge); `post_write_failures`
    // must equal only the partial-write outcome.
    let mut r = zeroed_receipt();
    r.apply_edge_outcome(EdgeWriteOutcome::InsertAndPostWriteOk);
    r.apply_edge_outcome(EdgeWriteOutcome::InsertOkPostWriteFailed);
    r.apply_edge_outcome(EdgeWriteOutcome::NoWrite);
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

    let receipt = run_auto_linking(
        &server,
        &fresh,
        &fresh.entities,
        DbScope::Global,
        None,
        true,
    )
    .expect("sampled auto-link pass must return a receipt");

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

    // tachi#1432 CONCERN 3: the fixture above forces a real supersede, so
    // `mark_superseded_closing_validity` ran against `seeded_id` with the
    // `now = memcore::now_utc_iso()` stamp this call site (line ~732)
    // computes. Read the row back and pin its shape — reintroducing a
    // bare `Utc::now().to_rfc3339()` at that call site must RED here,
    // not just in the renderer's own unit test.
    let (updated_at, valid_until): (String, Option<String>) = server
        .with_global_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT updated_at, valid_until FROM memories WHERE id = ?1",
                    [&seeded_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|e| e.to_string())
        })
        .expect("read back superseded row");
    assert_canonical_timestamp_shape(&updated_at);
    assert_canonical_timestamp_shape(
        valid_until
            .as_deref()
            .expect("supersede must close valid_until"),
    );

    // Timer 1 — `read_timer` (brackets the per-entity store search).
    assert!(
        receipt.read_elapsed > Duration::ZERO,
        "read_elapsed must be > ZERO: {} searches executed against a live \
         store, so a zero means `read_timer` never started",
        receipt.searches_executed
    );
    // Timer 1b (tachi#1145) — `pool_checkout_wait` is the subset of
    // `read_elapsed` spent waiting for a global read-pool slot. This
    // call is `DbScope::Global` with no named project, the one path
    // wired to the recording read call, so it must be `Measured`, not
    // `Unavailable`/`NotSampled` — and it can never exceed the total
    // read wall time it is carved out of.
    match receipt.pool_checkout_wait {
        LayerAvailability::Measured(pool_wait) => {
            assert!(
                pool_wait <= receipt.read_elapsed,
                "pool_checkout_wait ({pool_wait:?}) must be <= read_elapsed \
                 ({:?}) — it is a SUBSET of the read wall time, not a \
                 separate span",
                receipt.read_elapsed
            );
        }
        other => panic!(
            "pool_checkout_wait must be Measured on the DbScope::Global + \
             no-named-project path (the recording read call is wired \
             there), got {other:?}"
        ),
    }
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

    // The redaction choke point is live on this path too: UUID syntax is
    // caller-controllable, so it is still never emitted.
    assert_eq!(receipt.entry_id, REDACTED_ENTRY_ID);
    assert_eq!(receipt.entity_count, 2);
}

#[test]
fn auto_link_recomputes_overlap_after_same_id_target_update() {
    let server = crate::tests::make_server();
    let path = format!("/auto-link-interleave-{}", uuid::Uuid::new_v4());
    let old_id = format!("auto-link-interleave-old-{}", uuid::Uuid::new_v4());
    let mut old = test_entry(&old_id, "older shared-entity observation");
    old.entities = vec!["stale-a".to_string(), "stale-b".to_string()];
    old.path = path.clone();
    old.timestamp = "2026-01-01T00:00:00Z".to_string();
    server
        .with_global_store(|store| store.upsert(&old).map_err(|error| error.to_string()))
        .expect("seed old candidate");

    let fresh_id = format!("auto-link-interleave-new-{}", uuid::Uuid::new_v4());
    let mut stale_fresh = test_entry(&fresh_id, "newer shared-entity observation");
    stale_fresh.entities = old.entities.clone();
    stale_fresh.path = path;
    stale_fresh.timestamp = "2026-01-02T00:00:00Z".to_string();
    server
        .with_global_store(|store| {
            store
                .upsert(&stale_fresh)
                .map_err(|error| error.to_string())
        })
        .expect("seed initial fresh target");

    // Simulate the async task having captured `stale_fresh` before a
    // same-ID patch commits a disjoint entity set.
    let mut committed_fresh = stale_fresh.clone();
    committed_fresh.entities = vec!["current-x".to_string(), "current-y".to_string()];
    server
        .with_global_store(|store| {
            store
                .upsert(&committed_fresh)
                .map_err(|error| error.to_string())
        })
        .expect("commit intervening same-id target update");

    let receipt = run_auto_linking(
        &server,
        &stale_fresh,
        &stale_fresh.entities,
        DbScope::Global,
        None,
        true,
    )
    .expect("sampled auto-link receipt");
    assert!(
        receipt.edges_attempted >= 1,
        "stale snapshot must reach the write discriminator"
    );
    assert_eq!(receipt.edges_written, 0);
    server
        .with_global_store(|store| {
            let superseded_by = store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = ?1",
                    [&old_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(|error| error.to_string())?;
            let stale_edges: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges
                     WHERE source_id = ?1 AND target_id = ?2 AND relation = 'supersedes'",
                    rusqlite::params![&fresh_id, &old_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(superseded_by, None);
            assert_eq!(stale_edges, 0);
            Ok(())
        })
        .expect("verify stale overlap left no lifecycle or edge write");
}

#[test]
fn auto_link_recomputes_reinforcement_after_same_id_target_update() {
    let server = crate::tests::make_server();
    let old_id = format!("auto-link-reinforce-old-{}", uuid::Uuid::new_v4());
    let mut old = test_entry(&old_id, "older reinforcement candidate");
    old.entities = vec!["stale-shared".to_string()];
    old.path = "/reinforce/old".to_string();
    old.timestamp = "2026-01-01T00:00:00Z".to_string();
    let mut old_vector = vec![0.0_f32; crate::memory_search_ops::EXPECTED_EMBEDDING_DIM];
    old_vector[0] = 1.0;
    old.vector = Some(old_vector);
    server
        .with_global_store(|store| store.upsert(&old).map_err(|error| error.to_string()))
        .expect("seed reinforcement candidate");

    let fresh_id = format!("auto-link-reinforce-new-{}", uuid::Uuid::new_v4());
    let mut stale_fresh = test_entry(&fresh_id, "captured reinforcement target");
    stale_fresh.entities = old.entities.clone();
    stale_fresh.path = "/reinforce/new".to_string();
    stale_fresh.timestamp = "2026-01-02T00:00:00Z".to_string();
    let mut fresh_vector = vec![0.0_f32; crate::memory_search_ops::EXPECTED_EMBEDDING_DIM];
    fresh_vector[0] = 0.8;
    fresh_vector[1] = 0.6;
    stale_fresh.vector = Some(fresh_vector);
    server
        .with_global_store(|store| {
            store
                .upsert(&stale_fresh)
                .map_err(|error| error.to_string())
        })
        .expect("seed captured reinforcement target");

    // The captured value qualifies for reinforcement. The committed
    // same-ID row no longer overlaps, so persisting the stale decision
    // would create both an edge and an unjustified confidence increment.
    let mut committed_fresh = stale_fresh.clone();
    committed_fresh.entities = vec!["current-disjoint".to_string()];
    server
        .with_global_store(|store| {
            store
                .upsert(&committed_fresh)
                .map_err(|error| error.to_string())
        })
        .expect("commit intervening same-id reinforcement update");

    let receipt = run_auto_linking(
        &server,
        &stale_fresh,
        &stale_fresh.entities,
        DbScope::Global,
        None,
        true,
    )
    .expect("sampled auto-link receipt");
    assert!(
        receipt.edges_attempted >= 1,
        "captured reinforcement must reach the write discriminator"
    );
    assert_eq!(receipt.edges_written, 0);
    server
        .with_global_store_read(|store| {
            let stale_edges: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges
                     WHERE source_id = ?1 AND target_id = ?2 AND relation = 'reinforces'",
                    rusqlite::params![&fresh_id, &old_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(stale_edges, 0);
            Ok(())
        })
        .expect("verify stale reinforcement left no edge write");
}

#[test]
fn auto_link_protected_supersession_refusal_leaves_no_edge_or_lifecycle_write() {
    let server = crate::tests::make_server();
    let path = format!("/auto-link-protected-{}", uuid::Uuid::new_v4());
    let old_id = format!("auto-link-protected-old-{}", uuid::Uuid::new_v4());
    let mut old = test_entry(&old_id, "older protected shared-entity observation");
    old.entities = vec!["protected-a".to_string(), "protected-b".to_string()];
    old.path = path.clone();
    old.timestamp = "2026-01-01T00:00:00Z".to_string();
    old.retention_policy = Some("durable".to_string());
    server
        .with_global_store(|store| store.upsert(&old).map_err(|e| e.to_string()))
        .expect("seed protected candidate");

    let fresh_id = format!("auto-link-protected-new-{}", uuid::Uuid::new_v4());
    let mut fresh = test_entry(&fresh_id, "newer shared-entity observation");
    fresh.entities = old.entities.clone();
    fresh.path = path;
    fresh.timestamp = "2026-01-02T00:00:00Z".to_string();
    server
        .with_global_store(|store| store.upsert(&fresh).map_err(|e| e.to_string()))
        .expect("seed fresh candidate");

    let receipt = run_auto_linking(
        &server,
        &fresh,
        &fresh.entities,
        DbScope::Global,
        None,
        true,
    )
    .expect("sampled auto-link receipt");
    assert!(
        receipt.edges_attempted >= 1,
        "fixture must reach the semantic supersession write: {receipt:?}"
    );
    assert_eq!(receipt.edges_written, 0);

    server
        .with_global_store_read(|store| {
            let source_state: (bool, Option<String>) = store
                .connection()
                .query_row(
                    "SELECT archived, superseded_by FROM memories WHERE id = ?1",
                    [&old_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            let edge_count: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges WHERE source_id = ?1 AND target_id = ?2",
                    [&fresh_id, &old_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(source_state, (false, None));
            assert_eq!(
                edge_count, 0,
                "edge and lifecycle close must roll back as one transaction"
            );
            Ok(())
        })
        .expect("verify protected auto-link refusal");
}

#[test]
fn auto_link_edge_failure_rolls_back_supersession_and_receipt() {
    let server = crate::tests::make_server();
    let path = format!("/auto-link-edge-failure-{}", uuid::Uuid::new_v4());
    let old_id = format!("auto-link-edge-failure-old-{}", uuid::Uuid::new_v4());
    let mut old = test_entry(&old_id, "older shared-entity observation");
    old.entities = vec!["rollback-a".to_string(), "rollback-b".to_string()];
    old.path = path.clone();
    old.timestamp = "2026-01-01T00:00:00Z".to_string();

    let fresh_id = format!("auto-link-edge-failure-new-{}", uuid::Uuid::new_v4());
    let mut fresh = test_entry(&fresh_id, "newer shared-entity observation");
    fresh.entities = old.entities.clone();
    fresh.path = path;
    fresh.timestamp = "2026-01-02T00:00:00Z".to_string();

    let error = server
        .with_global_store(|store| {
            store.upsert(&old).map_err(|e| e.to_string())?;
            store.upsert(&fresh).map_err(|e| e.to_string())?;
            let source_snapshot = store
                .get(&old_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "source snapshot missing".to_string())?;
            let target_snapshot = store
                .get(&fresh_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "target snapshot missing".to_string())?;
            let invalid_edge = memcore::MemoryEdge {
                source_id: fresh_id.clone(),
                target_id: old_id.clone(),
                relation: "invalid_test_relation".to_string(),
                weight: 0.9,
                metadata: json!({"auto_link": true}),
                created_at: memcore::now_utc_iso(),
                valid_from: String::new(),
                valid_to: None,
            };
            commit_auto_link_supersession(store, &invalid_edge, &source_snapshot, &target_snapshot)
                .map(|_| ())
        })
        .expect_err("dependent edge rejection must abort the semantic claim");
    assert!(
        error.contains("invalid_test_relation") || error.contains("relation"),
        "the failure must come from the dependent edge write: {error}"
    );

    server
        .with_global_store_read(|store| {
            let source_state: (bool, Option<String>) = store
                .connection()
                .query_row(
                    "SELECT archived, superseded_by FROM memories WHERE id = ?1",
                    [&old_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            let edge_count: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges WHERE source_id = ?1 AND target_id = ?2",
                    [&fresh_id, &old_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let receipt_id = memcore::SupersessionReceipt::id_for(
                AUTO_LINK_SUPERSESSION_ROUTE,
                AUTO_LINK_SUPERSESSION_POLICY_VERSION,
                &old_id,
                &fresh_id,
            );
            let durable_receipt = store
                .get_state_kv(memcore::SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
                .map_err(|e| e.to_string())?;
            assert_eq!(source_state, (false, None));
            assert_eq!(edge_count, 0);
            assert!(
                durable_receipt.is_none(),
                "the rolled-back semantic claim must not leave a receipt"
            );
            Ok(())
        })
        .expect("verify edge-failure rollback");
}

fn search_params_for(query: &str) -> SearchMemoryParams {
    SearchMemoryParams {
        query: query.to_string(),
        query_vec: None,
        top_k: 10,
        path_prefix: None,
        include_training: false,
        include_archived: false,
        candidates_per_channel: 20,
        mmr_threshold: None,
        graph_expand_hops: 0,
        graph_relation_filter: None,
        weights: None,
        context_symbols: Vec::new(),
        agent_role: None,
        project: None,
        domain: None,
        file_context: None,
        error_context: None,
        enable_rerank: false,
        conversation_scope: None,
        auto_conversation_scope: true,
        multi_evidence_pack: true,
        as_of: None,
        include_superseded: false,
        include_test: false,
        include_metadata: false,
        // Explicit JSON so assertions can index rows by id instead of
        // parsing the markdown digest (tachi#1201 k3 default).
        format: Some("json".to_string()),
    }
}

/// tachi#1435 slice 5 / #2059 codex round 3, "tooth A" (BUG, pre-fix RED):
/// auto-link's supersede write closes an old memory's validity the same
/// way a contradiction-detection supersede does (`memory_search_ops::
/// contradiction`) — a stale cached search result still showing the
/// now-superseded row must not survive it. Drives the exact same
/// `run_auto_linking` entry point `run_auto_linking_reports_live_read_write_and_total_timers`
/// above already exercises for its supersede fixture, reused here.
///
/// `git stash`/`if false`-gate this file's `any_superseded` tracking +
/// its `invalidate_recall_cache_after_write` call at the end of
/// `run_auto_linking` and rerun this single test to see it fail pre-fix
/// (the post-supersede search still returns the superseded row from the
/// cache warmed before the supersede ran).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn auto_link_supersede_busts_a_warm_recall_cache() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");

    let server = crate::tests::make_server();
    let needle = format!("AutoLinkSentinel{}", uuid::Uuid::new_v4().simple());
    let path = format!("/alpha-{}", uuid::Uuid::new_v4());

    // The row auto-link will supersede.
    let old_id = format!("old-{}", uuid::Uuid::new_v4());
    let mut old = test_entry(&old_id, &format!("{needle} original observation"));
    old.entities = vec!["EntA".to_string(), "EntB".to_string()];
    old.path = path.clone();
    old.timestamp = "2026-01-01T00:00:00Z".to_string();
    server
        .with_global_store(|store| store.upsert(&old).map_err(|e| format!("seed old: {e}")))
        .expect("seed old");

    // Warm the cache: pre-supersede, the sentinel query hits the old row.
    let first = server
        .search_memory(Parameters(search_params_for(&needle)))
        .await
        .expect("warm search");
    let first_rows: serde_json::Value = serde_json::from_str(&first).expect("warm search json");
    let first_rows = first_rows.as_array().expect("warm search rows array");
    assert!(
        first_rows.iter().any(|r| r["id"] == old_id),
        "old row must be visible pre-supersede: {first_rows:#?}"
    );

    // The freshly-"saved" entry, mirroring what `save_memory` upserts
    // before it triggers auto-link — same shared-entity/path/newer-
    // timestamp shape `run_auto_linking_reports_live_read_write_and_total_timers`
    // uses to force a supersede.
    let fresh_id = uuid::Uuid::new_v4().to_string();
    let mut fresh = test_entry(&fresh_id, &format!("{needle} newer observation"));
    fresh.entities = old.entities.clone();
    fresh.path = path;
    fresh.timestamp = "2026-01-02T00:00:00Z".to_string();
    server
        .with_global_store(|store| store.upsert(&fresh).map_err(|e| format!("save fresh: {e}")))
        .expect("save fresh");

    let receipt = run_auto_linking(
        &server,
        &fresh,
        &fresh.entities,
        DbScope::Global,
        None,
        true,
    )
    .expect("sampled auto-link pass must return a receipt");
    assert!(
        receipt.edges_written >= 1,
        "fixture must trigger a supersede edge write, got receipt={receipt:?}"
    );

    // Post-fix: the SAME query must no longer surface the superseded old
    // row — not because a fresh compute always excludes it (it does),
    // but because the very next search must not be served the
    // pre-supersede cached answer that still contains it.
    let second = server
        .search_memory(Parameters(search_params_for(&needle)))
        .await
        .expect("post-supersede search");
    let second_rows: serde_json::Value = serde_json::from_str(&second).expect("post search json");
    let second_rows = second_rows.as_array().expect("post search rows array");
    assert!(
        !second_rows.iter().any(|r| r["id"] == old_id),
        "the superseded row must NOT appear in the very next identical \
         search — it must not survive via the pre-supersede cached \
         answer: {second_rows:#?}"
    );
}

// -----------------------------------------------------------------------
// tachi#1145 — `pool_checkout_wait` honestly reports `Unavailable` (never
// a guessed zero) on every path that is NOT the one recording read call
// wired today. Companion to the `Measured` assertion inside
// `run_auto_linking_reports_live_read_write_and_total_timers` above,
// which covers the `DbScope::Global` + no-named-project path.
// -----------------------------------------------------------------------

/// A named-project read has no recording twin yet (#1125 cold review
/// scoped that twin to the global read pool only), so `pool_checkout_wait`
/// must be `Unavailable`, not a guessed zero — regardless of whether the
/// named project itself resolves. The classification branch is decided
/// purely by `named_project.is_some()` before any store call runs, so an
/// intentionally-nonexistent project name is a valid, deterministic
/// fixture: it forces `SearchOutcome::Failed`, which this test does not
/// care about, while still exercising the per-entity read-timer block
/// (one entity, so the loop body — and therefore this classification —
/// actually runs).
#[test]
fn pool_checkout_wait_is_unavailable_for_named_project_reads() {
    let server = crate::tests::make_server();
    let entry = test_entry("named-project-probe", "probe entry");
    let entities = vec!["probe-entity".to_string()];

    let receipt = run_auto_linking(
        &server,
        &entry,
        &entities,
        DbScope::Global,
        Some("definitely-not-a-real-project"),
        true,
    )
    .expect("sampled auto-link pass must return a receipt");

    assert_eq!(
        receipt.pool_checkout_wait,
        LayerAvailability::Unavailable,
        "named-project reads have no recording twin yet — this must be \
         Unavailable, never a guessed Measured(0) or NotSampled"
    );
}

/// `DbScope::Project` (no named project) is likewise not wired to the
/// recording read call — only `DbScope::Global` is. No project DB is
/// attached to this test server, so the read itself errors, but (as
/// above) the classification is decided before that outcome is known.
#[test]
fn pool_checkout_wait_is_unavailable_for_project_scope_reads() {
    let server = crate::tests::make_server();
    let entry = test_entry("project-scope-probe", "probe entry");
    let entities = vec!["probe-entity".to_string()];

    let receipt = run_auto_linking(&server, &entry, &entities, DbScope::Project, None, true)
        .expect("sampled auto-link pass must return a receipt");

    assert_eq!(
        receipt.pool_checkout_wait,
        LayerAvailability::Unavailable,
        "DbScope::Project reads have no recording twin yet — this must \
         be Unavailable, never a guessed Measured(0) or NotSampled"
    );
}

/// tachi#1185 fix-round (codex review checkpoint 8): the two
/// `Unavailable` tests above are both confounded — they trigger through
/// a search that also ERRORS (an unresolvable project name / a missing
/// project DB), so neither can distinguish "Unavailable by design,
/// because this path has no recording twin at all" from a hypothetical
/// buggy implementation that returns `Unavailable` only on error while
/// GUESSING `Measured(0)` on an unwired-but-successful read. This test
/// drives an actually SUCCESSFUL named-project search (a real, freshly
/// initialized project DB, an entity that legitimately returns zero
/// results rather than erroring) and still requires `Unavailable`.
#[test]
fn pool_checkout_wait_is_unavailable_for_successful_named_project_read() {
    crate::test_support::with_tachi_home(|home| {
        let project_db = home.join("projects/probe-project/memory.db");
        // Full schema init via the real constructor (same route
        // `tests/mod.rs`'s template-DB builder uses) — dropped
        // immediately after; `run_auto_linking` below reopens it
        // through the normal named-project resolution path.
        drop(MemoryServer::new(project_db, None).expect("init named-project db schema"));

        let server = crate::tests::make_server();
        let entry = test_entry("named-project-success-probe", "probe entry");
        let entities = vec!["probe-entity".to_string()];

        let receipt = run_auto_linking(
            &server,
            &entry,
            &entities,
            DbScope::Global,
            Some("probe-project"),
            true,
        )
        .expect("sampled auto-link pass must return a receipt");

        assert_eq!(
            receipt.pool_checkout_wait,
            LayerAvailability::Unavailable,
            "a SUCCESSFUL named-project read must still report \
             Unavailable — this path has no recording twin regardless \
             of whether the read itself succeeds or fails; a Measured \
             value here would mean the classification is secretly keyed \
             off Ok/Err rather than which path was actually taken"
        );
    });
}

// -----------------------------------------------------------------------
// tachi#1185 fix-round (codex review checkpoint 4, CRITICAL) —
// `accumulate_pool_checkout_wait` must keep `Unavailable` sticky across a
// mixed per-entity outcome: this is the exact fail-open scenario the
// review caught (an earlier entity's read errors, a later entity's read
// succeeds, and the old `_ => next` fallback let the later `Measured`
// silently re-promote the aggregate, masking the earlier unmeasured
// read). Testing the pure function directly (rather than trying to force
// a genuine mixed Ok/Err pair through the full `run_auto_linking`
// pipeline) makes the discrimination deterministic and exhaustive.
// -----------------------------------------------------------------------

#[test]
fn pool_checkout_wait_aggregation_unavailable_is_not_re_promoted_by_later_measured() {
    let acc = LayerAvailability::Unavailable;
    let acc =
        accumulate_pool_checkout_wait(acc, LayerAvailability::Measured(Duration::from_micros(5)));
    assert_eq!(
        acc,
        LayerAvailability::Unavailable,
        "an earlier entity's Unavailable read must not be erased by a \
         later entity's Measured read — that is the checkpoint 4 \
         fail-open bug this fix closes"
    );
}

#[test]
fn pool_checkout_wait_aggregation_measured_becomes_unavailable_after_later_failure() {
    let acc = LayerAvailability::Measured(Duration::from_micros(10));
    let acc = accumulate_pool_checkout_wait(acc, LayerAvailability::Unavailable);
    assert_eq!(
        acc,
        LayerAvailability::Unavailable,
        "a later entity's Unavailable read must degrade the aggregate \
         — Unavailable is sticky in both directions, never just a \
         one-way ratchet toward Measured"
    );
}

#[test]
fn pool_checkout_wait_aggregation_sums_multiple_measured() {
    let acc = LayerAvailability::NotSampled;
    let acc =
        accumulate_pool_checkout_wait(acc, LayerAvailability::Measured(Duration::from_micros(3)));
    let acc =
        accumulate_pool_checkout_wait(acc, LayerAvailability::Measured(Duration::from_micros(4)));
    assert_eq!(
        acc,
        LayerAvailability::Measured(Duration::from_micros(7)),
        "two genuinely Measured entities must sum, matching the \
         pre-existing read_elapsed accumulation convention"
    );
}

// -----------------------------------------------------------------------
// tachi#1145 — `spawn_queue_wait` measures REAL tokio dispatch delay, not
// a placeholder. `LayerAvailability` cannot express this one: unlike
// pool-checkout wait, no layer boundary makes it unreachable — capturing
// `Instant::now()` immediately before `tokio::spawn` needs nothing this
// module cannot already see. So the discrimination bar here is a REAL
// measured delay, not an enum classification.
// -----------------------------------------------------------------------

/// Deterministically forces queuing delay on a single-worker-thread
/// runtime: a wrapper task enqueues the measured task via
/// [`spawn_auto_linking_for_test`] (so `spawn_requested_at` is captured),
/// then IMMEDIATELY monopolizes the sole worker thread with a
/// synchronous, `.await`-free busy-spin for `BUSY_DURATION`. A task's
/// `poll()` must return before the runtime can run anything else, so the
/// already-enqueued measured task genuinely cannot start until the
/// busy-spin's poll returns — no reliance on tokio's LIFO-slot/queue
/// ordering internals, just the basic non-preemptive-poll guarantee.
///
/// Red on origin/main: `spawn_queue_wait` does not exist there at all
/// (this is a new field); red against a version of this leaf that
/// captures `spawn_requested_at` AFTER `tokio::spawn` instead of before
/// (the exact #1145 blind spot: "the auto-link receipt's total clock
/// starts after tokio::spawn") — that ordering would read ~0 regardless
/// of how long the worker was occupied before the task started.
// Plain `#[test]` + `Builder::new_multi_thread().worker_threads(1).block_on`
// (not `#[tokio::test(flavor = "multi_thread", worker_threads = 1)]`),
// matching the `global_test_lock` convention used everywhere else in
// this crate (e.g. `dispatch_ops::prompt::tests`, 5a561225): the guard
// protects the process-wide `TACHI_AUTO_LINK_PHASE_RECEIPTS` env var
// against `w5_auto_link_latency_under_saturation` racing the same key
// under `--include-ignored`, so it must stay held for the entire async
// body including its internal awaits — `block_on` runs that future to
// completion synchronously on this thread, so there is no `.await`
// expression in this function's own body for clippy's
// `await_holding_lock` lint to flag, while the guard's actual coverage
// is unchanged. The explicit `worker_threads(1)` on the `Builder`
// reproduces the removed `#[tokio::test]` attribute's single-worker
// flavor exactly — this test's determinism (the busy-spin wrapper task
// monopolizing the sole worker thread) depends on it; a bare
// `Runtime::new()` would default to a multi-worker runtime and silently
// break that guarantee.
#[test]
fn spawn_queue_wait_measures_dispatch_delay_under_worker_contention() {
    const BUSY_DURATION: Duration = Duration::from_millis(60);
    const MIN_EXPECTED_WAIT: Duration = Duration::from_millis(20);
    const CORRELATION_TOKEN: u64 = 42;

    // `spawn_auto_linking_for_test` goes through `spawn_and_run_auto_linking`,
    // which (unlike `run_auto_linking`'s explicit `sample: bool` param)
    // reads the sampling gate from this env var — the production entry
    // point's real gate, unchanged for this test.
    //
    // tachi#1185 fix-round (codex review checkpoint 6): the prior
    // comment here claimed this was safe without `global_test_lock`
    // because no other test in the crate touches this env var — that
    // was false (`auto_link_latency_w5.rs:259` sets the same var), and
    // the `!Send`-guard justification for skipping the lock was also
    // wrong: `#[tokio::test]` (tokio-macros 2.7) pins the test body to
    // `Pin<&mut dyn Future>` with NO `Send` bound and drives it via
    // `Runtime::block_on`, which itself has no `Send` requirement on the
    // future it polls (only `tokio::spawn`, used below for the INNER
    // task, requires `Send` — and this guard is never moved into that
    // inner task). Holding a `std::sync::MutexGuard` across this test's
    // own `.await` points is therefore fine, and is what actually
    // prevents interleaving with `w5_auto_link_latency_under_saturation`
    // under `--include-ignored`.
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = crate::test_support::EnvRestore::set("TACHI_AUTO_LINK_PHASE_RECEIPTS", "1");

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async {
            let server = crate::tests::make_server();
            let inner_server: MemoryServer = server.clone();
            let entry = test_entry("spawn-queue-wait-probe", "probe entry");
            let (tx, rx) = std::sync::mpsc::channel();

            tokio::spawn(async move {
                spawn_auto_linking_for_test(
                    &inner_server,
                    &entry,
                    DbScope::Global,
                    None,
                    CORRELATION_TOKEN,
                    tx,
                );
                let start = Instant::now();
                while start.elapsed() < BUSY_DURATION {
                    std::hint::spin_loop();
                }
            });

            // Give the single worker time to run the wrapper task
            // (busy-spin) and then the measured task it enqueued.
            tokio::time::sleep(BUSY_DURATION * 3).await;

            let (token, receipt) = rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("measured auto-link task must complete and report back");
            assert_eq!(token, CORRELATION_TOKEN);
            assert!(
                receipt.spawn_queue_wait > MIN_EXPECTED_WAIT,
                "spawn_queue_wait ({:?}) must reflect most of the {:?} the sole \
                 worker thread spent occupied by the busy-spin wrapper task \
                 before the measured task could even start — a value near zero \
                 means `spawn_requested_at` was captured too late (or never \
                 wired at all)",
                receipt.spawn_queue_wait,
                BUSY_DURATION
            );
        });
}

//! Precedent capture goldens (#950 slice 1: capture only).
//!
//! Behavior asserted, not existence:
//! - a `complete` carrying `rulings[]` persists retrievable `/precedents` rows
//!   with the structured ruling intact in metadata and provenance linked;
//! - a `complete` with no `rulings[]` writes no `/precedents` row (byte-compat
//!   with pre-#950 callers);
//! - a malformed ruling is skipped + warned but never fails the completion;
//! - #1027: retrying `complete` with the identical ruling dedupes onto the
//!   existing `/precedents` row (deterministic path derivation) instead of
//!   duplicating it, while two distinct rulings still land as two distinct
//!   rows;
//! - #1027 follow-up (B2): the identical ruling recaptured from a
//!   *different* dispatch/flow still dedupes onto the same row — precedent
//!   identity is the ruling content, not the capturing dispatch, so
//!   provenance never leaks into the dedup-gated `text`;
//! - #1027 follow-up (B1): a NUL byte straddling the `case`/`options`
//!   boundary can't collide two distinct rulings onto the same
//!   deterministic path — the short-id seed frames every field with an
//!   explicit length prefix instead of a bare separator.

use super::*;
use crate::tool_params::{ListMemoriesParams, RulingRecordParams};

fn base_complete() -> TachiCompleteParams {
    TachiCompleteParams {
        task_id: Some("precedent-cap-001".to_string()),
        task: "Adjudicate env-gated security bypass".to_string(),
        agent: "claude-code".to_string(),
        outcome: "success".to_string(),
        task_type: None,
        profile: None,
        risk: None,
        duration_ms: None,
        skills_used: Vec::new(),
        cost_tokens: None,
        cost_usd: None,
        quality_score: None,
        notes: None,
        trajectory: None,
        diff: None,
        worktree: None,
        subagents: Vec::new(),
        feedback_rules_applied: Vec::new(),
        dispatch_id: Some("disp-950".to_string()),
        flow_id: Some("flow-950".to_string()),
        issue_ref: Some("kckylechen1/tachi#530".to_string()),
        pr_ref: None,
        evidence_refs: Vec::new(),
        tests_run: Vec::new(),
        diff_present: None,
        scope: Some("project".to_string()),
        project: None,
        // `full` format returns the raw bundle untouched. The default
        // (compact) receipt also preserves the whole `{recorded, skipped}`
        // precedent-recording object now (`PIPELINE_VARIANT_OBJECT_STAGES` in
        // `evidence_format.rs`) — see
        // `default_format_receipt_still_surfaces_skipped_rulings` below,
        // which asserts that directly. `full` is kept here for parity with
        // how the other completion-record goldens in this dir are written.
        format: Some("full".to_string()),
        signatures: Vec::new(),
        rulings: Vec::new(),
    }
}

#[tokio::test]
async fn complete_with_rulings_persists_retrievable_precedent_rows() {
    let server = make_server();

    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: "cfg(test) env-flippable auth bypass in the vault access check".to_string(),
        options_considered: Some("keep the flag / delete it / gate on a const".to_string()),
        ruling: "env-flippable security switches are standing bypasses — delete, do not gate"
            .to_string(),
        principles_cited: vec![
            "constitution:security/fail-safe".to_string(),
            "precedent:530-P1".to_string(),
        ],
        outcome: Some("validated".to_string()),
        overturned_by: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete with rulings should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");

    let recording = &bundle["pipeline"]["precedent_recording"];
    let recorded = recording["recorded"]
        .as_array()
        .expect("recorded array present");
    assert_eq!(
        recorded.len(),
        1,
        "one ruling should persist: {recording:#}"
    );
    assert!(
        recording["skipped"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "no ruling should be skipped: {recording:#}"
    );

    let entry = &recorded[0];
    let path = entry["path"].as_str().expect("path present");
    assert!(
        path.starts_with("/precedents/global/"),
        "precedent path should nest under project segment: {path}"
    );
    let id = entry["id"].as_str().expect("id present").to_string();

    // The row is actually retrievable with the structured ruling intact.
    let fetched_str = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched: Value = serde_json::from_str(&fetched_str).expect("memory JSON");

    // Stored under `decision` with `metadata.kind` as the discriminator (the
    // documented fallback: `precedent` is not a schema category — see
    // precedent_ops module docs).
    assert_eq!(fetched["category"], json!("decision"));
    let meta = &fetched["metadata"];
    assert_eq!(meta["kind"], json!("precedent"));
    assert_eq!(
        meta["case"],
        json!("cfg(test) env-flippable auth bypass in the vault access check")
    );
    assert_eq!(
        meta["ruling"],
        json!("env-flippable security switches are standing bypasses — delete, do not gate")
    );
    assert_eq!(
        meta["principles_cited"][0],
        json!("constitution:security/fail-safe")
    );
    assert_eq!(meta["principles_cited"][1], json!("precedent:530-P1"));
    assert_eq!(meta["outcome"], json!("validated"));
    assert_eq!(
        meta["options_considered"],
        json!("keep the flag / delete it / gate on a const")
    );
    // Provenance links back to the completion that carried the ruling.
    assert_eq!(meta["dispatch_id"], json!("disp-950"));
    assert_eq!(meta["flow_id"], json!("flow-950"));
    assert_eq!(meta["issue_ref"], json!("kckylechen1/tachi#530"));

    // Human-readable body renders the ruling.
    let text = fetched["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("Ruling: env-flippable security switches are standing bypasses"),
        "body should render the ruling: {text}"
    );
}

/// #1027: the path used to embed a random `Uuid::new_v4()` short id (plus the
/// capture date), so a retried `complete` call carrying the identical ruling
/// never landed on the same path and the exact-path+exact-text dedup gate in
/// `save_memory::persist::find_exact_path_text_duplicate` could never fire —
/// every retry duplicated the row. The short id is now a deterministic
/// `Uuid::new_v5` hash of the ruling's own content (`precedent_short_id`), so
/// retrying the same capture must resolve to the same path and the same
/// existing row's id, not a fresh one.
#[tokio::test]
async fn retrying_same_ruling_dedupes_to_one_precedent_row() {
    let server = make_server();

    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: "duplicate capture check: a retried complete call carrying the identical ruling"
            .to_string(),
        options_considered: Some(
            "keep the random per-capture short id / derive it deterministically from ruling content"
                .to_string(),
        ),
        ruling: "derive the precedent path deterministically from ruling content so a retried \
                  complete lands on the existing row instead of duplicating it"
            .to_string(),
        principles_cited: vec!["precedent:1027-dedup".to_string()],
        outcome: Some("validated".to_string()),
        overturned_by: None,
    }];

    // First capture.
    let first_resp = server
        .tachi_complete(Parameters(params.clone()))
        .await
        .expect("first tachi_complete should succeed");
    let first_bundle: Value = serde_json::from_str(&first_resp).expect("bundle JSON");
    let first_recording = &first_bundle["pipeline"]["precedent_recording"];
    let first_recorded = first_recording["recorded"]
        .as_array()
        .expect("recorded array present");
    assert_eq!(
        first_recorded.len(),
        1,
        "first capture should record one row: {first_recording:#}"
    );
    let first_id = first_recorded[0]["id"]
        .as_str()
        .expect("id present")
        .to_string();
    let first_path = first_recorded[0]["path"]
        .as_str()
        .expect("path present")
        .to_string();

    // Second capture: identical rulings + identical completion metadata — a
    // retry of the exact same `complete` call.
    let second_resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("second (retried) tachi_complete should succeed");
    let second_bundle: Value = serde_json::from_str(&second_resp).expect("bundle JSON");
    let recording = &second_bundle["pipeline"]["precedent_recording"];
    let second_recorded = recording["recorded"]
        .as_array()
        .expect("recorded array present");
    assert_eq!(
        second_recorded.len(),
        1,
        "retried capture should still resolve to exactly one recorded ruling, not a second row: \
         {recording:#}"
    );
    assert!(
        recording["skipped"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "a deduplicated retry is a legitimate exact-duplicate response (real id), never a skip: \
         {recording:#}"
    );

    let second_id = second_recorded[0]["id"]
        .as_str()
        .expect("id present")
        .to_string();
    let second_path = second_recorded[0]["path"]
        .as_str()
        .expect("path present")
        .to_string();
    assert_eq!(
        second_id, first_id,
        "retried capture of the same ruling must dedupe onto the existing row's id, not mint a \
         new one"
    );
    assert_eq!(
        second_path, first_path,
        "same ruling content must derive the same deterministic precedent path across attempts"
    );

    // And there is, in fact, exactly one row sitting at that path — not two
    // rows that merely happen to share an id in the response.
    let listing = server
        .list_memories(Parameters(ListMemoriesParams {
            path_prefix: first_path.clone(),
            limit: 50,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("list_memories should succeed");
    let rows: Vec<Value> = serde_json::from_str(&listing).expect("list JSON");
    assert_eq!(
        rows.iter()
            .filter(|row| row["path"] == json!(first_path))
            .count(),
        1,
        "exactly one precedent row should exist at the deterministic path after a retry: {rows:#?}"
    );
}

/// The flip side of the dedup guarantee: two *different* rulings must never
/// collide onto the same deterministic path (the hash is over the ruling's
/// own content, not a shared constant).
#[tokio::test]
async fn different_ruling_content_gets_a_different_precedent_path() {
    let server = make_server();

    let mut params = base_complete();
    params.rulings = vec![
        RulingRecordParams {
            case: "first distinct case: worktree isolation boundary ruling".to_string(),
            options_considered: None,
            ruling: "first ruling text: dispatched lanes never touch the main checkout".to_string(),
            principles_cited: vec!["precedent:1027-distinct-a".to_string()],
            outcome: Some("validated".to_string()),
            overturned_by: None,
        },
        RulingRecordParams {
            case: "second distinct case: review-implementation vendor separation ruling"
                .to_string(),
            options_considered: None,
            ruling: "second ruling text: implementer and reviewer are always different vendors"
                .to_string(),
            principles_cited: vec!["precedent:1027-distinct-b".to_string()],
            outcome: Some("validated".to_string()),
            overturned_by: None,
        },
    ];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete with two distinct rulings should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let recording = &bundle["pipeline"]["precedent_recording"];
    let recorded = recording["recorded"]
        .as_array()
        .expect("recorded array present");
    assert_eq!(
        recorded.len(),
        2,
        "two distinct rulings should both persist as separate rows: {recording:#}"
    );

    let path_a = recorded[0]["path"].as_str().expect("path present");
    let path_b = recorded[1]["path"].as_str().expect("path present");
    assert_ne!(
        path_a, path_b,
        "distinct ruling content must derive distinct deterministic precedent paths: \
         {recording:#}"
    );
    let id_a = recorded[0]["id"].as_str().expect("id present");
    let id_b = recorded[1]["id"].as_str().expect("id present");
    assert_ne!(
        id_a, id_b,
        "distinct rulings must land as two separate rows, not dedupe onto each other: \
         {recording:#}"
    );
}

/// #1027 follow-up (B2): a precedent's identity is the *ruling content*,
/// never which dispatch happened to capture it. The pre-follow-up
/// `render_body` rendered a "Provenance: dispatch X / flow Y" line straight
/// into the dedup-gated `text`, so the identical ruling recaptured under a
/// *different* `dispatch_id`/`flow_id` derived the same deterministic path
/// but a different `text` — `find_exact_path_text_duplicate` (path+text,
/// both required) never fired, and the retry silently duplicated the row.
/// Provenance now lives only in `metadata`/`keywords`, never `text`, so a
/// same-ruling recapture from an unrelated dispatch must dedupe onto the
/// same row exactly like a same-dispatch retry does.
#[tokio::test]
async fn same_ruling_different_dispatch_still_dedupes_to_one_row() {
    let server = make_server();

    let ruling = RulingRecordParams {
        case: "same ruling, recaptured from an entirely different dispatch/flow".to_string(),
        options_considered: None,
        ruling: "a precedent's identity is its ruling content, not the dispatch that captured it"
            .to_string(),
        principles_cited: vec!["precedent:1027-b2".to_string()],
        outcome: Some("validated".to_string()),
        overturned_by: None,
    };

    let mut first_params = base_complete();
    first_params.dispatch_id = Some("disp-A".to_string());
    first_params.flow_id = Some("flow-A".to_string());
    first_params.rulings = vec![ruling.clone()];

    let first_resp = server
        .tachi_complete(Parameters(first_params))
        .await
        .expect("first tachi_complete should succeed");
    let first_bundle: Value = serde_json::from_str(&first_resp).expect("bundle JSON");
    let first_recording = &first_bundle["pipeline"]["precedent_recording"];
    let first_recorded = first_recording["recorded"]
        .as_array()
        .expect("recorded array present");
    assert_eq!(
        first_recorded.len(),
        1,
        "first capture should record one row: {first_recording:#}"
    );
    let first_id = first_recorded[0]["id"]
        .as_str()
        .expect("id present")
        .to_string();
    let first_path = first_recorded[0]["path"]
        .as_str()
        .expect("path present")
        .to_string();

    // Second capture: identical ruling, but a different dispatch/flow —
    // simulating a retry that happened to originate from a different
    // dispatch entirely, not a literal retry of the same one.
    let mut second_params = base_complete();
    second_params.dispatch_id = Some("disp-B".to_string());
    second_params.flow_id = Some("flow-B".to_string());
    second_params.rulings = vec![ruling];

    let second_resp = server
        .tachi_complete(Parameters(second_params))
        .await
        .expect("second tachi_complete should succeed");
    let second_bundle: Value = serde_json::from_str(&second_resp).expect("bundle JSON");
    let recording = &second_bundle["pipeline"]["precedent_recording"];
    let second_recorded = recording["recorded"]
        .as_array()
        .expect("recorded array present");
    assert_eq!(
        second_recorded.len(),
        1,
        "second (different-dispatch) capture should still resolve to exactly one recorded \
         ruling, not a second row: {recording:#}"
    );
    assert!(
        recording["skipped"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "a deduplicated cross-dispatch retry is a legitimate exact-duplicate response, never a \
         skip: {recording:#}"
    );

    let second_id = second_recorded[0]["id"]
        .as_str()
        .expect("id present")
        .to_string();
    let second_path = second_recorded[0]["path"]
        .as_str()
        .expect("path present")
        .to_string();
    assert_eq!(
        second_id, first_id,
        "same ruling captured from a different dispatch must dedupe onto the existing row's id, \
         not mint a new one"
    );
    assert_eq!(
        second_path, first_path,
        "same ruling content must derive the same deterministic precedent path regardless of \
         which dispatch/flow captured it"
    );

    let listing = server
        .list_memories(Parameters(ListMemoriesParams {
            path_prefix: first_path.clone(),
            limit: 50,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("list_memories should succeed");
    let rows: Vec<Value> = serde_json::from_str(&listing).expect("list JSON");
    assert_eq!(
        rows.iter()
            .filter(|row| row["path"] == json!(first_path))
            .count(),
        1,
        "exactly one precedent row should exist at the deterministic path after a \
         different-dispatch retry: {rows:#?}"
    );
}

/// #1027 follow-up (B1): the pre-follow-up seed NUL-joined fields with a
/// bare `\u{0}` separator and no framing, which doesn't disambiguate a field
/// that itself contains a NUL byte. `case="a\0b", options=None` and
/// `case="a", options=Some("b\0")` NUL-join to the identical byte sequence
/// (`"a\0b\0" == "a" + "\0" + "b\0" + "\0"`), so two genuinely distinct
/// rulings derived the same short id and the same `/precedents` path.
/// `precedent_short_id` now frames every field with an explicit
/// byte-length prefix (`frame_field`) before concatenating, which is
/// provably injective across field boundaries regardless of embedded NUL
/// bytes — these two rulings must land on distinct paths.
#[tokio::test]
async fn nul_ambiguous_ruling_pair_gets_distinct_precedent_paths() {
    let server = make_server();

    let mut params = base_complete();
    params.rulings = vec![
        RulingRecordParams {
            case: "nul-ambiguity check: embedded NUL byte inside `case`, no options\u{0}tail"
                .to_string(),
            options_considered: None,
            ruling: "nul-ambiguity check ruling text shared by both fixtures in this pair"
                .to_string(),
            principles_cited: vec!["precedent:1027-b1".to_string()],
            outcome: Some("validated".to_string()),
            overturned_by: None,
        },
        RulingRecordParams {
            case: "nul-ambiguity check: embedded NUL byte inside `case`, no options".to_string(),
            options_considered: Some("tail\u{0}".to_string()),
            ruling: "nul-ambiguity check ruling text shared by both fixtures in this pair"
                .to_string(),
            principles_cited: vec!["precedent:1027-b1".to_string()],
            outcome: Some("validated".to_string()),
            overturned_by: None,
        },
    ];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete with the NUL-ambiguous pair should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let recording = &bundle["pipeline"]["precedent_recording"];
    let recorded = recording["recorded"]
        .as_array()
        .expect("recorded array present");
    assert_eq!(
        recorded.len(),
        2,
        "both fixtures in the NUL-ambiguous pair should persist as separate rows: {recording:#}"
    );

    let path_a = recorded[0]["path"].as_str().expect("path present");
    let path_b = recorded[1]["path"].as_str().expect("path present");
    assert_ne!(
        path_a, path_b,
        "a NUL byte straddling the case/options boundary must not collide two distinct rulings \
         onto the same deterministic precedent path: {recording:#}"
    );
    let id_a = recorded[0]["id"].as_str().expect("id present");
    let id_b = recorded[1]["id"].as_str().expect("id present");
    assert_ne!(
        id_a, id_b,
        "the NUL-ambiguous pair must land as two separate rows, not dedupe onto each other: \
         {recording:#}"
    );
}

#[tokio::test]
async fn complete_without_rulings_writes_no_precedent() {
    let server = make_server();

    let resp = server
        .tachi_complete(Parameters(base_complete()))
        .await
        .expect("tachi_complete without rulings should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");

    assert_eq!(
        bundle["pipeline"]["precedent_recording"],
        json!("skipped (no rulings)"),
        "no rulings must skip precedent capture entirely: {bundle:#}"
    );
}

#[tokio::test]
async fn malformed_ruling_skipped_but_complete_succeeds() {
    let server = make_server();

    let mut params = base_complete();
    params.rulings = vec![
        // Malformed: empty `ruling`.
        RulingRecordParams {
            case: "some finding with no verdict attached".to_string(),
            options_considered: None,
            ruling: "   ".to_string(),
            principles_cited: Vec::new(),
            outcome: None,
            overturned_by: None,
        },
        // Malformed: unrecognized outcome.
        RulingRecordParams {
            case: "another finding".to_string(),
            options_considered: None,
            ruling: "some ruling".to_string(),
            principles_cited: Vec::new(),
            outcome: Some("maybe".to_string()),
            overturned_by: None,
        },
        // Valid: should still land alongside the skipped ones. Rendered body
        // is kept comfortably above the capture gate's 200-char
        // `BelowMinChars` floor (~360 chars here, not the ~210 a terser case/
        // ruling would render to) so this fixture stays valid if the gate is
        // ever run in `enforce` mode against this same test data.
        RulingRecordParams {
            case: "legacy env-gated security flag left enabled in the vault access path \
                   after the migration that was supposed to remove it"
                .to_string(),
            options_considered: None,
            ruling: "delete the flag outright; do not gate it behind another env var or \
                      config toggle"
                .to_string(),
            principles_cited: vec!["constitution:security/fail-safe".to_string()],
            outcome: None, // defaults to pending
            overturned_by: None,
        },
    ];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("malformed rulings must not fail the completion");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");

    let recording = &bundle["pipeline"]["precedent_recording"];
    let recorded = recording["recorded"].as_array().expect("recorded array");
    let skipped = recording["skipped"].as_array().expect("skipped array");
    assert_eq!(
        recorded.len(),
        1,
        "the one valid ruling lands: {recording:#}"
    );
    assert_eq!(
        skipped.len(),
        2,
        "both malformed rulings skipped: {recording:#}"
    );
    assert_eq!(recorded[0]["outcome"], json!("pending"));
    // Completion recording itself is unaffected.
    assert!(
        bundle.get("eval_entry").is_some(),
        "completion still recorded"
    );
}

/// #962 fix 3: the compact/default `complete` receipt used to reduce
/// `precedent_recording` down to just its `recorded` array
/// (`pipeline_stage_status`'s generic object handling grabs `recorded` and
/// drops everything else) — a rejected/skipped ruling was invisible to any
/// caller that didn't pass `format=full`. `PIPELINE_VARIANT_OBJECT_STAGES`
/// in `evidence_format.rs` now carries `precedent_recording` through whole.
#[tokio::test]
async fn default_format_receipt_still_surfaces_skipped_rulings() {
    let server = make_server();

    let mut params = base_complete();
    params.format = None; // exercise the compact/default receipt shaper
    params.rulings = vec![RulingRecordParams {
        case: "malformed: empty ruling text".to_string(),
        options_considered: None,
        ruling: "   ".to_string(),
        principles_cited: Vec::new(),
        outcome: None,
        overturned_by: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");

    let recording = &bundle["pipeline"]["precedent_recording"];
    let skipped = recording["skipped"]
        .as_array()
        .expect("skipped array must survive the default (non-full) receipt");
    assert_eq!(
        skipped.len(),
        1,
        "malformed ruling must stay visible as skipped under the default receipt: {recording:#}"
    );
    assert!(
        recording["recorded"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "nothing should have recorded: {recording:#}"
    );
}

/// Serializes access to the process-global `TACHI_CAPTURE_GATE` env var so
/// this test doesn't race other tests in the (parallel, same-process) suite.
/// Mirrors `TempHomeGuard`'s save/restore-on-drop pattern in `tests/mod.rs`.
struct CaptureGateEnforceGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    original: Option<std::ffi::OsString>,
}

impl CaptureGateEnforceGuard {
    fn new() -> Self {
        let lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("TACHI_CAPTURE_GATE");
        std::env::set_var("TACHI_CAPTURE_GATE", "enforce");
        Self {
            _lock: lock,
            original,
        }
    }
}

impl Drop for CaptureGateEnforceGuard {
    fn drop(&mut self) {
        match self.original.as_ref() {
            Some(value) => std::env::set_var("TACHI_CAPTURE_GATE", value),
            None => std::env::remove_var("TACHI_CAPTURE_GATE"),
        }
    }
}

/// #962 fix 1: under `TACHI_CAPTURE_GATE=enforce`, `/precedents` must be a
/// whitelisted capture-gate bucket (BASE_BUCKETS in
/// `memory-server-capture-gate`) so a well-formed ruling still persists, and
/// a ruling that's normalization-valid but genuinely too short to clear the
/// gate's `BelowMinChars` floor must land in `skipped` — never silently
/// counted as `recorded` with a null id.
#[tokio::test]
async fn enforce_mode_persists_valid_ruling_and_skips_gate_rejected_one() {
    let _guard = CaptureGateEnforceGuard::new();
    let server = make_server();

    let mut params = base_complete();
    params.rulings = vec![
        // Well-formed and long enough once rendered — must persist now that
        // /precedents is capture-gate whitelisted.
        RulingRecordParams {
            case: "cfg(test) env-flippable auth bypass in the vault access check, found \
                   during the #950 precedent-capture review of the completion pipeline"
                .to_string(),
            options_considered: Some(
                "keep the flag behind cfg(test) / delete it outright / gate on a const".to_string(),
            ),
            ruling: "env-flippable security switches are standing bypasses — delete them, \
                      do not gate them behind another toggle"
                .to_string(),
            principles_cited: vec![
                "constitution:security/fail-safe".to_string(),
                "precedent:530-P1".to_string(),
            ],
            outcome: Some("validated".to_string()),
            overturned_by: None,
        },
        // Normalization-valid (non-empty case/ruling) but the rendered body
        // is far under the 200-char capture floor — a genuine capture-gate
        // rejection, distinct from a malformed-ruling skip.
        RulingRecordParams {
            case: "x".to_string(),
            options_considered: None,
            ruling: "y".to_string(),
            principles_cited: Vec::new(),
            outcome: None,
            overturned_by: None,
        },
    ];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("enforce-mode capture-gate rejection must not fail the completion");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");

    let recording = &bundle["pipeline"]["precedent_recording"];
    let recorded = recording["recorded"].as_array().expect("recorded array");
    let skipped = recording["skipped"].as_array().expect("skipped array");
    assert_eq!(
        recorded.len(),
        1,
        "the well-formed ruling must persist under enforce mode: {recording:#}"
    );
    assert!(
        recorded[0]["id"].as_str().is_some_and(|id| !id.is_empty()),
        "persisted ruling must carry a real id: {recording:#}"
    );
    assert_eq!(
        skipped.len(),
        1,
        "the too-short ruling must be rejected by the gate, not silently persisted: {recording:#}"
    );
    assert_eq!(skipped[0]["case"], json!("x"));
    let reason = skipped[0]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("capture_gate") || reason.contains("BelowMinChars"),
        "skip reason should point at the capture gate: {reason}"
    );

    // Completion recording itself is unaffected by the precedent-capture
    // rejection (the fail-safe boundary from the module docs).
    assert!(
        bundle.get("eval_entry").is_some(),
        "completion still recorded"
    );
}

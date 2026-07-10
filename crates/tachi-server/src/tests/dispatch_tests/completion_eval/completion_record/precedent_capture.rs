//! Precedent capture goldens (#950 slice 1: capture only).
//!
//! Behavior asserted, not existence:
//! - a `complete` carrying `rulings[]` persists retrievable `/precedents` rows
//!   with the structured ruling intact in metadata and provenance linked;
//! - a `complete` with no `rulings[]` writes no `/precedents` row (byte-compat
//!   with pre-#950 callers);
//! - a malformed ruling is skipped + warned but never fails the completion.

use super::*;
use crate::tool_params::RulingRecordParams;

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
    assert_eq!(recorded.len(), 1, "one ruling should persist: {recording:#}");
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
    assert_eq!(recorded.len(), 1, "the one valid ruling lands: {recording:#}");
    assert_eq!(skipped.len(), 2, "both malformed rulings skipped: {recording:#}");
    assert_eq!(recorded[0]["outcome"], json!("pending"));
    // Completion recording itself is unaffected.
    assert!(bundle.get("eval_entry").is_some(), "completion still recorded");
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
                "keep the flag behind cfg(test) / delete it outright / gate on a const"
                    .to_string(),
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
    assert!(bundle.get("eval_entry").is_some(), "completion still recorded");
}

//! Principle-level precedent candidate decomposition goldens (#1076).
//!
//! Six-case frozen RED corpus (issue #1076):
//! 1. a verdict containing two independent principles produces two
//!    candidates; a one-summary output is RED;
//! 2. the 530-P1/894-S0 security-switch example preserves the shared
//!    principle and each distinct case ref;
//! 3. same source replay is idempotent; an edited owner comment
//!    (`id + updated_at + body_hash`) invalidates the old source revision
//!    instead of being treated as an identical replay;
//! 4. missing adjudicator/outcome/source authority remains pending and
//!    cannot masquerade as established;
//! 5. contradictory/self-overturn text is surfaced for the establishment
//!    gate, not harmonized away;
//! 6. full-source coverage is accounted; no truncation/drop-tail path.
//!
//! Every case below is RED on pre-#1076 `main`: `precedent_candidate_ops`
//! does not exist, `complete`'s pipeline never carries a
//! `precedent_candidate_decomposition` stage, and `RulingRecordParams` has
//! no `adjudicator` / `source_refs` / `engine_receipt` fields to even
//! construct these params — a compile error against pre-#1076 `main`.
//!
//! **Structural justification for compile-RED (#1183 fix-round finding 13):**
//! this is not a case of "any assertion is RED because nothing exists yet"
//! substituting for real behavioral discrimination — every field this test
//! module needs (`adjudicator`, `source_refs`, `engine_receipt`) is new,
//! Rust struct literals must name every field, and the decomposition
//! behavior under test is *defined in terms of* those new fields (an
//! adjudicator identity, source evidence, an engine receipt). There is no
//! pre-#1076 code path that can be driven with the new inputs to observe a
//! wrong OLD behavior, because the old code has no slot for those inputs at
//! all — the "call the existing entry point with the new inputs, assert on
//! behavior" pattern is unavailable by construction. What IS avoidable, and
//! what finding 13 correctly flagged, is conflating multiple independently-
//! meaningful conditions inside one compile-red case so a bug in one
//! could hide behind the others always being present too; RED case 4 is
//! split below into three cases (missing-adjudicator-only,
//! missing-source-refs-only, missing-both) for exactly that reason — see
//! each case's own doc comment.

use super::*;
use crate::tool_params::{
    ListMemoriesParams, RulingEngineReceiptParams, RulingRecordParams, RulingSourceRefParams,
};

fn base_complete() -> TachiCompleteParams {
    TachiCompleteParams {
        task_id: Some("precedent-candidate-001".to_string()),
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
        dispatch_id: Some("disp-1076".to_string()),
        flow_id: Some("flow-1076".to_string()),
        issue_ref: Some("kckylechen1/tachi#1076".to_string()),
        pr_ref: None,
        evidence_refs: Vec::new(),
        tests_run: Vec::new(),
        diff_present: None,
        scope: Some("project".to_string()),
        project: None,
        // `full` returns the raw pipeline bundle untouched (see
        // `precedent_capture.rs`'s identical convention).
        format: Some("full".to_string()),
        signatures: Vec::new(),
        rulings: Vec::new(),
        adjudication: None,
    }
}

fn comment_source_ref(
    comment_id: &str,
    updated_at: &str,
    body_hash: &str,
) -> RulingSourceRefParams {
    RulingSourceRefParams {
        relation: Some("supports".to_string()),
        target_kind: "comment".to_string(),
        target_ref: format!("kckylechen1/tachi#530#issuecomment-{comment_id}"),
        comment_id: Some(comment_id.to_string()),
        updated_at: Some(updated_at.to_string()),
        body_hash: Some(body_hash.to_string()),
        commit_sha: None,
        section_or_span: None,
    }
}

fn recorded_array(bundle: &Value) -> Vec<Value> {
    bundle["pipeline"]["precedent_candidate_decomposition"]["recorded"]
        .as_array()
        .expect("recorded array present")
        .clone()
}

async fn fetch_metadata(server: &crate::MemoryServer, id: &str) -> Value {
    let fetched_str = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched: Value = serde_json::from_str(&fetched_str).expect("memory JSON");
    fetched
}

/// RED case 1: a verdict containing two independent principles produces two
/// candidates; a one-summary output is RED.
#[tokio::test]
async fn two_independent_principles_produce_two_candidates() {
    let server = make_server();
    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: "cfg(test) env-flippable auth bypass in the vault access check".to_string(),
        options_considered: Some("keep the flag / delete it / gate on a const".to_string()),
        ruling: "env-flippable security switches are standing bypasses -- delete, do not gate"
            .to_string(),
        principles_cited: vec![
            "constitution:security/fail-safe".to_string(),
            "constitution:testing/no-prod-gates".to_string(),
        ],
        outcome: Some("validated".to_string()),
        overturned_by: None,
        adjudicator: Some("owner".to_string()),
        source_refs: vec![comment_source_ref("1", "2026-07-01T00:00:00Z", "hash-1")],
        engine_receipt: Some(RulingEngineReceiptParams {
            requested_role: Some("leader".to_string()),
            effective_provider: Some("anthropic".to_string()),
            effective_model: Some("claude-sonnet-5".to_string()),
            effective_version: Some("2026-07".to_string()),
            fallback_chain: Vec::new(),
            degraded: false,
        }),
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete with a two-principle ruling should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");

    let decomposition = &bundle["pipeline"]["precedent_candidate_decomposition"];
    let recorded = decomposition["recorded"]
        .as_array()
        .expect("recorded array present");
    assert_eq!(
        recorded.len(),
        2,
        "a verdict citing two independent principles must decompose into two candidates, not one \
         summary: {decomposition:#}"
    );
    assert!(
        decomposition["skipped"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "no ruling should be skipped: {decomposition:#}"
    );

    let principles: std::collections::HashSet<&str> = recorded
        .iter()
        .map(|c| c["principle"].as_str().expect("principle present"))
        .collect();
    assert!(principles.contains("constitution:security/fail-safe"));
    assert!(principles.contains("constitution:testing/no-prod-gates"));

    let paths: std::collections::HashSet<&str> = recorded
        .iter()
        .map(|c| c["path"].as_str().expect("path present"))
        .collect();
    assert_eq!(
        paths.len(),
        2,
        "each principle must land at its own distinct path: {recorded:#?}"
    );
    for p in &paths {
        assert!(
            p.starts_with("/precedent_candidates/global/"),
            "candidate path should nest under the precedent_candidates project segment: {p}"
        );
    }

    // Both candidates preserve the FULL case/ruling text, not a trimmed
    // one-summary rendering.
    for entry in recorded {
        let id = entry["id"].as_str().expect("id present");
        let fetched = fetch_metadata(&server, id).await;
        let meta = &fetched["metadata"];
        assert_eq!(fetched["category"], json!("decision"));
        assert_eq!(meta["kind"], json!("precedent_candidate"));
        assert_eq!(meta["candidate_status"], json!("pending"));
        assert_eq!(
            meta["case"],
            json!("cfg(test) env-flippable auth bypass in the vault access check")
        );
        assert_eq!(
            meta["ruling"],
            json!("env-flippable security switches are standing bypasses -- delete, do not gate")
        );
        assert_eq!(meta["principle_count"], json!(2));
        assert_eq!(meta["identity_status"], json!("known"));
        assert_eq!(meta["authority_complete"], json!(true));
    }
}

/// RED case 2: the 530-P1/894-S0 security-switch example preserves the
/// shared principle and each distinct case ref -- two rulings that cite the
/// SAME principle from two DIFFERENT cases must never be merged into one
/// candidate.
#[tokio::test]
async fn shared_principle_across_distinct_cases_stays_distinct() {
    let server = make_server();
    let mut params = base_complete();
    params.rulings = vec![
        RulingRecordParams {
            case: "precedent:530-P1 -- cfg(test) env-flippable auth bypass in the vault access \
                   check"
                .to_string(),
            options_considered: None,
            ruling: "env-flippable security switches are standing bypasses -- delete, do not gate"
                .to_string(),
            principles_cited: vec!["constitution:security/fail-safe".to_string()],
            outcome: Some("validated".to_string()),
            overturned_by: None,
            adjudicator: Some("owner".to_string()),
            source_refs: Vec::new(),
            engine_receipt: None,
        },
        RulingRecordParams {
            case: "precedent:894-S0 -- fail-open authorization gate defaulted to `true` on \
                   missing config"
                .to_string(),
            options_considered: None,
            ruling: "an authorization gate's missing-config default must be deny, not allow"
                .to_string(),
            principles_cited: vec!["constitution:security/fail-safe".to_string()],
            outcome: Some("validated".to_string()),
            overturned_by: None,
            adjudicator: Some("owner".to_string()),
            source_refs: Vec::new(),
            engine_receipt: None,
        },
    ];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete with two rulings sharing a principle should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let recorded = recorded_array(&bundle);
    assert_eq!(
        recorded.len(),
        2,
        "a principle shared across two distinct case refs must produce two candidates, one per \
         case, never merged into one: {recorded:#?}"
    );

    let cases: std::collections::HashSet<&str> = recorded
        .iter()
        .map(|c| c["case"].as_str().expect("case present"))
        .collect();
    assert_eq!(
        cases.len(),
        2,
        "each candidate must retain its own distinct case ref: {recorded:#?}"
    );
    assert!(cases.iter().any(|c| c.contains("530-P1")));
    assert!(cases.iter().any(|c| c.contains("894-S0")));

    for entry in &recorded {
        assert_eq!(
            entry["principle"],
            json!("constitution:security/fail-safe"),
            "the shared principle tag must be preserved on both candidates: {entry:#}"
        );
    }

    let paths: std::collections::HashSet<&str> = recorded
        .iter()
        .map(|c| c["path"].as_str().expect("path present"))
        .collect();
    assert_eq!(
        paths.len(),
        2,
        "distinct case refs sharing a principle must not collapse onto the same candidate path: \
         {recorded:#?}"
    );
}

/// RED case 3: same source replay is idempotent; an edited owner comment
/// (`id + updated_at + body_hash`) is a different immutable revision and
/// must not be treated as the same replay -- a new candidate revision is
/// appended (sharing the prior row's `candidate_group_id`) while the prior
/// row is left untouched.
#[tokio::test]
async fn same_source_replay_idempotent_edited_comment_appends_revision() {
    let server = make_server();

    let ruling_v1 = RulingRecordParams {
        case: "#1076 dedup check for precedent candidates".to_string(),
        options_considered: None,
        ruling: "candidate identity must fold in source-ref revisions, not just ruling content"
            .to_string(),
        principles_cited: vec!["precedent:1076-dedup".to_string()],
        outcome: Some("validated".to_string()),
        overturned_by: None,
        adjudicator: Some("owner".to_string()),
        source_refs: vec![comment_source_ref("42", "2026-07-01T00:00:00Z", "hash-v1")],
        engine_receipt: None,
    };

    let mut params = base_complete();
    params.rulings = vec![ruling_v1.clone()];

    let first_resp = server
        .tachi_complete(Parameters(params.clone()))
        .await
        .expect("first tachi_complete should succeed");
    let first_bundle: Value = serde_json::from_str(&first_resp).expect("bundle JSON");
    let first_recorded = recorded_array(&first_bundle);
    assert_eq!(
        first_recorded.len(),
        1,
        "first capture should record one candidate: {first_recorded:#?}"
    );
    let first_id = first_recorded[0]["id"].as_str().unwrap().to_string();
    let first_path = first_recorded[0]["path"].as_str().unwrap().to_string();
    let first_group = first_recorded[0]["candidate_group_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Replay: identical ruling, identical source_refs -- must dedupe onto
    // the same row.
    let replay_resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("replayed tachi_complete should succeed");
    let replay_bundle: Value = serde_json::from_str(&replay_resp).expect("bundle JSON");
    let replay_recorded = recorded_array(&replay_bundle);
    assert_eq!(
        replay_recorded.len(),
        1,
        "an exact-source replay must dedupe to one candidate: {replay_recorded:#?}"
    );
    assert_eq!(
        replay_recorded[0]["id"].as_str().unwrap(),
        first_id,
        "an exact-source replay must resolve to the SAME candidate row, not a new one"
    );
    assert_eq!(replay_recorded[0]["path"].as_str().unwrap(), first_path);

    // Edited comment: same comment_id, different updated_at/body_hash.
    let mut ruling_v2 = ruling_v1;
    ruling_v2.source_refs = vec![comment_source_ref("42", "2026-07-05T00:00:00Z", "hash-v2")];
    let mut edited_params = base_complete();
    edited_params.rulings = vec![ruling_v2];

    let edited_resp = server
        .tachi_complete(Parameters(edited_params))
        .await
        .expect("edited-comment tachi_complete should succeed");
    let edited_bundle: Value = serde_json::from_str(&edited_resp).expect("bundle JSON");
    let edited_recorded = recorded_array(&edited_bundle);
    assert_eq!(
        edited_recorded.len(),
        1,
        "the edited-comment capture should still record its own candidate: {edited_recorded:#?}"
    );
    let edited_id = edited_recorded[0]["id"].as_str().unwrap().to_string();
    let edited_path = edited_recorded[0]["path"].as_str().unwrap().to_string();
    assert_ne!(
        edited_id, first_id,
        "an edited comment revision (same comment_id, different updated_at/body_hash) must NOT be \
         treated as the same replay -- it must append a new candidate revision, not dedupe onto \
         the stale one"
    );
    assert_ne!(edited_path, first_path);
    assert_eq!(
        edited_recorded[0]["candidate_group_id"].as_str().unwrap(),
        first_group,
        "the new revision must still share the old candidate's case/principle group identity"
    );

    // The original row is untouched -- both rows are retrievable, append-only.
    let listing = server
        .list_memories(Parameters(ListMemoriesParams {
            path_prefix: "/precedent_candidates/global/".to_string(),
            limit: 50,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("list_memories should succeed");
    let rows: Vec<Value> = serde_json::from_str(&listing).expect("list JSON");
    let matching_count = rows
        .iter()
        .filter(|row| row["path"] == json!(first_path) || row["path"] == json!(edited_path))
        .count();
    assert_eq!(
        matching_count, 2,
        "both the original and the edited-revision candidate rows must exist -- the fan-out never \
         edits or deletes a previously written candidate: {rows:#?}"
    );
}

/// RED case 4a (#1183 fix-round finding 13 granularity split): an
/// adjudicator alone, with source authority present, is still NOT complete
/// authority -- isolates the "missing adjudicator" half of case 4 so a bug
/// that only checks `source_refs` can't hide behind both being absent at
/// once.
#[tokio::test]
async fn missing_adjudicator_alone_keeps_authority_incomplete() {
    let server = make_server();
    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: "ruling with pinned source evidence but no adjudicator identity".to_string(),
        options_considered: None,
        ruling: "source evidence alone cannot substitute for adjudicator identity".to_string(),
        principles_cited: vec!["constitution:precedent/authority".to_string()],
        outcome: Some("validated".to_string()),
        overturned_by: None,
        adjudicator: None,
        source_refs: vec![comment_source_ref("9", "2026-07-01T00:00:00Z", "hash-9")],
        engine_receipt: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let recorded = recorded_array(&bundle);
    assert_eq!(
        recorded.len(),
        1,
        "the ruling still decomposes: {recorded:#?}"
    );
    assert_eq!(
        recorded[0]["authority_complete"],
        json!(false),
        "a pinned source_ref with no adjudicator must not read as authority-complete: {recorded:#?}"
    );
}

/// RED case 4b (#1183 fix-round finding 13 granularity split): an
/// adjudicator present, with zero source refs, is still NOT complete
/// authority -- isolates the "missing source authority" half.
#[tokio::test]
async fn missing_source_refs_alone_keeps_authority_incomplete() {
    let server = make_server();
    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: "ruling with an adjudicator but no source evidence at all".to_string(),
        options_considered: None,
        ruling: "an adjudicator's say-so alone cannot substitute for source evidence".to_string(),
        principles_cited: vec!["constitution:precedent/authority".to_string()],
        outcome: Some("validated".to_string()),
        overturned_by: None,
        adjudicator: Some("owner".to_string()),
        source_refs: Vec::new(),
        engine_receipt: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let recorded = recorded_array(&bundle);
    assert_eq!(
        recorded.len(),
        1,
        "the ruling still decomposes: {recorded:#?}"
    );
    assert_eq!(
        recorded[0]["authority_complete"],
        json!(false),
        "an adjudicator with zero source_refs must not read as authority-complete: {recorded:#?}"
    );
}

/// RED case 4 (#1183 fix-round finding 5/6): a source_ref supplied but
/// REJECTED (missing the snapshot hash its `target_kind` requires to pin an
/// immutable revision) must degrade `authority_complete` and `coverage`,
/// never silently vanish behind an unconditional "full"/"complete" claim.
#[tokio::test]
async fn malformed_source_ref_degrades_coverage_and_authority() {
    let server = make_server();
    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: "ruling with one pinned ref and one unpinned (no body_hash) ref".to_string(),
        options_considered: None,
        ruling: "an unpinned source_ref must be dropped, not silently accepted as evidence"
            .to_string(),
        principles_cited: vec!["constitution:precedent/authority".to_string()],
        outcome: Some("validated".to_string()),
        overturned_by: None,
        adjudicator: Some("owner".to_string()),
        source_refs: vec![
            comment_source_ref("10", "2026-07-01T00:00:00Z", "hash-10"),
            RulingSourceRefParams {
                relation: Some("supports".to_string()),
                target_kind: "issue".to_string(),
                target_ref: "kckylechen1/tachi#1076".to_string(),
                comment_id: None,
                updated_at: Some("2026-07-16T00:00:00Z".to_string()),
                body_hash: None, // missing snapshot hash -- must be rejected
                commit_sha: None,
                section_or_span: None,
            },
        ],
        engine_receipt: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed even with a malformed source_ref");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let recorded = recorded_array(&bundle);
    assert_eq!(
        recorded.len(),
        1,
        "the ruling still decomposes: {recorded:#?}"
    );
    assert_eq!(
        recorded[0]["authority_complete"],
        json!(false),
        "a dropped (malformed) source_ref must degrade authority_complete, not vanish: \
         {recorded:#?}"
    );

    let id = recorded[0]["id"].as_str().unwrap();
    let fetched = fetch_metadata(&server, id).await;
    let meta = &fetched["metadata"];
    assert_eq!(
        meta["coverage"],
        json!("partial"),
        "a dropped source_ref must degrade coverage from \"full\" to \"partial\": {meta:#}"
    );
    assert_eq!(
        meta["source_ref_count"],
        json!(1),
        "only the pinned ref survives into source_refs: {meta:#}"
    );
    let warnings = meta["source_ref_warnings"]
        .as_array()
        .expect("source_ref_warnings present when a ref was dropped");
    assert_eq!(warnings.len(), 1, "exactly one ref was dropped: {meta:#}");
}

/// RED case 4 (combined): missing adjudicator/outcome/source authority
/// remains pending and cannot masquerade as established.
#[tokio::test]
async fn missing_adjudicator_and_source_refs_stays_pending_and_incomplete() {
    let server = make_server();
    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: "ruling captured with no adjudicator identity and no source evidence".to_string(),
        options_considered: None,
        ruling: "a ruling missing provenance still decomposes, but can never claim authority"
            .to_string(),
        principles_cited: vec!["constitution:precedent/authority".to_string()],
        outcome: None, // defaults to "pending"
        overturned_by: None,
        adjudicator: None,
        source_refs: Vec::new(),
        engine_receipt: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed even with incomplete authority");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let recorded = recorded_array(&bundle);
    assert_eq!(
        recorded.len(),
        1,
        "the ruling still decomposes: {recorded:#?}"
    );
    assert_eq!(
        recorded[0]["candidate_status"],
        json!("pending"),
        "a ruling with no adjudicator/source authority must still land as pending: {recorded:#?}"
    );
    assert_eq!(
        recorded[0]["authority_complete"],
        json!(false),
        "missing adjudicator + missing source_refs must mark authority_complete=false: \
         {recorded:#?}"
    );

    let id = recorded[0]["id"].as_str().unwrap();
    let fetched = fetch_metadata(&server, id).await;
    let meta = &fetched["metadata"];
    assert_eq!(meta["candidate_status"], json!("pending"));
    assert_eq!(meta["authority_complete"], json!(false));
    assert_eq!(meta["identity_status"], json!("preview_only"));
    assert!(
        meta.get("adjudicator").is_none(),
        "no adjudicator was supplied -- metadata must not fabricate one: {meta:#}"
    );
    let serialized = fetched.to_string();
    assert!(
        !serialized.contains("\"established\""),
        "an authority-incomplete candidate must never carry any 'established' marker anywhere in \
         its stored row: {serialized}"
    );
}

/// RED case 5: contradictory/self-overturn text is surfaced for the
/// establishment gate, not harmonized away.
#[tokio::test]
async fn contradictory_self_overturn_text_is_preserved_verbatim() {
    let server = make_server();
    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: "leader initially ruled the switch could stay behind a const gate".to_string(),
        options_considered: Some(
            "leader's first pass: gate the switch behind a const (losing alternative, later \
             reversed) / owner self-overturn: delete the switch entirely"
                .to_string(),
        ),
        ruling: "owner self-overturn: the const-gate ruling was wrong -- delete the switch, no \
                 exceptions"
            .to_string(),
        principles_cited: vec![
            "constitution:security/fail-safe".to_string(),
            "constitution:precedent/self-overturn".to_string(),
        ],
        outcome: Some("overturned".to_string()),
        overturned_by: Some(
            "owner direct ruling, 2026-07-16: supersedes the leader's const-gate proposal"
                .to_string(),
        ),
        adjudicator: Some("owner".to_string()),
        source_refs: Vec::new(),
        engine_receipt: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let recorded = recorded_array(&bundle);
    assert_eq!(
        recorded.len(),
        2,
        "both cited principles must each get their own candidate: {recorded:#?}"
    );

    for entry in &recorded {
        let id = entry["id"].as_str().unwrap();
        let fetched = fetch_metadata(&server, id).await;
        let meta = &fetched["metadata"];
        assert_eq!(meta["outcome"], json!("overturned"));
        assert_eq!(
            meta["overturned_by"],
            json!("owner direct ruling, 2026-07-16: supersedes the leader's const-gate proposal"),
            "the self-overturn text must be preserved verbatim on every principle candidate, not \
             dropped or harmonized away: {meta:#}"
        );
        assert_eq!(
            meta["options_considered"],
            json!(
                "leader's first pass: gate the switch behind a const (losing alternative, later \
                 reversed) / owner self-overturn: delete the switch entirely"
            ),
            "the losing alternative recorded in options_considered must survive verbatim: {meta:#}"
        );
        let text = fetched["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("Overturned by: owner direct ruling"),
            "the rendered body must surface the overturn, not silently drop it: {text}"
        );
        assert!(
            text.contains("losing alternative"),
            "the rendered body must surface the losing alternative, not silently drop it: {text}"
        );
    }
}

/// RED case 6: full-source coverage is accounted; no truncation/drop-tail
/// path.
#[tokio::test]
async fn full_source_coverage_no_truncation_or_drop_tail() {
    let server = make_server();
    // Longer than the 100-char `summary` teaser cap -- `metadata.case` (a
    // content field) must never be truncated even though `summary` is.
    let long_case = "x".repeat(300);
    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: long_case.clone(),
        options_considered: Some("full options text preserved regardless of length".to_string()),
        ruling: "full ruling text preserved regardless of length".to_string(),
        principles_cited: vec![
            "constitution:coverage/one".to_string(),
            "constitution:coverage/two".to_string(),
        ],
        outcome: Some("validated".to_string()),
        overturned_by: None,
        adjudicator: Some("owner".to_string()),
        source_refs: vec![
            comment_source_ref("1", "2026-07-01T00:00:00Z", "hash-a"),
            comment_source_ref("2", "2026-07-02T00:00:00Z", "hash-b"),
            RulingSourceRefParams {
                relation: Some("supports".to_string()),
                target_kind: "issue".to_string(),
                target_ref: "kckylechen1/tachi#1076".to_string(),
                comment_id: None,
                updated_at: Some("2026-07-16T00:00:00Z".to_string()),
                body_hash: Some("issue-hash".to_string()),
                commit_sha: None,
                section_or_span: Some("## Frozen contract".to_string()),
            },
        ],
        engine_receipt: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let recorded = recorded_array(&bundle);
    assert_eq!(
        recorded.len(),
        2,
        "two principles must both be decomposed: {recorded:#?}"
    );

    for entry in &recorded {
        let id = entry["id"].as_str().unwrap();
        let fetched = fetch_metadata(&server, id).await;
        let meta = &fetched["metadata"];
        assert_eq!(
            meta["case"],
            json!(long_case),
            "content fields are atomic -- the full case text must never be truncated: {meta:#}"
        );
        assert_eq!(meta["coverage"], json!("full"));
        let source_refs = meta["source_refs"]
            .as_array()
            .expect("source_refs array present");
        assert_eq!(
            source_refs.len(),
            3,
            "every principle candidate must carry the ruling's COMPLETE source_refs set, not a \
             partitioned subset: {meta:#}"
        );
        assert_eq!(meta["source_ref_count"], json!(3));

        // The `summary` teaser (not a content field) IS allowed to be
        // shortened -- confirms this test isn't accidentally passing because
        // nothing anywhere ever truncates.
        let summary = fetched["summary"].as_str().unwrap_or_default();
        assert!(
            summary.chars().count() <= 90,
            "the summary teaser should stay short even though content fields don't: {summary}"
        );
    }
}

/// Byte-compat: a `complete` call with no `rulings[]` writes no
/// `/precedent_candidates` rows -- same "skipped (no rulings)" shape as the
/// sibling `precedent_recording` stage.
#[tokio::test]
async fn complete_without_rulings_skips_candidate_decomposition() {
    let server = make_server();
    let params = base_complete();
    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete without rulings should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    assert_eq!(
        bundle["pipeline"]["precedent_candidate_decomposition"],
        json!("skipped (no rulings)")
    );
}

/// A ruling with no `principles_cited` has nothing principle-level to
/// decompose into -- fail-closed skip + warn, never a fabricated
/// "whole-verdict" candidate masquerading as principle-level.
#[tokio::test]
async fn ruling_with_no_principles_cited_is_skipped_not_fabricated() {
    let server = make_server();
    let mut params = base_complete();
    params.rulings = vec![RulingRecordParams {
        case: "ruling with no principles cited at all".to_string(),
        options_considered: None,
        ruling: "a ruling only becomes principle-level candidates when principles are cited"
            .to_string(),
        principles_cited: Vec::new(),
        outcome: Some("validated".to_string()),
        overturned_by: None,
        adjudicator: Some("owner".to_string()),
        source_refs: Vec::new(),
        engine_receipt: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");
    let decomposition = &bundle["pipeline"]["precedent_candidate_decomposition"];
    assert!(
        decomposition["recorded"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "no principle-level candidate should be fabricated from a ruling with zero \
         principles_cited: {decomposition:#}"
    );
    let skipped = decomposition["skipped"]
        .as_array()
        .expect("skipped array present");
    assert_eq!(
        skipped.len(),
        1,
        "the ruling must be reported skipped: {decomposition:#}"
    );
}

/// #1183 fix-round finding 12: `precedent_candidate_decomposition` was
/// added to `PIPELINE_VARIANT_OBJECT_STAGES` in `evidence_format.rs`
/// (mirroring the existing `precedent_recording` entry, see that const's
/// doc comment), but every other test in this file passes `format="full"`,
/// so nothing actually exercised the compact/default receipt shaper for the
/// candidate stage. Mirrors
/// `precedent_capture::default_format_receipt_still_surfaces_skipped_rulings`
/// (#962 fix 3) for the sibling stage: without the
/// `PIPELINE_VARIANT_OBJECT_STAGES` entry, `pipeline_stage_status`'s generic
/// object handling would grab only `recorded` and silently drop `skipped`
/// under the default (non-`full`) receipt.
#[tokio::test]
async fn default_format_receipt_still_surfaces_skipped_candidates() {
    let server = make_server();

    let mut params = base_complete();
    params.format = None; // exercise the compact/default receipt shaper
    params.rulings = vec![RulingRecordParams {
        case: "ruling with no principles cited, captured under the default receipt".to_string(),
        options_considered: None,
        ruling: "nothing principle-level to decompose".to_string(),
        principles_cited: Vec::new(),
        outcome: Some("validated".to_string()),
        overturned_by: None,
        adjudicator: Some("owner".to_string()),
        source_refs: Vec::new(),
        engine_receipt: None,
    }];

    let resp = server
        .tachi_complete(Parameters(params))
        .await
        .expect("tachi_complete should succeed");
    let bundle: Value = serde_json::from_str(&resp).expect("bundle JSON");

    let decomposition = &bundle["pipeline"]["precedent_candidate_decomposition"];
    let skipped = decomposition["skipped"]
        .as_array()
        .expect("skipped array must survive the default (non-full) receipt");
    assert_eq!(
        skipped.len(),
        1,
        "a ruling with zero principles_cited must stay visible as skipped under the default \
         receipt: {decomposition:#}"
    );
    assert!(
        decomposition["recorded"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "no candidate should be fabricated: {decomposition:#}"
    );
}

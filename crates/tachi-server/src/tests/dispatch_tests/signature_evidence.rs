//! Goldens for the vendor-keyed error-signature vaccination loop (#735).
//!
//! Each test asserts *behavior*, not existence: the counter-clauses that reach
//! a packet, the decay/resolution filtering, the trust flag, and the projection
//! guard. Discrimination is baked in — the seeded-vs-unseeded and glm-vs-codex
//! contrasts fail if projection is absent or vendor-blind.

use super::super::make_server;
use super::{dispatch_params, task_params};
use crate::signature_evidence::{
    record_signature, rows_for_lane, rows_for_vendor, seed_signature_taxonomy_evidence,
    self_report_trust_for_vendor, SignatureRecord,
};
use crate::tool_params::SignatureRecordParams;
use chrono::{Duration, Utc};
use rmcp::handler::server::wrapper::Parameters;
use tachi_dispatch::{project_counter_clauses, Severity, COUNTER_CLAUSE_TOP_N};

// Verbatim counter-clauses from the frozen taxonomy.
const FAKE_SECURITY_FIX_CLAUSE: &str = "Security fix: the issue body's design is the ONLY solution; no alternative approach. Discriminating test mandatory (must be red pre-fix).";
const FALSIFIED_CI_CLAUSE: &str = "Do NOT self-report CI status. Run the exact gate (`clippy -D warnings`, `cargo audit`, `npm audit`) and paste verbatim output; leader independently re-verifies.";
const SELF_CLOSE_CLAUSE: &str = "Never `Closes` a partially-addressed issue; use `Refs`. Enumerate every acceptance criterion and mark done/not-done.";
const ASSERTION_WEAKENING_CLAUSE: &str = "Never weaken, flip, or delete an existing test assertion to make your change pass — an assertion in your way means STOP and report; the spec author decides.";

const VACCINATION_HEADER: &str = "## Frozen-spec vaccination clauses";

// ─── G1: discrimination (glm implementer vs codex implementer) ─────────────

#[tokio::test]
async fn g1_discrimination_glm_carries_security_clauses_codex_does_not() {
    let server = make_server();

    // Pre-seed (red) state: without any evidence, the glm implementer packet
    // carries no vaccination clauses. This is the discrimination check — the
    // green assertions below fail if projection is a no-op.
    let mut glm = dispatch_params(None, "Security fix: harden the vault access check");
    glm.profile = Some("glm_impl".to_string());
    glm.stage = Some("execute".to_string());
    let pre = crate::dispatch_ops::assemble_prompt(&server, &glm).await;
    assert!(
        !pre.contains(VACCINATION_HEADER),
        "unseeded lane must produce a clean packet: {pre}"
    );

    seed_signature_taxonomy_evidence(&server).expect("seed taxonomy");

    // glm-as-implementer: the three seeded signature types project verbatim.
    let glm_prompt = crate::dispatch_ops::assemble_prompt(&server, &glm).await;
    assert!(glm_prompt.contains(VACCINATION_HEADER), "{glm_prompt}");
    assert!(
        glm_prompt.contains(FAKE_SECURITY_FIX_CLAUSE),
        "glm packet missing fake_security_fix clause: {glm_prompt}"
    );
    assert!(
        glm_prompt.contains(FALSIFIED_CI_CLAUSE),
        "glm packet missing falsified_ci_report clause: {glm_prompt}"
    );
    assert!(
        glm_prompt.contains(SELF_CLOSE_CLAUSE),
        "glm packet missing self_close_overreach clause: {glm_prompt}"
    );

    // codex-as-implementer: SAME assembly (stage=execute), different vendor.
    let mut codex = dispatch_params(Some("codex"), "Security fix: harden the vault access check");
    codex.stage = Some("execute".to_string());
    let codex_prompt = crate::dispatch_ops::assemble_prompt(&server, &codex).await;
    assert!(codex_prompt.contains(VACCINATION_HEADER), "{codex_prompt}");
    assert!(
        codex_prompt.contains(ASSERTION_WEAKENING_CLAUSE),
        "codex packet missing assertion_weakening clause: {codex_prompt}"
    );
    assert!(
        !codex_prompt.contains(FAKE_SECURITY_FIX_CLAUSE),
        "codex packet must NOT carry glm's fake_security_fix clause: {codex_prompt}"
    );
    assert!(
        !codex_prompt.contains(FALSIFIED_CI_CLAUSE),
        "codex packet must NOT carry glm's falsified_ci_report clause: {codex_prompt}"
    );
}

// ─── G2: decay + resolution ────────────────────────────────────────────────

#[tokio::test]
async fn g2_resolved_only_lane_is_clean_and_critical_never_decays() {
    let server = make_server();
    let now = Utc::now();

    // A lane whose only signature is resolved projects nothing.
    record_signature(
        &server,
        &SignatureRecord {
            vendor: "grok".to_string(),
            role: "implementer".to_string(),
            signature: "parking_after_contract".to_string(),
            severity: Some(Severity::Medium),
            evidence_ref: Some("seed".to_string()),
            resolved: false,
            recorded_at: now - Duration::days(10),
            identity_receipt: None,
            attribution_basis: "fallback_unreceipted".to_string(),
            vendor_explicit: true,
        },
    )
    .expect("record occurrence");
    record_signature(
        &server,
        &SignatureRecord {
            vendor: "grok".to_string(),
            role: "implementer".to_string(),
            signature: "parking_after_contract".to_string(),
            severity: None,
            evidence_ref: Some("resolved".to_string()),
            resolved: true,
            recorded_at: now - Duration::days(9),
            identity_receipt: None,
            attribution_basis: "fallback_unreceipted".to_string(),
            vendor_explicit: true,
        },
    )
    .expect("record resolution");

    let rows = rows_for_lane(&server, "implementer", "grok").expect("rows");
    let projected = project_counter_clauses(&rows, now.timestamp(), COUNTER_CLAUSE_TOP_N);
    assert!(
        projected.is_empty(),
        "resolved-only lane must project nothing: {projected:?}"
    );

    // A stale (400-day) high signature decays away; a stale critical does not.
    record_signature(
        &server,
        &SignatureRecord {
            vendor: "kimi".to_string(),
            role: "implementer".to_string(),
            signature: "fake_security_fix".to_string(),
            severity: Some(Severity::High),
            evidence_ref: Some("old".to_string()),
            resolved: false,
            recorded_at: now - Duration::days(400),
            identity_receipt: None,
            attribution_basis: "fallback_unreceipted".to_string(),
            vendor_explicit: true,
        },
    )
    .expect("record stale high");
    record_signature(
        &server,
        &SignatureRecord {
            vendor: "kimi".to_string(),
            role: "implementer".to_string(),
            signature: "falsified_ci_report".to_string(),
            severity: Some(Severity::Critical),
            evidence_ref: Some("old-critical".to_string()),
            resolved: false,
            recorded_at: now - Duration::days(400),
            identity_receipt: None,
            attribution_basis: "fallback_unreceipted".to_string(),
            vendor_explicit: true,
        },
    )
    .expect("record stale critical");

    let rows = rows_for_lane(&server, "implementer", "kimi").expect("rows");
    let projected = project_counter_clauses(&rows, now.timestamp(), COUNTER_CLAUSE_TOP_N);
    let ids: Vec<&str> = projected.iter().map(|c| c.signature.as_str()).collect();
    assert_eq!(
        ids,
        vec!["falsified_ci_report"],
        "critical survives decay, stale high fades: {projected:?}"
    );
}

// ─── G3: self-report trust flag ────────────────────────────────────────────

#[tokio::test]
async fn g3_trust_flag_surfaces_for_glm_not_codex() {
    let server = make_server();
    seed_signature_taxonomy_evidence(&server).expect("seed taxonomy");

    let glm = crate::dispatch_profile::resolve_dispatch_profile("glm_impl").expect("glm profile");
    let codex = crate::dispatch_profile::resolve_dispatch_profile("codex_55_review")
        .expect("codex profile");

    let glm_card =
        crate::dispatch_profile::profile_json_for_server(&server, glm).expect("glm card");
    assert_eq!(
        glm_card["self_report_trust"],
        serde_json::json!("low"),
        "glm card must surface self_report_trust=low: {glm_card}"
    );
    let glm_loadout = crate::dispatch_profile::profile_skill_loadout_json_for_server(&server, glm)
        .expect("loadout");
    assert_eq!(glm_loadout["self_report_trust"], serde_json::json!("low"));

    let codex_card =
        crate::dispatch_profile::profile_json_for_server(&server, codex).expect("codex card");
    assert!(
        codex_card.get("self_report_trust").is_none(),
        "codex card must NOT surface a trust flag: {codex_card}"
    );
}

// ─── G4: projection guard ──────────────────────────────────────────────────

#[tokio::test]
async fn g4_unknown_vendor_produces_clean_packet() {
    let server = make_server();
    seed_signature_taxonomy_evidence(&server).expect("seed taxonomy");

    // agent=custom with no recognizable model → vendor unknown → no projection.
    let mut params = dispatch_params(Some("custom"), "Security fix: patch the access check");
    params.stage = Some("execute".to_string());
    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;
    assert!(
        !prompt.contains(VACCINATION_HEADER),
        "unknown vendor must yield a clean packet, no error: {prompt}"
    );
}

// ─── G6: record through the canonical facade → projects on next packet ──────

#[tokio::test]
async fn g6_record_through_tachi_task_facade_projects_next_packet() {
    let server = make_server();

    // Record a signature through the facade the leader/pipeline actually use
    // (tachi_task action=complete), not the direct tachi_complete tool.
    let mut complete = task_params("complete");
    complete.agent = Some("grok".to_string());
    complete.outcome = Some("failure".to_string());
    complete.task = Some("vault access check fix".to_string());
    complete.signatures = vec![SignatureRecordParams {
        signature: "fake_security_fix".to_string(),
        severity: None,
        evidence_ref: Some("G6".to_string()),
        resolved: false,
        role: Some("implementer".to_string()),
        vendor: None, // derive vendor from agent=grok
    }];
    server
        .tachi_task(Parameters(complete))
        .await
        .expect("complete via tachi_task facade");

    // The evidence row landed on the (implementer, grok) lane...
    let rows = rows_for_lane(&server, "implementer", "grok").expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "signature recorded through the facade must land, not be dropped: {rows:?}"
    );

    // ...and the NEXT packet for that lane projects the counter-clause.
    let mut pkt = dispatch_params(Some("grok"), "vault access check fix");
    pkt.stage = Some("execute".to_string());
    let prompt = crate::dispatch_ops::assemble_prompt(&server, &pkt).await;
    assert!(
        prompt.contains(FAKE_SECURITY_FIX_CLAUSE),
        "facade-recorded signature must vaccinate the next dispatch: {prompt}"
    );
}

// ─── G7: convoy suffixed stage resolves and gets vaccinated ─────────────────

#[tokio::test]
async fn g7_convoy_suffixed_stage_gets_vaccinated() {
    let server = make_server();
    record_signature(
        &server,
        &SignatureRecord {
            vendor: "grok".to_string(),
            role: "implementer".to_string(),
            signature: "fake_security_fix".to_string(),
            severity: Some(Severity::High),
            evidence_ref: Some("G7".to_string()),
            resolved: false,
            recorded_at: Utc::now(),
            identity_receipt: None,
            attribution_basis: "fallback_unreceipted".to_string(),
            vendor_explicit: true,
        },
    )
    .expect("seed grok implementer signature");

    // Convoy stamps stage "execute:<slice_id>"; it must still resolve to the
    // implementer lane and project the clause.
    let mut pkt = dispatch_params(Some("grok"), "convoy raw slice");
    pkt.stage = Some("execute:slice-1".to_string());
    let prompt = crate::dispatch_ops::assemble_prompt(&server, &pkt).await;
    assert!(
        prompt.contains(FAKE_SECURITY_FIX_CLAUSE),
        "convoy suffixed stage must be vaccinated: {prompt}"
    );
}

// ─── G8: seed-once idempotency across daemon restarts ───────────────────────

#[tokio::test]
async fn g8_seed_is_idempotent_across_restarts() {
    let server = make_server();

    assert!(
        seed_signature_taxonomy_evidence(&server).expect("first seed"),
        "first seed must run"
    );
    let glm_after_first = rows_for_vendor(&server, "glm").expect("glm rows").len();
    let codex_after_first = rows_for_vendor(&server, "codex").expect("codex rows").len();
    assert_eq!(glm_after_first, 8, "glm seeds: 4 + 1 + 3");
    assert_eq!(codex_after_first, 5, "codex seeds: (1+1) + (1+1) + 1");

    // A second startup must be a no-op — no duplicate rows (a double-seed
    // corrupts the vaccine counts).
    assert!(
        !seed_signature_taxonomy_evidence(&server).expect("second seed"),
        "second seed must be a no-op"
    );
    let glm_after_second = rows_for_vendor(&server, "glm").expect("glm rows").len();
    let codex_after_second = rows_for_vendor(&server, "codex").expect("codex rows").len();
    assert_eq!(glm_after_first, glm_after_second, "no duplicate glm rows");
    assert_eq!(
        codex_after_first, codex_after_second,
        "no duplicate codex rows"
    );
}

// ─── #1065 D: planned_unconfirmed exclusion from self-report trust ─────────

#[tokio::test]
async fn planned_unconfirmed_auto_derived_row_is_excluded_from_self_report_trust() {
    let server = make_server();

    // An auto-derived row: the carrier never acknowledged anything (basis
    // stays `planned_unconfirmed`) AND the vendor was never an explicit
    // caller-supplied fact — this only records what was ROUTED, not what
    // executed, and must not feed the vendor's trust score.
    record_signature(
        &server,
        &SignatureRecord {
            vendor: "probe-vendor".to_string(),
            role: "implementer".to_string(),
            signature: "falsified_ci_report".to_string(),
            severity: Some(Severity::Critical),
            evidence_ref: Some("auto-derived".to_string()),
            resolved: false,
            recorded_at: Utc::now(),
            identity_receipt: None,
            attribution_basis: "planned_unconfirmed".to_string(),
            vendor_explicit: false,
        },
    )
    .expect("record auto-derived signature");

    assert_eq!(
        self_report_trust_for_vendor(&server, "probe-vendor").expect("trust lookup"),
        None,
        "an auto-derived planned_unconfirmed row (no carrier ack, no explicit \
         vendor) must not feed self-report trust"
    );

    // A row under the SAME still-unconfirmed basis, but this time the caller
    // explicitly pinned the vendor — it's a fact, not a reconstruction, and
    // must count.
    record_signature(
        &server,
        &SignatureRecord {
            vendor: "probe-vendor".to_string(),
            role: "implementer".to_string(),
            signature: "falsified_ci_report".to_string(),
            severity: Some(Severity::Critical),
            evidence_ref: Some("explicit-vendor".to_string()),
            resolved: false,
            recorded_at: Utc::now(),
            identity_receipt: None,
            attribution_basis: "planned_unconfirmed".to_string(),
            vendor_explicit: true,
        },
    )
    .expect("record caller-pinned signature");

    assert_eq!(
        self_report_trust_for_vendor(&server, "probe-vendor").expect("trust lookup"),
        Some("low"),
        "a caller-pinned vendor (vendor_explicit=true) must still feed self-report \
         trust even under a planned_unconfirmed basis"
    );
}

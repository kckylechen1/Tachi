//! Goldens for the vendor-keyed error-signature vaccination loop (#735).
//!
//! Each test asserts *behavior*, not existence: the counter-clauses that reach
//! a packet, the decay/resolution filtering, the trust flag, and the projection
//! guard. Discrimination is baked in — the seeded-vs-unseeded and glm-vs-codex
//! contrasts fail if projection is absent or vendor-blind.

use super::super::make_server;
use super::dispatch_params;
use crate::signature_evidence::{
    record_signature, rows_for_lane, seed_signature_taxonomy_evidence, SignatureRecord,
};
use chrono::{Duration, Utc};
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
    glm.profile = Some("glm_51_impl".to_string());
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

    let glm =
        crate::dispatch_profile::resolve_dispatch_profile("glm_51_impl").expect("glm profile");
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

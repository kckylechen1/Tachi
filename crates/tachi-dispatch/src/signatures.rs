//! Vendor-keyed error-signature taxonomy and counter-clause projection.
//!
//! First-cut slice of #534 / #735: an adjudicated lane failure is recorded as a
//! vendor-keyed error signature; on the lane's *next* dispatch the matching
//! counter-clause is projected verbatim into the packet's frozen-spec section,
//! ACT-R-decayed by recency/frequency.
//!
//! This module is pure policy: the taxonomy is code constants (never persisted
//! as truth), evidence rows are supplied by the caller (stored append-only in
//! the memory server), and the projection is *computed* here at assembly time.
//! Timestamps arrive already parsed to epoch seconds — the caller owns RFC3339
//! parsing so the epoch-not-lexical comparison rule lives in exactly one place.

use serde::{Deserialize, Serialize};

/// Severity of an error signature. Governs both projection ranking and the
/// decay exemption: `Critical` never fades until explicitly resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    /// `critical`-severity signatures are exempt from ACT-R decay until an
    /// explicit resolution row is appended (#735 frozen decision (c)).
    pub fn decay_exempt(self) -> bool {
        matches!(self, Severity::Critical)
    }

    /// Ranking weight (higher = projected first when trimming to top-N).
    pub fn rank(self) -> u8 {
        match self {
            Severity::Low => 0,
            Severity::Medium => 1,
            Severity::High => 2,
            Severity::Critical => 3,
        }
    }
}

/// A frozen taxonomy entry: stable id, verbatim counter-clause, severity.
#[derive(Debug, Clone, Copy)]
pub struct SignatureDef {
    pub id: &'static str,
    pub counter_clause: &'static str,
    pub severity: Severity,
}

/// The frozen error-signature taxonomy (code constants, never persisted).
///
/// The first eight rows are verbatim from
/// `docs/engineering/architecture/experience-to-card-evolution.md`
/// §"Error-signature taxonomy"; `assertion_weakening` was ratified 2026-07-06
/// from the #733 adjudication (issue #735).
pub const ERROR_SIGNATURE_TAXONOMY: &[SignatureDef] = &[
    SignatureDef {
        id: "fake_security_fix",
        counter_clause: "Security fix: the issue body's design is the ONLY solution; no alternative approach. Discriminating test mandatory (must be red pre-fix).",
        severity: Severity::High,
    },
    SignatureDef {
        id: "self_close_overreach",
        counter_clause: "Never `Closes` a partially-addressed issue; use `Refs`. Enumerate every acceptance criterion and mark done/not-done.",
        severity: Severity::Medium,
    },
    SignatureDef {
        id: "zero_discriminating_test",
        counter_clause: "Every behavior/security change ships a test that fails on the pre-fix code. STOP and report if you can't write one.",
        severity: Severity::High,
    },
    SignatureDef {
        id: "falsified_ci_report",
        counter_clause: "Do NOT self-report CI status. Run the exact gate (`clippy -D warnings`, `cargo audit`, `npm audit`) and paste verbatim output; leader independently re-verifies.",
        severity: Severity::Critical,
    },
    SignatureDef {
        id: "inherited_base_commit",
        counter_clause: "Workspace base SHA is `<leader-supplied verified SHA>`; do not fetch/derive your own base (no-network sandboxes make 'cut from origin/main' a lie).",
        severity: Severity::High,
    },
    SignatureDef {
        id: "stale_rlib_poisoning",
        counter_clause: "A gate failure whose unresolved symbols grep-exist in source = shared-target cross-poisoning; `cargo clean -p <crate>` then rebuild before attributing.",
        severity: Severity::Low,
    },
    SignatureDef {
        id: "breadcrumb_violation",
        counter_clause: "Slice under ~150 lines with no cross-crate contract change merges into the previous slice; no new branch/PR/ceremony.",
        severity: Severity::Medium,
    },
    SignatureDef {
        id: "parking_after_contract",
        counter_clause: "MANDATE = the whole todo ledger, not one contract; 'end to end' = ledger drained; ship one, start the next.",
        severity: Severity::Medium,
    },
    SignatureDef {
        id: "assertion_weakening",
        counter_clause: "Never weaken, flip, or delete an existing test assertion to make your change pass — an assertion in your way means STOP and report; the spec author decides.",
        severity: Severity::High,
    },
];

/// Look up a taxonomy entry by its stable id.
pub fn signature_def(id: &str) -> Option<&'static SignatureDef> {
    ERROR_SIGNATURE_TAXONOMY.iter().find(|def| def.id == id)
}

// ─── Provisional decay / projection parameters ─────────────────────────────
// Flat magic numbers are banned; these are named and provisional. Calibrate
// from telemetry once enough adjudication traces accumulate (#735 decision 4).

/// Top-N counter-clauses projected into a packet — provisional, calibrate from
/// telemetry.
pub const COUNTER_CLAUSE_TOP_N: usize = 3;

/// ACT-R base-level decay rate `d`. 0.5 is the canonical ACT-R default —
/// provisional, calibrate from telemetry.
pub const ACT_R_DECAY_RATE: f64 = 0.5;

/// Age floor (in days) applied to each occurrence before decay so a
/// just-recorded signature does not divide by zero — provisional, calibrate
/// from telemetry.
pub const ACT_R_MIN_AGE_DAYS: f64 = 0.5;

/// Base-level activation below which a (non-critical) signature is considered
/// faded and is not projected — provisional, calibrate from telemetry. Chosen
/// so a single occurrence stays active for roughly five months and multiple
/// recent occurrences stay active far longer.
pub const ACT_R_ACTIVATION_FLOOR: f64 = -2.5;

const SECONDS_PER_DAY: f64 = 86_400.0;

/// Kind of an appended evidence row. Evidence is append-only: a resolution is a
/// *new* row, never a deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureRowKind {
    Signature,
    Resolution,
}

/// One append-only evidence row for a `(role, vendor)` lane, already filtered to
/// a single lane and with its timestamp parsed to epoch seconds by the caller.
#[derive(Debug, Clone)]
pub struct SignatureEvidenceRow {
    pub kind: SignatureRowKind,
    pub signature: String,
    /// Severity as recorded; taxonomy severity takes precedence when the id is
    /// known. `None` on resolution rows.
    pub severity: Option<Severity>,
    pub evidence_ref: Option<String>,
    pub recorded_at_epoch: i64,
}

/// A projected counter-clause ready to inject into a packet's frozen-spec
/// section.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedCounterClause {
    pub signature: String,
    pub counter_clause: String,
    pub severity: Severity,
    pub activation: f64,
}

/// ACT-R base-level activation: `B = ln( Σ (Δt_i)^-d )`, Δt in days, floored at
/// `ACT_R_MIN_AGE_DAYS`. Recency and frequency both raise activation.
fn base_level_activation(occurrence_epochs: &[i64], now_epoch: i64) -> f64 {
    let sum: f64 = occurrence_epochs
        .iter()
        .map(|&t| {
            let age_days = ((now_epoch - t) as f64 / SECONDS_PER_DAY).max(ACT_R_MIN_AGE_DAYS);
            age_days.powf(-ACT_R_DECAY_RATE)
        })
        .sum();
    if sum <= 0.0 {
        f64::NEG_INFINITY
    } else {
        sum.ln()
    }
}

/// True if the signature's latest resolution is at or after its latest
/// occurrence (append-only: a later occurrence re-activates a resolved
/// signature). Compares epochs numerically — never RFC3339 strings.
fn signature_is_resolved(rows: &[&SignatureEvidenceRow]) -> bool {
    let latest_occurrence = rows
        .iter()
        .filter(|r| r.kind == SignatureRowKind::Signature)
        .map(|r| r.recorded_at_epoch)
        .max();
    let latest_resolution = rows
        .iter()
        .filter(|r| r.kind == SignatureRowKind::Resolution)
        .map(|r| r.recorded_at_epoch)
        .max();
    match (latest_occurrence, latest_resolution) {
        (Some(occ), Some(res)) => res >= occ,
        (None, _) => true, // no occurrences left to project
        (Some(_), None) => false,
    }
}

/// Effective severity for a signature id: taxonomy is authoritative; fall back
/// to the recorded severity, then `Medium`.
fn effective_severity(signature: &str, rows: &[&SignatureEvidenceRow]) -> Severity {
    if let Some(def) = signature_def(signature) {
        return def.severity;
    }
    rows.iter()
        .filter_map(|r| r.severity)
        .max_by_key(|s| s.rank())
        .unwrap_or(Severity::Medium)
}

/// Group rows by signature id, preserving first-seen order for determinism.
fn group_by_signature(
    rows: &[SignatureEvidenceRow],
) -> Vec<(String, Vec<&SignatureEvidenceRow>)> {
    let mut groups: Vec<(String, Vec<&SignatureEvidenceRow>)> = Vec::new();
    for row in rows {
        if let Some(entry) = groups.iter_mut().find(|(id, _)| id == &row.signature) {
            entry.1.push(row);
        } else {
            groups.push((row.signature.clone(), vec![row]));
        }
    }
    groups
}

/// Project the active counter-clauses for a lane, ACT-R-decayed and trimmed to
/// `top_n`. `rows` must already be filtered to a single `(role, vendor)` lane.
///
/// A signature is projected when it is unresolved AND either critical
/// (decay-exempt) or its base-level activation clears the floor. Ranking:
/// severity desc, then activation desc, then signature id for determinism.
pub fn project_counter_clauses(
    rows: &[SignatureEvidenceRow],
    now_epoch: i64,
    top_n: usize,
) -> Vec<ProjectedCounterClause> {
    let mut projected: Vec<ProjectedCounterClause> = Vec::new();
    for (signature, group) in group_by_signature(rows) {
        // Unknown ids have no counter-clause to inject.
        let Some(def) = signature_def(&signature) else {
            continue;
        };
        if signature_is_resolved(&group) {
            continue;
        }
        let severity = effective_severity(&signature, &group);
        let occurrence_epochs: Vec<i64> = group
            .iter()
            .filter(|r| r.kind == SignatureRowKind::Signature)
            .map(|r| r.recorded_at_epoch)
            .collect();
        if occurrence_epochs.is_empty() {
            continue;
        }
        let activation = base_level_activation(&occurrence_epochs, now_epoch);
        if !severity.decay_exempt() && activation < ACT_R_ACTIVATION_FLOOR {
            continue;
        }
        projected.push(ProjectedCounterClause {
            signature: signature.clone(),
            counter_clause: def.counter_clause.to_string(),
            severity,
            activation,
        });
    }
    projected.sort_by(|a, b| {
        b.severity
            .rank()
            .cmp(&a.severity.rank())
            .then(
                b.activation
                    .partial_cmp(&a.activation)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.signature.cmp(&b.signature))
    });
    projected.truncate(top_n);
    projected
}

/// Vendor-level self-report trust. Returns `Some("low")` when an unresolved
/// `falsified_ci_report` row is present for the vendor (#735 decision 5).
/// Computed at read time, never persisted. `rows` should include every row for
/// the vendor across roles.
pub fn self_report_trust(rows: &[SignatureEvidenceRow]) -> Option<&'static str> {
    let falsified: Vec<&SignatureEvidenceRow> = rows
        .iter()
        .filter(|r| r.signature == "falsified_ci_report")
        .collect();
    if falsified.is_empty() {
        return None;
    }
    if signature_is_resolved(&falsified) {
        None
    } else {
        Some("low")
    }
}

/// Normalize a `(backend, model)` pair to a canonical vendor lane id.
///
/// Model identity wins (the family lives in the model string); the backend is a
/// fallback for the named single-model CLIs. Granularity is family-level
/// (`glm`, `codex`, `claude`, ...) so a lane's card survives model version
/// bumps. Unresolvable → `"unknown"`, which never receives clause projection
/// (#735 frozen decision 1).
pub fn normalize_vendor(backend: &str, model: Option<&str>) -> String {
    if let Some(model) = model {
        let tail = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        if let Some(family) = vendor_family_from_token(&tail) {
            return family.to_string();
        }
    }
    let backend_norm = backend.trim().to_ascii_lowercase();
    match backend_norm.as_str() {
        "claude" => "claude".to_string(),
        "codex" | "openai" => "codex".to_string(),
        "grok" | "xai" => "grok".to_string(),
        "kimi" | "moonshot" => "kimi".to_string(),
        // Adapter backends (custom / opencode) carry no vendor of their own; the
        // model token already failed to resolve above.
        _ => "unknown".to_string(),
    }
}

fn vendor_family_from_token(token: &str) -> Option<&'static str> {
    // Order matters only for disjoint substrings; these families do not overlap.
    const FAMILIES: &[(&str, &str)] = &[
        ("glm", "glm"),
        ("deepseek", "deepseek"),
        ("kimi", "kimi"),
        ("moonshot", "kimi"),
        ("qwen", "qwen"),
        ("codex", "codex"),
        ("gpt", "codex"),
        ("o1", "codex"),
        ("o3", "codex"),
        ("claude", "claude"),
        ("opus", "claude"),
        ("sonnet", "claude"),
        ("haiku", "claude"),
        ("grok", "grok"),
    ];
    FAMILIES
        .iter()
        .find(|(needle, _)| token.contains(needle))
        .map(|(_, family)| *family)
}

/// Coarse role class for the `(role, vendor)` axis. Accepts a dispatch profile
/// `role`, a `stage`, or an already-canonical class, so the seed, the `complete`
/// recording path, and packet assembly all agree. Unrecognized → `None` (no
/// projection).
pub fn dispatch_role_class(role_or_stage: &str) -> Option<&'static str> {
    match role_or_stage.trim().to_ascii_lowercase().as_str() {
        "implementer" | "executor" | "execute" | "hotfix" => Some("implementer"),
        "reviewer" | "senior_reviewer" | "fast_checker" | "review" | "review_light"
        | "ux_researcher" => Some("reviewer"),
        "planner" | "plan" => Some("planner"),
        "architect" | "plan_review" => Some("architect"),
        "explorer" | "explore" | "probe" => Some("explorer"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(signature: &str, severity: Severity, age_days: f64, now: i64) -> SignatureEvidenceRow {
        SignatureEvidenceRow {
            kind: SignatureRowKind::Signature,
            signature: signature.to_string(),
            severity: Some(severity),
            evidence_ref: None,
            recorded_at_epoch: now - (age_days * SECONDS_PER_DAY) as i64,
        }
    }

    fn resolution(signature: &str, age_days: f64, now: i64) -> SignatureEvidenceRow {
        SignatureEvidenceRow {
            kind: SignatureRowKind::Resolution,
            signature: signature.to_string(),
            severity: None,
            evidence_ref: None,
            recorded_at_epoch: now - (age_days * SECONDS_PER_DAY) as i64,
        }
    }

    #[test]
    fn taxonomy_has_all_nine_frozen_signatures() {
        for id in [
            "fake_security_fix",
            "self_close_overreach",
            "zero_discriminating_test",
            "falsified_ci_report",
            "inherited_base_commit",
            "stale_rlib_poisoning",
            "breadcrumb_violation",
            "parking_after_contract",
            "assertion_weakening",
        ] {
            assert!(signature_def(id).is_some(), "missing taxonomy id {id}");
        }
        assert_eq!(ERROR_SIGNATURE_TAXONOMY.len(), 9);
        assert!(signature_def("falsified_ci_report")
            .unwrap()
            .severity
            .decay_exempt());
    }

    #[test]
    fn normalize_vendor_maps_families_and_unknown() {
        assert_eq!(
            normalize_vendor("custom", Some("zhipuai-coding-plan/glm-5.1")),
            "glm"
        );
        assert_eq!(
            normalize_vendor("custom", Some("zhipuai-coding-plan/glm-5.2")),
            "glm"
        );
        assert_eq!(
            normalize_vendor("opencode", Some("deepseek/deepseek-v4-flash")),
            "deepseek"
        );
        assert_eq!(normalize_vendor("codex", None), "codex");
        assert_eq!(normalize_vendor("openai", None), "codex");
        assert_eq!(normalize_vendor("codex", Some("gpt-5.5-codex")), "codex");
        assert_eq!(normalize_vendor("claude", None), "claude");
        assert_eq!(
            normalize_vendor("custom", Some("anthropic/claude-opus")),
            "claude"
        );
        assert_eq!(normalize_vendor("grok", None), "grok");
        assert_eq!(normalize_vendor("kimi", None), "kimi");
        // Adapter backend with no recognizable model → unknown.
        assert_eq!(normalize_vendor("custom", None), "unknown");
        assert_eq!(
            normalize_vendor("custom", Some("some-unlisted-model")),
            "unknown"
        );
    }

    #[test]
    fn role_class_maps_profile_roles_stages_and_canonical() {
        assert_eq!(dispatch_role_class("executor"), Some("implementer"));
        assert_eq!(dispatch_role_class("execute"), Some("implementer"));
        assert_eq!(dispatch_role_class("implementer"), Some("implementer"));
        assert_eq!(dispatch_role_class("senior_reviewer"), Some("reviewer"));
        assert_eq!(dispatch_role_class("review"), Some("reviewer"));
        assert_eq!(dispatch_role_class("explore"), Some("explorer"));
        assert_eq!(dispatch_role_class("nonsense"), None);
    }

    #[test]
    fn projection_ranks_by_severity_and_trims_to_top_n() {
        let now = 1_700_000_000;
        let rows = vec![
            sig("self_close_overreach", Severity::Medium, 0.1, now),
            sig("fake_security_fix", Severity::High, 0.1, now),
            sig("fake_security_fix", Severity::High, 0.1, now),
            sig("falsified_ci_report", Severity::Critical, 0.1, now),
            sig("breadcrumb_violation", Severity::Medium, 0.1, now),
        ];
        let projected = project_counter_clauses(&rows, now, COUNTER_CLAUSE_TOP_N);
        assert_eq!(projected.len(), 3);
        // critical first, then high, then a medium.
        assert_eq!(projected[0].signature, "falsified_ci_report");
        assert_eq!(projected[1].signature, "fake_security_fix");
        assert_eq!(projected[2].severity, Severity::Medium);
    }

    #[test]
    fn resolved_signature_is_not_projected_but_later_recurrence_reactivates() {
        let now = 1_700_000_000;
        // occurrence older than its resolution → resolved → dropped.
        let resolved = vec![
            sig("parking_after_contract", Severity::Medium, 10.0, now),
            resolution("parking_after_contract", 9.0, now),
        ];
        assert!(project_counter_clauses(&resolved, now, COUNTER_CLAUSE_TOP_N).is_empty());

        // a fresh occurrence AFTER the resolution re-activates the signature.
        let mut reactivated = resolved.clone();
        reactivated.push(sig("parking_after_contract", Severity::Medium, 0.1, now));
        let projected = project_counter_clauses(&reactivated, now, COUNTER_CLAUSE_TOP_N);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].signature, "parking_after_contract");
    }

    #[test]
    fn stale_signature_decays_but_critical_is_exempt() {
        let now = 1_700_000_000;
        // A single 400-day-old high signature falls below the floor.
        let stale = vec![sig("fake_security_fix", Severity::High, 400.0, now)];
        assert!(project_counter_clauses(&stale, now, COUNTER_CLAUSE_TOP_N).is_empty());

        // A single 400-day-old critical signature is projected regardless of age.
        let old_critical = vec![sig("falsified_ci_report", Severity::Critical, 400.0, now)];
        let projected = project_counter_clauses(&old_critical, now, COUNTER_CLAUSE_TOP_N);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].signature, "falsified_ci_report");
    }

    #[test]
    fn self_report_trust_reflects_unresolved_falsified_ci() {
        let now = 1_700_000_000;
        let low = vec![sig("falsified_ci_report", Severity::Critical, 1.0, now)];
        assert_eq!(self_report_trust(&low), Some("low"));

        let resolved = vec![
            sig("falsified_ci_report", Severity::Critical, 2.0, now),
            resolution("falsified_ci_report", 1.0, now),
        ];
        assert_eq!(self_report_trust(&resolved), None);

        let none = vec![sig("fake_security_fix", Severity::High, 1.0, now)];
        assert_eq!(self_report_trust(&none), None);
    }
}

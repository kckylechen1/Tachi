//! Report-only instruction drift / density sentinel (#1304).
//!
//! Consumes [`scan_instruction_manifest`] status snapshots. Never writes
//! sources, projections, approvals, or promotions.

#[cfg(test)]
use super::instruction_manifest::scan_instruction_manifest;
use super::instruction_manifest::{
    InstructionManifestStatus, InstructionSourceStatus, InstructionTargetStatus,
};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::sync::OnceLock;

const DRIFT_SCHEMA: &str = "tachi.instruction_drift.v1";
const CLEAN_STATUS: &str = "clean";
const FINDINGS_STATUS: &str = "findings";
const INCOMPLETE_STATUS: &str = "incomplete";

const CHECK_PARITY_DRIFT: &str = "parity_drift";
const CHECK_MISSING_TARGET: &str = "missing_target";
const CHECK_STALE_TARGET: &str = "stale_target";
const CHECK_WRONG_CARRIER: &str = "wrong_carrier";
const CHECK_CONTRADICTION: &str = "mechanical_contradiction";
const CHECK_DENSITY_OVERRUN: &str = "density_budget_overrun";
const CHECK_AUDIENCE_LEAK: &str = "audience_carrier_leak";
const CHECK_INCOMPLETE_COVERAGE: &str = "incomplete_coverage";

// OpenCode is a real co-host of the current Claude/OpenCode private manual
// (`CLAUDE.md` marker), but it is intentionally not a projection Carrier enum
// member.
const PRIVATE_MARKER_CARRIERS: &[&str] = &[
    "Codex",
    "Claude",
    "OpenCode",
    "Gemini",
    "Antigravity",
    "Cursor",
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct InstructionDriftFinding {
    pub(super) source_id: String,
    pub(super) source_revision: String,
    pub(super) source_hash: Option<String>,
    pub(super) target_path: Option<String>,
    pub(super) target_hash: Option<String>,
    pub(super) audience: String,
    pub(super) tier: String,
    pub(super) density_budget_name: String,
    pub(super) density_budget_bytes: u64,
    pub(super) check_kind: String,
    pub(super) evidence_span: String,
    pub(super) remediation_owner: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub(super) struct InstructionDriftSummary {
    pub(super) findings: usize,
    pub(super) incomplete: usize,
    pub(super) parity_drift: usize,
    pub(super) density_overrun: usize,
    pub(super) audience_leak: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct InstructionDriftReport {
    pub(super) schema_version: String,
    pub(super) manifest_path: String,
    pub(super) manifest_hash: String,
    pub(super) root: String,
    pub(super) status: String,
    pub(super) summary: InstructionDriftSummary,
    pub(super) findings: Vec<InstructionDriftFinding>,
}

/// Scan the declared manifest and emit a deterministic drift/density report.
#[cfg(test)]
pub(super) fn scan_instruction_drift(
    manifest_path: &Path,
) -> Result<InstructionDriftReport, String> {
    let status = scan_instruction_manifest(manifest_path)?;
    build_instruction_drift_report(&status)
}

pub(super) fn build_instruction_drift_report(
    status: &InstructionManifestStatus,
) -> Result<InstructionDriftReport, String> {
    let mut findings = Vec::new();

    let mut claimed_sources: BTreeSet<String> = BTreeSet::new();

    for source in &status.sources {
        claimed_sources.insert(normalize_declared_path(&source.source));
    }

    for source in &status.sources {
        if source.targets.is_empty() {
            findings.push(finding_for_source(
                source,
                None,
                None,
                CHECK_INCOMPLETE_COVERAGE,
                "targets=[]: projection coverage unscanned".to_string(),
            ));
        }

        evaluate_source_content_checks(source, source.content.as_ref(), &mut findings);
        evaluate_target_checks(source, &mut findings);
    }

    evaluate_required_sources(status, &claimed_sources, &mut findings);
    findings.sort_by(|left, right| {
        left.source_id
            .cmp(&right.source_id)
            .then_with(|| left.check_kind.cmp(&right.check_kind))
            .then_with(|| left.target_path.cmp(&right.target_path))
            .then_with(|| left.evidence_span.cmp(&right.evidence_span))
    });
    findings.dedup();

    let mut summary = InstructionDriftSummary {
        findings: findings.len(),
        ..InstructionDriftSummary::default()
    };
    for finding in &findings {
        match finding.check_kind.as_str() {
            CHECK_INCOMPLETE_COVERAGE => summary.incomplete += 1,
            CHECK_PARITY_DRIFT => summary.parity_drift += 1,
            CHECK_DENSITY_OVERRUN => summary.density_overrun += 1,
            CHECK_AUDIENCE_LEAK => summary.audience_leak += 1,
            _ => {}
        }
    }

    let status_label = if findings.is_empty() {
        CLEAN_STATUS
    } else if summary.incomplete > 0 {
        INCOMPLETE_STATUS
    } else {
        FINDINGS_STATUS
    };

    Ok(InstructionDriftReport {
        schema_version: DRIFT_SCHEMA.to_string(),
        manifest_path: status.manifest_path.clone(),
        manifest_hash: status.manifest_hash.clone(),
        root: status.root.clone(),
        status: status_label.to_string(),
        summary,
        findings,
    })
}

fn evaluate_required_sources(
    status: &InstructionManifestStatus,
    claimed_sources: &BTreeSet<String>,
    findings: &mut Vec<InstructionDriftFinding>,
) {
    for required in &status.required_sources {
        let normalized = normalize_declared_path(required);
        if claimed_sources.contains(&normalized) {
            continue;
        }
        findings.push(InstructionDriftFinding {
            source_id: format!("unregistered:{normalized}"),
            source_revision: String::new(),
            source_hash: None,
            target_path: Some(required.clone()),
            target_hash: None,
            audience: String::new(),
            tier: String::new(),
            density_budget_name: String::new(),
            density_budget_bytes: 0,
            check_kind: CHECK_INCOMPLETE_COVERAGE.to_string(),
            evidence_span: format!(
                "required_sources entry '{required}' is not claimed by any registered surface"
            ),
            remediation_owner: "repository-owner".to_string(),
        });
    }
}

fn evaluate_source_content_checks(
    source: &InstructionSourceStatus,
    content: Option<&String>,
    findings: &mut Vec<InstructionDriftFinding>,
) {
    let Some(text) = content else {
        return;
    };
    let bytes = text.len() as u64;
    if bytes > source.density_budget.bytes {
        findings.push(finding_for_source(
            source,
            None,
            None,
            CHECK_DENSITY_OVERRUN,
            format!(
                "bytes 0..{bytes} exceed budget {} ({})",
                source.density_budget.bytes, source.density_budget.name
            ),
        ));
    }

    if source.audience == "public" {
        for (start, end, matched) in carrier_mechanism_spans(text) {
            findings.push(finding_for_source(
                source,
                None,
                None,
                CHECK_AUDIENCE_LEAK,
                format!("bytes {start}..{end}: carrier mechanism '{matched}'"),
            ));
        }
        for (start, end, matched) in private_contract_spans(text) {
            findings.push(finding_for_source(
                source,
                None,
                None,
                CHECK_CONTRADICTION,
                format!("bytes {start}..{end}: public surface contains private marker '{matched}'"),
            ));
        }
    }
}

fn evaluate_target_checks(
    source: &InstructionSourceStatus,
    findings: &mut Vec<InstructionDriftFinding>,
) {
    for target in &source.targets {
        if !carrier_matches_path(&target.carrier, &target.path) {
            findings.push(finding_for_source(
                source,
                Some(target),
                target.hash.clone(),
                CHECK_WRONG_CARRIER,
                format!(
                    "declared carrier '{}' does not appear as a path segment in '{}'",
                    target.carrier, target.path
                ),
            ));
        }

        if !target.exists || target.status == "missing" {
            findings.push(finding_for_source(
                source,
                Some(target),
                None,
                CHECK_MISSING_TARGET,
                format!("declared target '{}' is missing", target.path),
            ));
            continue;
        }

        if source.status != CLEAN_STATUS || !source.exists {
            findings.push(finding_for_source(
                source,
                Some(target),
                target.hash.clone(),
                CHECK_STALE_TARGET,
                format!(
                    "target '{}' exists while source '{}' is {}",
                    target.path, source.source, source.status
                ),
            ));
        }

        if target.projection == "exact" {
            if let (Some(source_text), Some(target_text)) =
                (source.content.as_ref(), target.content.as_ref())
            {
                if source_text != target_text {
                    findings.push(finding_for_source(
                        source,
                        Some(target),
                        target.hash.clone(),
                        CHECK_PARITY_DRIFT,
                        format!(
                            "exact projection diverges: source_hash={:?} target_hash={:?}",
                            source.hash, target.hash
                        ),
                    ));
                }
            }
        }

        if source.audience == "public" {
            if let Some(target_text) = target.content.as_deref() {
                for (start, end, matched) in carrier_mechanism_spans(target_text) {
                    findings.push(finding_for_source(
                        source,
                        Some(target),
                        target.hash.clone(),
                        CHECK_AUDIENCE_LEAK,
                        format!("bytes {start}..{end}: carrier mechanism '{matched}'"),
                    ));
                }
                for (start, end, matched) in private_contract_spans(target_text) {
                    findings.push(finding_for_source(
                        source,
                        Some(target),
                        target.hash.clone(),
                        CHECK_CONTRADICTION,
                        format!(
                            "bytes {start}..{end}: public target contains private marker '{matched}'"
                        ),
                    ));
                }
            }
        }
    }
}

fn finding_for_source(
    source: &InstructionSourceStatus,
    target: Option<&InstructionTargetStatus>,
    target_hash: Option<String>,
    check_kind: &str,
    evidence_span: String,
) -> InstructionDriftFinding {
    InstructionDriftFinding {
        source_id: source.id.clone(),
        source_revision: source.adapter_version.clone(),
        source_hash: source.hash.clone(),
        target_path: target.map(|value| value.path.clone()),
        target_hash,
        audience: source.audience.clone(),
        tier: source.tier.clone(),
        density_budget_name: source.density_budget.name.clone(),
        density_budget_bytes: source.density_budget.bytes,
        check_kind: check_kind.to_string(),
        evidence_span,
        remediation_owner: source.remediation_owner.clone(),
    }
}

fn carrier_matches_path(carrier: &str, declared_path: &str) -> bool {
    let carrier = carrier.to_ascii_lowercase();
    Path::new(declared_path).components().any(|component| {
        matches!(component, Component::Normal(part) if part.to_string_lossy().to_ascii_lowercase() == carrier)
    })
}

fn normalize_declared_path(path: &str) -> String {
    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    parts.join("/")
}

fn carrier_mechanism_spans(text: &str) -> Vec<(usize, usize, String)> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(?:\btachi_staff\b|\bEnterWorktree\b|\bClaude Code\b|\bopencode\b|\bcodex:dispatch\b|\bcodex:rescue\b|`Agent` tool|/model\b)",
        )
        .expect("carrier mechanism regex")
    });
    re.find_iter(text)
        .map(|m| (m.start(), m.end(), m.as_str().to_string()))
        .collect()
}

fn private_contract_spans(text: &str) -> Vec<(usize, usize, String)> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        let carriers = PRIVATE_MARKER_CARRIERS.join("|");
        regex::Regex::new(&format!(
            r"(?i)(?:This file is (?:{carriers})-only\b|Claude/OpenCode-only\b|carrier-private manual\b)"
        ))
        .expect("private marker regex")
    });
    re.find_iter(text)
        .map(|m| (m.start(), m.end(), m.as_str().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let root = crate::utils::test_fixture_path(format!(
            "instruction-drift-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".agents")).expect("fixture root");
        (root.clone(), root.join(".agents/instruction-surfaces.json"))
    }

    fn write_manifest(path: &Path, value: &Value) {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(value).expect("manifest JSON"),
        )
        .expect("manifest file");
    }

    fn clean_manifest() -> Value {
        json!({
            "schema_version": "tachi.instruction_surfaces.v1",
            "root": "..",
            "required_sources": ["AGENTS.md", "CLAUDE.md"],
            "surfaces": [
                {
                    "id": "public-agents",
                    "source": "AGENTS.md",
                    "audience": "public",
                    "tier": "compressed-adapter",
                    "adapter_version": "fixture-v1",
                    "density_budget": {"name": "public-budget", "bytes": 4096},
                    "remediation_owner": "repository-owner",
                    "targets": [
                        {
                            "carrier": "cursor",
                            "path": "targets/cursor/AGENTS.md",
                            "ownership_mode": "source-owned",
                            "projection": "exact"
                        }
                    ]
                },
                {
                    "id": "claude-private-manual",
                    "source": "CLAUDE.md",
                    "audience": "carrier-private",
                    "tier": "expanded-manual",
                    "adapter_version": "fixture-v1",
                    "density_budget": {"name": "private-budget", "bytes": 8192},
                    "remediation_owner": "carrier-manual-owner",
                    "targets": [
                        {
                            "carrier": "claude",
                            "path": "targets/claude/CLAUDE.md",
                            "ownership_mode": "carrier-owned",
                            "projection": "carrier-adapted"
                        }
                    ]
                }
            ]
        })
    }

    fn write_clean_tree(root: &Path) {
        std::fs::write(root.join("AGENTS.md"), "public contract\n").expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private manual body\n").expect("source");
        std::fs::create_dir_all(root.join("targets/cursor")).expect("cursor dir");
        std::fs::create_dir_all(root.join("targets/claude")).expect("claude dir");
        std::fs::write(root.join("targets/cursor/AGENTS.md"), "public contract\n")
            .expect("exact target");
        std::fs::write(
            root.join("targets/claude/CLAUDE.md"),
            "private manual body\ncarrier note\n",
        )
        .expect("adapted target");
    }

    fn kinds(report: &InstructionDriftReport) -> Vec<&str> {
        report
            .findings
            .iter()
            .map(|finding| finding.check_kind.as_str())
            .collect()
    }

    #[test]
    fn clean_declared_source_targets_accounted_clean() {
        let (root, manifest_path) = fixture("clean");
        write_clean_tree(&root);
        write_manifest(&manifest_path, &clean_manifest());

        let report = scan_instruction_drift(&manifest_path).expect("clean scan");
        assert_eq!(report.status, CLEAN_STATUS);
        assert!(report.findings.is_empty(), "{:?}", report.findings);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn one_byte_exact_projection_edit_emits_parity_finding() {
        let (root, manifest_path) = fixture("parity");
        write_clean_tree(&root);
        std::fs::write(root.join("targets/cursor/AGENTS.md"), "public contract!\n")
            .expect("mutated target");
        write_manifest(&manifest_path, &clean_manifest());

        let report = scan_instruction_drift(&manifest_path).expect("parity scan");
        assert!(
            kinds(&report).contains(&CHECK_PARITY_DRIFT),
            "{:?}",
            kinds(&report)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stored_content_snapshot_ignores_post_scan_target_mutation() {
        let (root, manifest_path) = fixture("snapshot");
        write_clean_tree(&root);
        write_manifest(&manifest_path, &clean_manifest());

        let status = scan_instruction_manifest(&manifest_path).expect("clean status");
        let serialized = serde_json::to_value(&status).expect("status JSON");
        assert!(!serialized["sources"][0]
            .as_object()
            .expect("source object")
            .contains_key("content"));
        assert!(!serialized["sources"][0]["targets"][0]
            .as_object()
            .expect("target object")
            .contains_key("content"));

        std::fs::write(root.join("targets/cursor/AGENTS.md"), "public contract!\n")
            .expect("post-scan mutation");
        let report = build_instruction_drift_report(&status).expect("snapshot report");
        assert_eq!(report.status, CLEAN_STATUS);
        assert!(report.findings.is_empty(), "{:?}", report.findings);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_and_wrong_carrier_are_distinct_findings() {
        let (root, manifest_path) = fixture("target-kinds");
        write_clean_tree(&root);
        let mut manifest = clean_manifest();
        manifest["surfaces"][0]["targets"] = json!([
            {
                "carrier": "cursor",
                "path": "targets/cursor/missing.md",
                "ownership_mode": "source-owned",
                "projection": "exact"
            },
            {
                "carrier": "codex",
                "path": "targets/cursor/AGENTS.md",
                "ownership_mode": "source-owned",
                "projection": "exact"
            }
        ]);
        // Drop the private surface targets to keep this fixture focused; keep
        // one clean private target so incomplete coverage does not dominate.
        write_manifest(&manifest_path, &manifest);

        let report = scan_instruction_drift(&manifest_path).expect("target kinds");
        let kind_set: BTreeSet<&str> = kinds(&report).into_iter().collect();
        assert!(kind_set.contains(CHECK_MISSING_TARGET), "{kind_set:?}");
        assert!(kind_set.contains(CHECK_WRONG_CARRIER), "{kind_set:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn existing_target_with_missing_source_emits_stale_target() {
        let (root, manifest_path) = fixture("stale-target");
        write_clean_tree(&root);
        std::fs::remove_file(root.join("AGENTS.md")).expect("missing source");
        write_manifest(&manifest_path, &clean_manifest());

        let report = scan_instruction_drift(&manifest_path).expect("stale target scan");
        assert!(
            report.findings.iter().any(|finding| {
                finding.check_kind == CHECK_STALE_TARGET
                    && finding.target_path.as_deref() == Some("targets/cursor/AGENTS.md")
            }),
            "{:?}",
            report.findings
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn generic_carrier_names_are_not_mechanism_matches() {
        let prose = "Claude and Codex are model families; this model is general prose.";
        assert!(carrier_mechanism_spans(prose).is_empty());
    }

    #[test]
    fn compressed_over_budget_emits_density_finding() {
        let (root, manifest_path) = fixture("density-case");
        let oversized = format!("{}\n", "x".repeat(300));
        std::fs::write(root.join("AGENTS.md"), &oversized).expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private manual body\n").expect("source");
        std::fs::create_dir_all(root.join("targets/cursor")).expect("dir");
        std::fs::create_dir_all(root.join("targets/claude")).expect("dir");
        std::fs::write(root.join("targets/cursor/AGENTS.md"), &oversized).expect("target");
        std::fs::write(
            root.join("targets/claude/CLAUDE.md"),
            "private manual body\n",
        )
        .expect("target");

        let mut manifest = clean_manifest();
        manifest["surfaces"][0]["density_budget"]["bytes"] = json!(64);
        write_manifest(&manifest_path, &manifest);

        let report = scan_instruction_drift(&manifest_path).expect("density scan");
        let kind_set: BTreeSet<&str> = kinds(&report).into_iter().collect();
        assert!(kind_set.contains(CHECK_DENSITY_OVERRUN), "{kind_set:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn expanded_manual_uses_only_its_own_budget() {
        let (root, manifest_path) = fixture("expanded-budget");
        let private = "y".repeat(200);
        std::fs::write(root.join("AGENTS.md"), "public contract\n").expect("source");
        std::fs::write(root.join("CLAUDE.md"), &private).expect("source");
        std::fs::create_dir_all(root.join("targets/cursor")).expect("dir");
        std::fs::create_dir_all(root.join("targets/claude")).expect("dir");
        std::fs::write(root.join("targets/cursor/AGENTS.md"), "public contract\n").expect("target");
        std::fs::write(root.join("targets/claude/CLAUDE.md"), &private).expect("target");

        let mut manifest = clean_manifest();
        // Public budget is tiny but public content fits; private is large but
        // its own budget permits it. Compressed budget must not judge private.
        manifest["surfaces"][0]["density_budget"]["bytes"] = json!(64);
        manifest["surfaces"][1]["density_budget"]["bytes"] = json!(1024);
        write_manifest(&manifest_path, &manifest);

        let report = scan_instruction_drift(&manifest_path).expect("budget scan");
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.source_id == "claude-private-manual"
                    && finding.check_kind == CHECK_DENSITY_OVERRUN),
            "{:?}",
            report.findings
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn public_target_with_carrier_command_emits_audience_leak() {
        let (root, manifest_path) = fixture("audience-leak");
        write_clean_tree(&root);
        std::fs::write(
            root.join("targets/cursor/AGENTS.md"),
            "public contract\nUse tachi_staff(action='start') here.\n",
        )
        .expect("leaky target");
        // Keep exact projection parity out of this fixture.
        let mut manifest = clean_manifest();
        manifest["surfaces"][0]["targets"][0]["projection"] = json!("carrier-adapted");
        manifest["surfaces"][0]["targets"][0]["ownership_mode"] = json!("carrier-owned");
        write_manifest(&manifest_path, &manifest);

        let report = scan_instruction_drift(&manifest_path).expect("leak scan");
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.check_kind == CHECK_AUDIENCE_LEAK
                    && finding.target_path.as_deref() == Some("targets/cursor/AGENTS.md")),
            "{:?}",
            report.findings
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_prose_is_not_a_private_marker() {
        let (root, manifest_path) = fixture("private-marker-boundary");
        write_clean_tree(&root);
        write_manifest(&manifest_path, &clean_manifest());

        std::fs::write(root.join("AGENTS.md"), "This file is read-only.\n")
            .expect("generic prose source");
        std::fs::write(
            root.join("targets/cursor/AGENTS.md"),
            "This file is read-only.\n",
        )
        .expect("generic prose target");
        let status = scan_instruction_manifest(&manifest_path).expect("generic prose scan");
        let report = build_instruction_drift_report(&status).expect("generic prose report");
        assert!(!kinds(&report).contains(&CHECK_CONTRADICTION), "{report:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn private_marker_on_public_source_is_source_bound() {
        let (root, manifest_path) = fixture("public-source-private-marker");
        write_clean_tree(&root);
        std::fs::write(root.join("AGENTS.md"), "This file is Claude-only.\n")
            .expect("carrier-private source");
        std::fs::write(
            root.join("targets/cursor/AGENTS.md"),
            "neutral public projection\n",
        )
        .expect("neutral target");
        let mut manifest = clean_manifest();
        manifest["surfaces"][0]["targets"][0]["projection"] = json!("carrier-adapted");
        manifest["surfaces"][0]["targets"][0]["ownership_mode"] = json!("carrier-owned");
        write_manifest(&manifest_path, &manifest);

        let status = scan_instruction_manifest(&manifest_path).expect("source marker scan");
        let report = build_instruction_drift_report(&status).expect("source marker report");
        assert!(
            report.findings.iter().any(|finding| {
                finding.check_kind == CHECK_CONTRADICTION && finding.target_path.is_none()
            }),
            "{report:?}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn claude_onlyish_is_not_a_private_marker() {
        assert!(private_contract_spans("This file is Claude-onlyish.").is_empty());
    }

    #[test]
    fn compound_private_marker_suffix_is_not_a_match() {
        assert!(private_contract_spans("Claude/OpenCode-onlyish").is_empty());
    }

    #[test]
    fn generic_private_marker_suffix_is_not_a_match() {
        assert!(private_contract_spans("carrier-private manualish").is_empty());
    }

    #[test]
    fn canonical_private_marker_carriers_and_phrases_match() {
        for carrier in PRIVATE_MARKER_CARRIERS {
            let text = format!("This file is {carrier}-only.");
            assert_eq!(private_contract_spans(&text).len(), 1, "{text}");
        }
        for phrase in ["Claude/OpenCode-only", "carrier-private manual"] {
            assert_eq!(private_contract_spans(phrase).len(), 1, "{phrase}");
        }
        assert!(private_contract_spans("This file is Rust-only.").is_empty());
    }

    #[test]
    fn public_carrier_adapted_target_private_marker_is_reported_on_target() {
        let (root, manifest_path) = fixture("public-target-private-marker");
        write_clean_tree(&root);
        std::fs::write(
            root.join("targets/cursor/AGENTS.md"),
            "This file is Claude-only.\n",
        )
        .expect("private target marker");
        let mut manifest = clean_manifest();
        manifest["surfaces"][0]["targets"][0]["projection"] = json!("carrier-adapted");
        manifest["surfaces"][0]["targets"][0]["ownership_mode"] = json!("carrier-owned");
        write_manifest(&manifest_path, &manifest);

        let status = scan_instruction_manifest(&manifest_path).expect("target marker scan");
        let report = build_instruction_drift_report(&status).expect("target marker report");
        assert!(
            report.findings.iter().any(|finding| {
                finding.check_kind == CHECK_CONTRADICTION
                    && finding.target_path.as_deref() == Some("targets/cursor/AGENTS.md")
            }),
            "{report:?}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unregistered_required_source_is_incomplete_coverage() {
        let (root, manifest_path) = fixture("unregistered");
        write_clean_tree(&root);
        std::fs::write(root.join("EXTRA.md"), "orphan surface\n").expect("extra");
        let mut manifest = clean_manifest();
        manifest["required_sources"] = json!(["AGENTS.md", "CLAUDE.md", "EXTRA.md"]);
        write_manifest(&manifest_path, &manifest);

        let report = scan_instruction_drift(&manifest_path).expect("unregistered scan");
        assert_eq!(report.status, INCOMPLETE_STATUS);
        assert!(
            report.findings.iter().any(|finding| {
                finding.check_kind == CHECK_INCOMPLETE_COVERAGE
                    && finding.source_id == "unregistered:EXTRA.md"
            }),
            "{:?}",
            report.findings
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unchanged_repeated_scans_are_canonically_identical() {
        let (root, manifest_path) = fixture("stable");
        write_clean_tree(&root);
        write_manifest(&manifest_path, &clean_manifest());

        let first = scan_instruction_drift(&manifest_path).expect("first");
        let second = scan_instruction_drift(&manifest_path).expect("second");
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn empty_targets_are_incomplete_never_clean() {
        let (root, manifest_path) = fixture("empty-targets");
        std::fs::write(root.join("AGENTS.md"), "public contract\n").expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private manual body\n").expect("source");
        let mut manifest = clean_manifest();
        manifest["surfaces"][0]["targets"] = json!([]);
        manifest["surfaces"][1]["targets"] = json!([]);
        write_manifest(&manifest_path, &manifest);

        let report = scan_instruction_drift(&manifest_path).expect("empty targets");
        assert_eq!(report.status, INCOMPLETE_STATUS);
        assert!(
            report
                .findings
                .iter()
                .filter(|finding| finding.check_kind == CHECK_INCOMPLETE_COVERAGE)
                .count()
                >= 2,
            "{:?}",
            report.findings
        );

        let _ = std::fs::remove_dir_all(root);
    }
}

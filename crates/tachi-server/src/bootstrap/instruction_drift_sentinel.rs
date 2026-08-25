//! Report-only instruction drift / density sentinel (#1304).
//!
//! Consumes [`scan_instruction_manifest`] status plus re-reads of declared
//! paths. Never writes sources, projections, approvals, or promotions.

#[cfg(test)]
use super::instruction_manifest::scan_instruction_manifest;
use super::instruction_manifest::{
    InstructionManifestStatus, InstructionSourceStatus, InstructionTargetStatus,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::sync::OnceLock;

const DRIFT_SCHEMA: &str = "tachi.instruction_drift.v1";
const CLEAN_STATUS: &str = "clean";
const FINDINGS_STATUS: &str = "findings";
const INCOMPLETE_STATUS: &str = "incomplete";

const CHECK_PARITY_DRIFT: &str = "parity_drift";
const CHECK_MISSING_TARGET: &str = "missing_target";
const CHECK_TARGET_ALIASES_SOURCE: &str = "target_aliases_source";
const CHECK_STALE_TARGET: &str = "stale_target";
const CHECK_UNREADABLE_TARGET: &str = "unreadable_target";
const CHECK_WRONG_CARRIER: &str = "wrong_carrier";
const CHECK_DUPLICATE_BLOCK: &str = "duplicate_block";
const CHECK_CONTRADICTION: &str = "mechanical_contradiction";
const CHECK_DENSITY_OVERRUN: &str = "density_budget_overrun";
const CHECK_DATED_CASE_BODY: &str = "dated_case_body";
const CHECK_AUDIENCE_LEAK: &str = "audience_carrier_leak";
const CHECK_INCOMPLETE_COVERAGE: &str = "incomplete_coverage";
const CHECK_UNREADABLE_MANIFEST_ENTRY: &str = "unreadable_manifest_entry";

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

/// A finding scoped to the manifest itself rather than to a fully-inspected
/// source (#1710): `finding_for_source` needs a live `&InstructionSourceStatus`
/// (resolved_path/hash/bytes/exists/status all populated) to build an
/// [`InstructionDriftFinding`], and a declared source or target the tool
/// could not read, parse, or validate never produces one. This variant
/// carries only what the manifest JSON itself declared (surface id, role,
/// declared path, carrier) plus the read error -- it never fakes a source
/// identity for input that was never successfully inspected.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct ManifestDriftFinding {
    pub(super) surface_id: String,
    /// `"source"` or `"target"`.
    pub(super) role: String,
    pub(super) declared_path: String,
    pub(super) carrier: Option<String>,
    pub(super) check_kind: String,
    pub(super) evidence_span: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub(super) struct InstructionDriftSummary {
    pub(super) findings: usize,
    pub(super) incomplete: usize,
    pub(super) parity_drift: usize,
    pub(super) density_overrun: usize,
    pub(super) audience_leak: usize,
    pub(super) unreadable_inputs: usize,
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
    /// "The manifest itself is broken" findings, kept visibly distinct from
    /// `findings`' "this surface is broken" ones (#1710).
    pub(super) manifest_findings: Vec<ManifestDriftFinding>,
}

/// Refuse fix/apply/promote/delete shaped requests. Report-only contract.
///
/// Unreachable from the CLI today, deliberately (#1710): `HarnessAction`
/// (`tachi-bootstrap/src/cli/maintenance_actions.rs`) has exactly one
/// variant, `Status`, and clap's `Subcommand` derive means a mutation token
/// (`fix`/`apply`/…) cannot even be *parsed* into a `HarnessAction` — `tachi
/// harness fix` is rejected by clap itself before `run_harness_command`
/// (`harness_cli.rs`) ever runs. The report-only contract is structurally
/// true right now, not runtime-enforced, so there is no live action string
/// to wire this against; stringifying `HarnessAction`'s `Debug` output and
/// checking that would guard an input no caller can actually construct,
/// which is decoration, not a gate.
///
/// This function stays, unreachable, as the pre-built gate for the day a
/// mutation-shaped `HarnessAction` variant is added (a separate CLI-surface
/// decision, out of scope here): wire the call in then, instead of
/// re-deriving the refusal wording and blocked-verb list from scratch. Its
/// contract is exercised directly by
/// `tests::fix_shaped_request_refuses_and_leaves_bytes_identical` below,
/// which is what's actually tested today, not a CLI path.
#[allow(dead_code)]
pub(super) fn refuse_instruction_mutation(action: &str) -> Result<(), String> {
    let normalized = action.trim().to_ascii_lowercase();
    let blocked = [
        "fix", "apply", "promote", "delete", "write", "amend", "repair", "autofix", "--fix",
        "--apply",
    ];
    if blocked
        .iter()
        .any(|needle| normalized == *needle || normalized.contains(needle))
    {
        return Err(format!(
            "instruction drift sentinel is report-only; refusing mutation action '{action}'"
        ));
    }
    Ok(())
}

pub(super) fn build_instruction_drift_report(
    status: &InstructionManifestStatus,
) -> Result<InstructionDriftReport, String> {
    let mut findings = Vec::new();

    let mut source_contents: BTreeMap<String, String> = BTreeMap::new();
    let mut claimed_sources: BTreeSet<String> = BTreeSet::new();

    for source in &status.sources {
        claimed_sources.insert(normalize_declared_path(&source.source));
        if source.exists && source.status == CLEAN_STATUS {
            match std::fs::read_to_string(&source.resolved_path) {
                Ok(text) => {
                    source_contents.insert(source.id.clone(), text);
                }
                Err(error) => {
                    findings.push(finding_for_source(
                        source,
                        None,
                        None,
                        CHECK_UNREADABLE_TARGET,
                        format!("source '{}': unreadable ({error})", source.resolved_path),
                    ));
                }
            }
        }
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

        evaluate_source_content_checks(source, source_contents.get(&source.id), &mut findings);
        evaluate_target_checks(source, &claimed_sources, &source_contents, &mut findings);
    }

    evaluate_required_sources(status, &claimed_sources, &mut findings);
    evaluate_duplicate_blocks(&status.sources, &source_contents, &mut findings);
    evaluate_source_path_contradictions(&status.sources, &mut findings);

    findings.sort_by(|left, right| {
        left.source_id
            .cmp(&right.source_id)
            .then_with(|| left.check_kind.cmp(&right.check_kind))
            .then_with(|| left.target_path.cmp(&right.target_path))
            .then_with(|| left.evidence_span.cmp(&right.evidence_span))
    });
    findings.dedup();

    // #1710: every declared source/target the manifest scan could not
    // read, parse, or validate becomes a manifest-scoped finding here,
    // distinct from the source-scoped `findings` above, instead of
    // silently vanishing along with the abort it used to cause.
    let mut manifest_findings: Vec<ManifestDriftFinding> = status
        .unreadable
        .iter()
        .map(|entry| ManifestDriftFinding {
            surface_id: entry.surface_id.clone(),
            role: entry.role.clone(),
            declared_path: entry.declared_path.clone(),
            carrier: entry.carrier.clone(),
            check_kind: CHECK_UNREADABLE_MANIFEST_ENTRY.to_string(),
            evidence_span: entry.error.clone(),
        })
        .collect();
    manifest_findings.sort_by(|left, right| {
        left.surface_id
            .cmp(&right.surface_id)
            .then_with(|| left.role.cmp(&right.role))
            .then_with(|| left.declared_path.cmp(&right.declared_path))
    });

    let mut summary = InstructionDriftSummary {
        findings: findings.len(),
        unreadable_inputs: manifest_findings.len(),
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

    let status_label = if findings.is_empty() && manifest_findings.is_empty() {
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
        manifest_findings,
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

    if source.tier == "compressed-adapter" {
        for (start, end, matched) in dated_case_spans(text) {
            findings.push(finding_for_source(
                source,
                None,
                None,
                CHECK_DATED_CASE_BODY,
                format!("bytes {start}..{end}: dated case body '{matched}'"),
            ));
        }
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
    claimed_sources: &BTreeSet<String>,
    source_contents: &BTreeMap<String, String>,
    findings: &mut Vec<InstructionDriftFinding>,
) {
    let source_text = source_contents.get(&source.id);
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

        if claimed_sources.contains(&normalize_declared_path(&target.path)) {
            findings.push(finding_for_source(
                source,
                Some(target),
                target.hash.clone(),
                CHECK_TARGET_ALIASES_SOURCE,
                format!(
                    "target path '{}' resolves onto a registered source path",
                    target.path
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

        let target_text = match std::fs::read_to_string(&target.resolved_path) {
            Ok(text) => text,
            Err(error) => {
                findings.push(finding_for_source(
                    source,
                    Some(target),
                    target.hash.clone(),
                    CHECK_UNREADABLE_TARGET,
                    format!("target '{}': unreadable ({error})", target.resolved_path),
                ));
                continue;
            }
        };

        if target.projection == "exact" {
            if let Some(source_text) = source_text {
                if source_text != &target_text {
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
            for (start, end, matched) in carrier_mechanism_spans(&target_text) {
                findings.push(finding_for_source(
                    source,
                    Some(target),
                    target.hash.clone(),
                    CHECK_AUDIENCE_LEAK,
                    format!("bytes {start}..{end}: carrier mechanism '{matched}'"),
                ));
            }
        }
    }
}

/// A block (#1710) is a level-2 (`##`) Markdown section: its heading plus
/// all content up to the next heading of the same or higher level (`#` or
/// `##`; a nested `###`+ subsection does not end the block). Text before the
/// first `##` heading (a title, preamble, or a bare `#` heading) belongs to
/// no block and never participates in duplicate detection -- bullet-level
/// detection is explicitly out of scope for this leaf.
fn extract_h2_blocks(text: &str) -> Vec<(usize, usize, &str)> {
    let mut boundaries: Vec<(usize, usize)> = Vec::new(); // (byte offset, heading level)
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if let Some(level) = markdown_heading_level(content) {
            if level <= 2 {
                boundaries.push((offset, level));
            }
        }
        offset += line.len();
    }

    let mut blocks = Vec::new();
    for (index, &(start, level)) in boundaries.iter().enumerate() {
        if level != 2 {
            continue;
        }
        let end = boundaries
            .get(index + 1)
            .map_or(text.len(), |&(next_start, _)| next_start);
        blocks.push((start, end, &text[start..end]));
    }
    blocks
}

fn markdown_heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|&ch| ch == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if rest.is_empty() || rest.starts_with(' ') {
        Some(hashes)
    } else {
        None
    }
}

fn evaluate_duplicate_blocks(
    sources: &[InstructionSourceStatus],
    source_contents: &BTreeMap<String, String>,
    findings: &mut Vec<InstructionDriftFinding>,
) {
    let mut by_normalized: BTreeMap<String, Vec<(&InstructionSourceStatus, usize, usize)>> =
        BTreeMap::new();
    for source in sources {
        let Some(text) = source_contents.get(&source.id) else {
            continue;
        };
        for (start, end, block_text) in extract_h2_blocks(text) {
            if block_text.trim().is_empty() {
                continue;
            }
            let normalized = normalize_instruction_text(block_text);
            if normalized.is_empty() {
                continue;
            }
            by_normalized
                .entry(normalized)
                .or_default()
                .push((source, start, end));
        }
    }

    for group in by_normalized.values() {
        if group.len() < 2 {
            continue;
        }
        let spans: Vec<String> = group
            .iter()
            .map(|(source, start, end)| format!("{}:bytes {start}..{end}", source.id))
            .collect();
        for (source, start, end) in group {
            findings.push(finding_for_source(
                source,
                None,
                None,
                CHECK_DUPLICATE_BLOCK,
                format!(
                    "normalized duplicate ## section across {}; local span bytes {start}..{end}",
                    spans.join(" | "),
                ),
            ));
        }
    }
}

fn evaluate_source_path_contradictions(
    sources: &[InstructionSourceStatus],
    findings: &mut Vec<InstructionDriftFinding>,
) {
    let mut by_resolved: BTreeMap<&str, Vec<&InstructionSourceStatus>> = BTreeMap::new();
    for source in sources {
        by_resolved
            .entry(source.resolved_path.as_str())
            .or_default()
            .push(source);
    }
    for (path, group) in by_resolved {
        if group.len() < 2 {
            continue;
        }
        let ids: Vec<&str> = group.iter().map(|source| source.id.as_str()).collect();
        for source in group {
            findings.push(finding_for_source(
                source,
                None,
                None,
                CHECK_CONTRADICTION,
                format!(
                    "multiple surfaces {:?} claim resolved source '{}'",
                    ids, path
                ),
            ));
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

fn normalize_instruction_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// A `##`-section-independent notion of "sentence": the run of text bounded
/// by the enclosing line (bullets in this corpus are one line each) and, on
/// top of that, the nearest `. `/`! `/`? ` sentence punctuation on either
/// side of `start..end`. A long bullet frequently contains a ratification
/// clause and a separately-punctuated incident clause; scoping corroboration
/// to the *sentence* rather than the whole line/bullet is what keeps those
/// two apart (see `AGENTS.md`'s `owner-ratified 2026-08-03` clause, which
/// sits several sentences upstream of that same bullet's `Evidence:
/// ... 2026-08-02` clause).
fn enclosing_sentence(text: &str, start: usize, end: usize) -> (usize, usize) {
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[end..].find('\n').map_or(text.len(), |i| end + i);

    let before = &text[line_start..start];
    let sentence_start = [". ", "! ", "? "]
        .iter()
        .filter_map(|needle| before.rfind(needle).map(|i| i + needle.len()))
        .max()
        .map_or(line_start, |offset| line_start + offset);

    let after = &text[end..line_end];
    let sentence_end = [". ", "! ", "? "]
        .iter()
        .filter_map(|needle| after.find(needle).map(|i| i + 1))
        .min()
        .map_or(line_end, |offset| end + offset);

    (sentence_start, sentence_end)
}

fn dated_case_spans(text: &str) -> Vec<(usize, usize, String)> {
    static DATE_RE: OnceLock<regex::Regex> = OnceLock::new();
    let date_re = DATE_RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(?:\b20\d{2}-\d{2}-\d{2}\b|\b(?:jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\s+\d{1,2},\s+20\d{2}\b)",
        )
        .expect("dated case regex")
    });
    // Frozen (#1710): a date is only a dated *case* -- not a governance/
    // ratification timestamp -- if its enclosing sentence also carries a
    // corroborating signal: an explicit `Evidence:` marker or an issue/PR
    // reference. The `owner-ratified` prefix is deliberately not part of
    // the date pattern above: a ratification date is never itself an
    // incident, regardless of corroboration. `DATE_RE` alone can't tell a
    // ratification date from an incident date -- it matches the bare digits
    // either way -- so any date immediately governed by an `owner-ratified`
    // prefix is excluded up front, *before* the corroboration check, so a
    // same-sentence `Evidence:`/`#NNNN` elsewhere in the sentence can't pull
    // the ratification date back in.
    static OWNER_RATIFIED_DATE_RE: OnceLock<regex::Regex> = OnceLock::new();
    let owner_ratified_date_re = OWNER_RATIFIED_DATE_RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)owner-ratified\s*:?\s*(?:\b20\d{2}-\d{2}-\d{2}\b|\b(?:jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\s+\d{1,2},\s+20\d{2}\b)",
        )
        .expect("owner-ratified date regex")
    });
    let ratified_spans: Vec<(usize, usize)> = owner_ratified_date_re
        .find_iter(text)
        .map(|m| (m.start(), m.end()))
        .collect();

    static CORROBORATION_RE: OnceLock<regex::Regex> = OnceLock::new();
    let corroboration_re = CORROBORATION_RE
        .get_or_init(|| regex::Regex::new(r"Evidence:|#\d+").expect("corroboration regex"));

    date_re
        .find_iter(text)
        .filter_map(|m| {
            let governed_by_ratification = ratified_spans
                .iter()
                .any(|(start, end)| *start <= m.start() && *end >= m.end());
            if governed_by_ratification {
                return None;
            }
            let (sentence_start, sentence_end) = enclosing_sentence(text, m.start(), m.end());
            if corroboration_re.is_match(&text[sentence_start..sentence_end]) {
                Some((m.start(), m.end(), m.as_str().to_string()))
            } else {
                None
            }
        })
        .collect()
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
        regex::Regex::new(
            r"(?i)(?:This file is .+?-only|Claude/OpenCode-only|carrier-private manual)",
        )
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
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// Test-only manifest-path -> drift-report composition. The production
    /// call path (`harness_cli.rs`) already holds `InstructionManifestStatus`
    /// separately and calls `build_instruction_drift_report` on it directly,
    /// so a `pub(super)` one-shot wrapper had no non-test caller and was
    /// dropped (#1710); this local helper keeps the fixture-heavy tests
    /// below from repeating the two-call chain everywhere.
    fn scan_drift_for_test(manifest_path: &Path) -> Result<InstructionDriftReport, String> {
        let status = scan_instruction_manifest(manifest_path)?;
        build_instruction_drift_report(&status)
    }

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

    fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).expect("read dir") {
                let entry = entry.expect("dir entry");
                let path = entry.path();
                let rel = path
                    .strip_prefix(root)
                    .expect("prefix")
                    .to_string_lossy()
                    .into_owned();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.insert(rel, std::fs::read(&path).expect("read file"));
                }
            }
        }
        walk(root, root, &mut out);
        out
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

        let report = scan_drift_for_test(&manifest_path).expect("clean scan");
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

        let report = scan_drift_for_test(&manifest_path).expect("parity scan");
        assert!(
            kinds(&report).contains(&CHECK_PARITY_DRIFT),
            "{:?}",
            kinds(&report)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_extra_wrong_carrier_are_distinct_findings() {
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
                "carrier": "cursor",
                "path": "AGENTS.md",
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

        let report = scan_drift_for_test(&manifest_path).expect("target kinds");
        let kind_set: BTreeSet<&str> = kinds(&report).into_iter().collect();
        assert!(kind_set.contains(CHECK_MISSING_TARGET), "{kind_set:?}");
        assert!(
            kind_set.contains(CHECK_TARGET_ALIASES_SOURCE),
            "{kind_set:?}"
        );
        assert!(kind_set.contains(CHECK_WRONG_CARRIER), "{kind_set:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn target_aliasing_its_own_source_is_reported_under_the_renamed_check() {
        // A target whose declared path is textually equal to a registered
        // source's declared path is source aliasing, not surplus-target
        // accounting (#1710): the finding must carry the literal
        // `target_aliases_source` check_kind string, not the old
        // `extra_target` name.
        let (root, manifest_path) = fixture("target-alias-name");
        write_clean_tree(&root);
        let mut manifest = clean_manifest();
        manifest["surfaces"][0]["targets"] = json!([
            {
                "carrier": "cursor",
                "path": "AGENTS.md",
                "ownership_mode": "source-owned",
                "projection": "exact"
            }
        ]);
        write_manifest(&manifest_path, &manifest);

        let report = scan_drift_for_test(&manifest_path).expect("alias scan");
        let value = serde_json::to_value(&report).expect("report JSON");
        let check_kinds: Vec<&str> = value["findings"]
            .as_array()
            .expect("findings array")
            .iter()
            .map(|finding| finding["check_kind"].as_str().expect("check_kind"))
            .collect();
        assert!(
            check_kinds.contains(&"target_aliases_source"),
            "{check_kinds:?}"
        );
        assert!(!check_kinds.contains(&"extra_target"), "{check_kinds:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn two_sources_sharing_exactly_one_h2_section_both_get_duplicate_findings() {
        // Frozen (#1710): a block is a `##` section (heading through the
        // next `#`/`##` heading), not a whole file. Two sources here share
        // exactly one `##` section and differ everywhere else; only that
        // section should produce duplicate findings.
        let (root, manifest_path) = fixture("duplicate-section");
        let agents_md = "# Public Contract\n\n## Shared Rule\n\nSome   shared   guidance text here.\n\n## Public Only\n\nPublic-only content unique to this file.\n";
        let claude_md = "# Private Manual\n\n## Shared Rule\n\nsome shared guidance text here.\n\n## Private Only\n\nPrivate-only content unique to this file.\n";
        std::fs::write(root.join("AGENTS.md"), agents_md).expect("source");
        std::fs::write(root.join("CLAUDE.md"), claude_md).expect("source");
        std::fs::create_dir_all(root.join("targets/cursor")).expect("dir");
        std::fs::create_dir_all(root.join("targets/claude")).expect("dir");
        std::fs::write(root.join("targets/cursor/AGENTS.md"), agents_md).expect("target");
        std::fs::write(root.join("targets/claude/CLAUDE.md"), claude_md).expect("target");
        write_manifest(&manifest_path, &clean_manifest());

        let report = scan_drift_for_test(&manifest_path).expect("duplicate scan");
        let duplicate: Vec<_> = report
            .findings
            .iter()
            .filter(|finding| finding.check_kind == CHECK_DUPLICATE_BLOCK)
            .collect();
        assert_eq!(duplicate.len(), 2, "{:?}", report.findings);
        let joined = duplicate
            .iter()
            .map(|finding| finding.evidence_span.as_str())
            .collect::<Vec<_>>()
            .join(" || ");
        assert!(joined.contains("public-agents:bytes"), "{joined}");
        assert!(joined.contains("claude-private-manual:bytes"), "{joined}");
        // The differing "Public Only"/"Private Only" sections must not
        // spuriously match, and the file-level (old) grouping must not
        // fire either -- exactly one shared section, exactly two findings.

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bullet_level_sharing_without_a_shared_h2_section_is_not_a_duplicate() {
        // Frozen (#1710): bullet-level detection is explicitly out of scope
        // for this leaf. Two sources sharing one identical bullet inside
        // otherwise-different `##` sections must not produce a duplicate
        // finding -- only whole-section equality counts.
        let (root, manifest_path) = fixture("bullet-only-shared");
        let agents_md =
            "# Public Contract\n\n## Public Section\n\n- Shared bullet phrasing here.\n- Public-only extra bullet.\n";
        let claude_md =
            "# Private Manual\n\n## Private Section\n\n- Shared bullet phrasing here.\n- Private-only extra bullet.\n";
        std::fs::write(root.join("AGENTS.md"), agents_md).expect("source");
        std::fs::write(root.join("CLAUDE.md"), claude_md).expect("source");
        std::fs::create_dir_all(root.join("targets/cursor")).expect("dir");
        std::fs::create_dir_all(root.join("targets/claude")).expect("dir");
        std::fs::write(root.join("targets/cursor/AGENTS.md"), agents_md).expect("target");
        std::fs::write(root.join("targets/claude/CLAUDE.md"), claude_md).expect("target");
        write_manifest(&manifest_path, &clean_manifest());

        let report = scan_drift_for_test(&manifest_path).expect("bullet-only scan");
        assert!(
            !kinds(&report).contains(&CHECK_DUPLICATE_BLOCK),
            "{:?}",
            report.findings
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn compressed_over_budget_with_dated_case_emits_density_and_case() {
        let (root, manifest_path) = fixture("density-case");
        // Frozen (#1710): a bare date is not a dated case; the sentence
        // must also carry an `Evidence:`/`#NNNN` corroborating signal.
        let oversized = format!(
            "{}\nEvidence: an incident on 2026-08-02 with case body (#1710).\n",
            "x".repeat(300)
        );
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

        let report = scan_drift_for_test(&manifest_path).expect("density scan");
        let kind_set: BTreeSet<&str> = kinds(&report).into_iter().collect();
        assert!(kind_set.contains(CHECK_DENSITY_OVERRUN), "{kind_set:?}");
        assert!(kind_set.contains(CHECK_DATED_CASE_BODY), "{kind_set:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn owner_ratified_date_without_corroboration_is_not_a_dated_case() {
        // Frozen (#1710), rule 1: `owner-ratified` is removed from the
        // incident pattern entirely -- a ratification date is never itself
        // an incident, corroborated or not.
        let (root, manifest_path) = fixture("owner-ratified-no-evidence");
        std::fs::write(
            root.join("AGENTS.md"),
            "- T2 default (owner-ratified 2026-07-14): supervised write lane.\n",
        )
        .expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private manual body\n").expect("source");
        std::fs::create_dir_all(root.join("targets/cursor")).expect("dir");
        std::fs::create_dir_all(root.join("targets/claude")).expect("dir");
        std::fs::write(
            root.join("targets/cursor/AGENTS.md"),
            "- T2 default (owner-ratified 2026-07-14): supervised write lane.\n",
        )
        .expect("target");
        std::fs::write(
            root.join("targets/claude/CLAUDE.md"),
            "private manual body\n",
        )
        .expect("target");
        write_manifest(&manifest_path, &clean_manifest());

        let report = scan_drift_for_test(&manifest_path).expect("owner-ratified scan");
        assert!(
            !kinds(&report).contains(&CHECK_DATED_CASE_BODY),
            "{:?}",
            report.findings
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn corroboration_must_share_the_dates_sentence_not_just_its_bullet() {
        // Real-corpus shape (AGENTS.md line 32): one long bullet holding an
        // uncorroborated `owner-ratified` clause several sentences upstream
        // of a genuinely corroborated `Evidence:` clause. Whole-line/whole-
        // bullet corroboration would wrongly flag the ratification date too;
        // sentence-scoped corroboration must not.
        let (root, manifest_path) = fixture("sentence-scope");
        let bullet = "- Some rule applies (owner-ratified 2026-07-14) for every case. Evidence: an incident on 2026-08-02 forced this (#1710).\n";
        std::fs::write(root.join("AGENTS.md"), bullet).expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private manual body\n").expect("source");
        std::fs::create_dir_all(root.join("targets/cursor")).expect("dir");
        std::fs::create_dir_all(root.join("targets/claude")).expect("dir");
        std::fs::write(root.join("targets/cursor/AGENTS.md"), bullet).expect("target");
        std::fs::write(
            root.join("targets/claude/CLAUDE.md"),
            "private manual body\n",
        )
        .expect("target");
        write_manifest(&manifest_path, &clean_manifest());

        let report = scan_drift_for_test(&manifest_path).expect("sentence scope scan");
        let dated: Vec<&str> = report
            .findings
            .iter()
            .filter(|finding| finding.check_kind == CHECK_DATED_CASE_BODY)
            .map(|finding| finding.evidence_span.as_str())
            .collect();
        assert!(
            dated.iter().any(|span| span.contains("2026-08-02")),
            "{dated:?}"
        );
        assert!(
            !dated.iter().any(|span| span.contains("2026-07-14")),
            "{dated:?}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn same_sentence_owner_ratified_with_corroboration_produces_no_dated_case() {
        // Regression test (#1710): a ratification date is never an incident.
        // Even if the exact same sentence contains both `owner-ratified <date>`
        // and an `Evidence:`/#NNNN marker, the ratification date must be
        // suppressed up front rather than falsely flagged as an incident.
        let (root, manifest_path) = fixture("same-sentence-ratified-corroborated");
        let sentence = "- Rule adopted (owner-ratified 2026-07-14) per Evidence: #1710.\n";
        std::fs::write(root.join("AGENTS.md"), sentence).expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private manual body\n").expect("source");
        std::fs::create_dir_all(root.join("targets/cursor")).expect("dir");
        std::fs::create_dir_all(root.join("targets/claude")).expect("dir");
        std::fs::write(root.join("targets/cursor/AGENTS.md"), sentence).expect("target");
        std::fs::write(
            root.join("targets/claude/CLAUDE.md"),
            "private manual body\n",
        )
        .expect("target");
        write_manifest(&manifest_path, &clean_manifest());

        let report = scan_drift_for_test(&manifest_path).expect("same sentence scan");
        assert!(
            !kinds(&report).contains(&CHECK_DATED_CASE_BODY),
            "{:?}",
            report.findings
        );

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

        let report = scan_drift_for_test(&manifest_path).expect("budget scan");
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

        let report = scan_drift_for_test(&manifest_path).expect("leak scan");
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
    fn unregistered_required_source_is_incomplete_coverage() {
        let (root, manifest_path) = fixture("unregistered");
        write_clean_tree(&root);
        std::fs::write(root.join("EXTRA.md"), "orphan surface\n").expect("extra");
        let mut manifest = clean_manifest();
        manifest["required_sources"] = json!(["AGENTS.md", "CLAUDE.md", "EXTRA.md"]);
        write_manifest(&manifest_path, &manifest);

        let report = scan_drift_for_test(&manifest_path).expect("unregistered scan");
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
    fn fix_shaped_request_refuses_and_leaves_bytes_identical() {
        let (root, manifest_path) = fixture("refuse-fix");
        write_clean_tree(&root);
        write_manifest(&manifest_path, &clean_manifest());
        let before = snapshot_tree(&root);

        let error = refuse_instruction_mutation("--fix").expect_err("must refuse");
        assert!(error.contains("report-only"), "{error}");
        let _ = scan_drift_for_test(&manifest_path).expect("scan still works");

        let after = snapshot_tree(&root);
        assert_eq!(before, after);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unchanged_repeated_scans_are_canonically_identical() {
        let (root, manifest_path) = fixture("stable");
        write_clean_tree(&root);
        write_manifest(&manifest_path, &clean_manifest());

        let first = scan_drift_for_test(&manifest_path).expect("first");
        let second = scan_drift_for_test(&manifest_path).expect("second");
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

        let report = scan_drift_for_test(&manifest_path).expect("empty targets");
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

    #[test]
    fn one_unreadable_source_becomes_a_manifest_finding_and_the_scan_continues() {
        // Acceptance (#1710): a manifest with one unreadable source must
        // still produce a report containing a finding for it plus findings
        // for the healthy surfaces -- not vanish along with an abort.
        let (root, manifest_path) = fixture("one-unreadable-source");
        std::fs::create_dir(root.join("AGENTS.md")).expect("AGENTS.md is a directory: unreadable");
        std::fs::write(root.join("CLAUDE.md"), "private manual body\n").expect("source");
        let mut manifest = clean_manifest();
        // claude-private-manual stays readable but gets its own real
        // (source-scoped) finding, to prove "findings for the healthy
        // surfaces" literally, not just "the scan didn't error".
        manifest["surfaces"][1]["targets"] = json!([]);
        write_manifest(&manifest_path, &manifest);

        let report = scan_drift_for_test(&manifest_path)
            .expect("scan continues past an unreadable source and still returns a report");

        assert_eq!(
            report.manifest_findings.len(),
            1,
            "{:?}",
            report.manifest_findings
        );
        assert_eq!(report.manifest_findings[0].surface_id, "public-agents");
        assert_eq!(report.manifest_findings[0].role, "source");
        assert_eq!(
            report.manifest_findings[0].check_kind,
            CHECK_UNREADABLE_MANIFEST_ENTRY
        );
        assert!(
            report.manifest_findings[0]
                .evidence_span
                .contains("directory"),
            "{:?}",
            report.manifest_findings[0]
        );

        assert!(
            report.findings.iter().any(|finding| {
                finding.source_id == "claude-private-manual"
                    && finding.check_kind == CHECK_INCOMPLETE_COVERAGE
            }),
            "{:?}",
            report.findings
        );
        assert_ne!(report.status, CLEAN_STATUS);

        let _ = std::fs::remove_dir_all(root);
    }
}

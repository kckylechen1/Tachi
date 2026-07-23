//! Read-only execution/reporting harness for the owner-approved #1059 pilot.
//!
//! This module deliberately sits outside the pure adapter: it supplies
//! already-fetched GitHub JSON through GithubCorpusReader, pins every
//! owner-selected PR merge SHA, and writes a reviewable receipt. It never
//! establishes a candidate, writes GitHub, or reads secret material.

use std::collections::HashSet;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tachi_params::{
    lesson_identity_status, sha256_hex, LessonCandidateKindV1, LessonCoverageV1,
    LessonEngineReceiptV1,
};

use super::adapt::adapt_corpus_case;
use super::pilot::{freeze_corpus_manifest, CorpusCaseV1, CorpusManifestV1, CORPUS_PILOT_SIZE};
use super::reader::{fetch_case_bundle, GithubCorpusReader};

pub const INPUT_SCHEMA_VERSION: &str = "github_corpus_exact20_input_v1";
pub const REPORT_SCHEMA_VERSION: &str = "github_corpus_exact20_report_v1";

/// Owner-approved row metadata that is intentionally outside CorpusCaseV1.
///
/// CorpusCaseV1 is the adapter's spend-gating schema. The expected merge SHA
/// and human handoff are execution receipt fields: keeping them here avoids
/// silently claiming that the core candidate type already modeled them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerCorpusCaseV1 {
    pub case_id: String,
    pub repo: String,
    pub issue_number: u64,
    pub pr_number: u64,
    pub expected_merge_sha: String,
    pub selection_reason: String,
    pub reference_decision: String,
    pub target_kind: LessonCandidateKindV1,
    pub cold_start_material_decision: String,
    pub behavior_test_handoff: String,
    #[serde(default)]
    pub follow_up_context: Vec<String>,
}

impl OwnerCorpusCaseV1 {
    fn as_corpus_case(&self) -> CorpusCaseV1 {
        CorpusCaseV1 {
            case_id: self.case_id.clone(),
            repo: self.repo.clone(),
            issue_number: self.issue_number,
            pr_number: Some(self.pr_number),
            selection_reason: self.selection_reason.clone(),
            reference_decision: self.reference_decision.clone(),
            target_kind: self.target_kind,
            cold_start_material_decision: self.cold_start_material_decision.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusPilotInputV1 {
    pub schema_version: String,
    pub project: String,
    pub cases: Vec<OwnerCorpusCaseV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderResolutionReceiptV1 {
    /// Whether the existing, read-only Vault status surface was queried.
    pub vault_status_checked: bool,
    /// The Vault status command is allowed to report cache readiness, but not
    /// individual provider names, models, aliases, or secret values.
    pub provider_cache_loaded: Option<bool>,
    /// A machine-readable reason that the run must remain preview-only.
    pub identity_proof: String,
}

impl ProviderResolutionReceiptV1 {
    pub fn unknown_without_engine_invocation() -> Self {
        Self {
            vault_status_checked: false,
            provider_cache_loaded: None,
            identity_proof: "github_corpus_adapter made no model invocation; effective provider/model/version are unproven".to_string(),
        }
    }
}

/// Explicitly unknown is safer than a requested model label: candidate identity
/// stays preview-only until a real engine receipt can prove all three identity
/// fields.
pub fn preview_only_engine_receipt() -> LessonEngineReceiptV1 {
    LessonEngineReceiptV1 {
        requested_role: "github_corpus_adapter".to_string(),
        effective_provider: None,
        effective_model: None,
        effective_version: None,
        fallback_chain: vec![
            "effective provider/model/version unavailable: no model invocation in the read-only adapter run".to_string(),
        ],
        degraded: true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectedCommentReceiptV1 {
    pub comment_id: String,
    pub updated_at: String,
    pub body_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateYieldV1 {
    pub emitted: bool,
    pub candidate_id: String,
    pub candidate_status: String,
    pub identity_status: String,
    pub evidence_ref_count: usize,
    pub outcome_evidence: bool,
    pub overturn_appended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaseLatencyCostV1 {
    pub latency_ms: u128,
    /// This adapter does not invoke a model or a paid generation endpoint.
    pub cost_usd: Option<String>,
    pub cost_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorpusPilotCaseReportV1 {
    pub case_id: String,
    pub issue_ref: String,
    pub pr_ref: String,
    pub expected_merge_sha: String,
    pub observed_merge_sha: String,
    pub issue_snapshot_hash: String,
    pub pr_snapshot_hash: String,
    /// Empty is meaningful: the adapter selected no structured comments, so
    /// this receipt must not invent a comment revision merely for completeness.
    pub selected_comment_revisions: Vec<SelectedCommentReceiptV1>,
    pub source_coverage: LessonCoverageV1,
    pub candidate_yield: CandidateYieldV1,
    pub cost_latency: CaseLatencyCostV1,
    pub behavior_test_handoff: String,
    pub follow_up_context: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorpusPilotReportV1 {
    pub schema_version: String,
    pub manifest_sha256: String,
    pub captured_at: String,
    /// complete is possible only with a fully known engine identity.
    pub disposition: String,
    pub engine_receipt: LessonEngineReceiptV1,
    pub provider_resolution: ProviderResolutionReceiptV1,
    pub candidates_emitted: usize,
    pub total_latency_ms: u128,
    pub cases: Vec<CorpusPilotCaseReportV1>,
}

fn nonempty(field: &str, value: &str, case_id: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("case {case_id} has empty {field}"))
    } else {
        Ok(())
    }
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn validate_input(input: &CorpusPilotInputV1) -> Result<CorpusManifestV1, String> {
    if input.schema_version != INPUT_SCHEMA_VERSION {
        return Err(format!(
            "unsupported corpus pilot input schema '{}' (expected '{INPUT_SCHEMA_VERSION}')",
            input.schema_version
        ));
    }
    nonempty("project", &input.project, "manifest")?;

    let mut pairs = HashSet::new();
    for case in &input.cases {
        nonempty("case_id", &case.case_id, "manifest")?;
        nonempty("repo", &case.repo, &case.case_id)?;
        nonempty(
            "expected_merge_sha",
            &case.expected_merge_sha,
            &case.case_id,
        )?;
        nonempty(
            "behavior_test_handoff",
            &case.behavior_test_handoff,
            &case.case_id,
        )?;
        if case.repo != "kckylechen1/tachi" {
            return Err(format!(
                "case {} is not owner-controlled kckylechen1/tachi: {}",
                case.case_id, case.repo
            ));
        }
        if !is_full_sha(&case.expected_merge_sha) {
            return Err(format!(
                "case {} expected_merge_sha is not a full 40-hex commit SHA",
                case.case_id
            ));
        }
        if !pairs.insert((case.repo.clone(), case.issue_number, case.pr_number)) {
            return Err(format!(
                "duplicate issue/PR pair in owner manifest: {}#{} / #{}",
                case.repo, case.issue_number, case.pr_number
            ));
        }
    }

    freeze_corpus_manifest(
        input
            .cases
            .iter()
            .map(OwnerCorpusCaseV1::as_corpus_case)
            .collect(),
    )
    .map_err(|errors| {
        format!(
            "owner corpus manifest failed the exact-{CORPUS_PILOT_SIZE} adapter gate: {errors:?}"
        )
    })
}

fn checked_comment_receipts(
    case_id: &str,
    comments: &[tachi_params::CommentRevisionV1],
) -> Result<Vec<SelectedCommentReceiptV1>, String> {
    comments
        .iter()
        .map(|comment| {
            if comment.comment_id.trim().is_empty()
                || comment.updated_at.trim().is_empty()
                || comment.body_hash.trim().is_empty()
            {
                return Err(format!(
                    "case {case_id} has adapter-selected comment without id, updated_at, or body_hash"
                ));
            }
            if sha256_hex(comment.body.as_bytes()) != comment.body_hash {
                return Err(format!(
                    "case {case_id} has adapter-selected comment {} whose body_hash does not match its body",
                    comment.comment_id
                ));
            }
            Ok(SelectedCommentReceiptV1 {
                comment_id: comment.comment_id.clone(),
                updated_at: comment.updated_at.clone(),
                body_hash: comment.body_hash.clone(),
            })
        })
        .collect()
}

fn check_expected_merge(case: &OwnerCorpusCaseV1, observed: Option<&str>) -> Result<(), String> {
    let Some(observed) = observed else {
        return Err(format!(
            "case {} PR #{} has no observed merge commit; refusing to adapt against an unpinned PR",
            case.case_id, case.pr_number
        ));
    };
    if observed != case.expected_merge_sha {
        return Err(format!(
            "case {} PR #{} merge SHA mismatch: expected {}, observed {}",
            case.case_id, case.pr_number, case.expected_merge_sha, observed
        ));
    }
    Ok(())
}

/// Execute the frozen adapter pilot through a caller-owned read-only reader.
///
/// A failed row aborts rather than yielding a deceptive partial candidate set:
/// the report's exact-20 cardinality is an invariant, not a best-effort goal.
pub fn run_corpus_pilot(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    engine_receipt: LessonEngineReceiptV1,
    provider_resolution: ProviderResolutionReceiptV1,
) -> Result<CorpusPilotReportV1, String> {
    let input: CorpusPilotInputV1 = serde_json::from_slice(input_bytes)
        .map_err(|error| format!("parse corpus pilot input JSON: {error}"))?;
    let manifest = validate_input(&input)?;
    let manifest_sha256 = sha256_hex(input_bytes);
    let started = Instant::now();
    let mut cases = Vec::with_capacity(input.cases.len());

    for owner_case in &input.cases {
        let case_started = Instant::now();
        let corpus_case = owner_case.as_corpus_case();
        let bundle = fetch_case_bundle(reader, &corpus_case, Vec::new(), captured_at)?;
        let pull_request = bundle.pull_request.as_ref().ok_or_else(|| {
            format!(
                "case {} expected PR #{} but reader returned no PR snapshot",
                owner_case.case_id, owner_case.pr_number
            )
        })?;
        check_expected_merge(owner_case, pull_request.merge_commit_sha.as_deref())?;
        let selected_comment_revisions = checked_comment_receipts(
            &owner_case.case_id,
            &bundle.issue.selected_comment_revisions,
        )?;
        let result = adapt_corpus_case(
            &input.project,
            &manifest,
            &bundle,
            None,
            Some(engine_receipt.clone()),
        )
        .map_err(|error| format!("adapt {}: {error}", owner_case.case_id))?;
        let identity_status = lesson_identity_status(result.candidate.engine_receipt.as_ref());

        cases.push(CorpusPilotCaseReportV1 {
            case_id: owner_case.case_id.clone(),
            issue_ref: result.issue.issue_ref.clone(),
            pr_ref: pull_request.pr_ref.clone(),
            expected_merge_sha: owner_case.expected_merge_sha.clone(),
            observed_merge_sha: pull_request
                .merge_commit_sha
                .clone()
                .expect("checked above"),
            issue_snapshot_hash: result.issue_snapshot_hash,
            pr_snapshot_hash: result
                .pr_snapshot_hash
                .expect("PR is required by owner manifest"),
            selected_comment_revisions,
            source_coverage: result.candidate.coverage,
            candidate_yield: CandidateYieldV1 {
                emitted: true,
                candidate_id: result.candidate.candidate_id,
                candidate_status: result.candidate.candidate_status.as_str().to_string(),
                identity_status: identity_status.to_string(),
                evidence_ref_count: result.evidence_refs.len(),
                outcome_evidence: result.outcome_evidence,
                overturn_appended: result.overturn_appended,
            },
            cost_latency: CaseLatencyCostV1 {
                latency_ms: case_started.elapsed().as_millis(),
                cost_usd: None,
                cost_status: "not_applicable_no_model_invocation".to_string(),
            },
            behavior_test_handoff: owner_case.behavior_test_handoff.clone(),
            follow_up_context: owner_case.follow_up_context.clone(),
        });
    }

    if cases.len() != CORPUS_PILOT_SIZE {
        return Err(format!(
            "internal pilot cardinality failure: emitted {} cases, expected {CORPUS_PILOT_SIZE}",
            cases.len()
        ));
    }
    let all_known = engine_receipt.has_known_identity();
    Ok(CorpusPilotReportV1 {
        schema_version: REPORT_SCHEMA_VERSION.to_string(),
        manifest_sha256,
        captured_at: captured_at.to_string(),
        disposition: if all_known {
            "complete".to_string()
        } else {
            "partial_preview_only".to_string()
        },
        engine_receipt,
        provider_resolution,
        candidates_emitted: cases.len(),
        total_latency_ms: started.elapsed().as_millis(),
        cases,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::github_corpus_ops::reader::FixtureCorpusReader;

    const MERGE_SHA: &str = "1111111111111111111111111111111111111111";

    fn input_bytes(expected_merge_sha: &str) -> Vec<u8> {
        let cases: Vec<_> = (0..CORPUS_PILOT_SIZE)
            .map(|number| {
                json!({
                    "case_id": format!("owner-case-{number}"),
                    "repo": "kckylechen1/tachi",
                    "issue_number": 10_000 + number as u64,
                    "pr_number": 20_000 + number as u64,
                    "expected_merge_sha": expected_merge_sha,
                    "selection_reason": "owner-approved real-source row",
                    "reference_decision": "preserve the named invariant",
                    "target_kind": if number == 0 { "precedent" } else { "bug_class" },
                    "cold_start_material_decision": "candidate must carry complete source coverage",
                    "behavior_test_handoff": "write a discriminating regression test for the named invariant",
                    "follow_up_context": []
                })
            })
            .collect();
        serde_json::to_vec(&json!({
            "schema_version": INPUT_SCHEMA_VERSION,
            "project": "sigil",
            "cases": cases,
        }))
        .expect("serialize fixture input")
    }

    fn fixture_reader(with_marker: bool) -> FixtureCorpusReader {
        let mut reader = FixtureCorpusReader::new();
        for number in 0..CORPUS_PILOT_SIZE {
            let issue_number = 10_000 + number as u64;
            let pr_number = 20_000 + number as u64;
            let comment_body = if with_marker && number == 0 {
                "Related: #1".to_string()
            } else {
                "ordinary discussion with no adapter marker".to_string()
            };
            reader.insert_issue(
                "kckylechen1/tachi",
                issue_number,
                json!({
                    "title": format!("Issue {issue_number}"),
                    "body": "Body source",
                    "state": "CLOSED",
                    "labels": [],
                    "updatedAt": "2026-07-24T00:00:00Z",
                    "comments": [{
                        "id": format!("comment-{issue_number}"),
                        "body": comment_body,
                        "createdAt": "2026-07-24T00:00:00Z",
                        "author": {"login": "owner"}
                    }]
                }),
            );
            reader.insert_pr(
                "kckylechen1/tachi",
                pr_number,
                json!({
                    "title": format!("PR {pr_number}"),
                    "body": "PR source",
                    "state": "MERGED",
                    "headRefOid": "2222222222222222222222222222222222222222",
                    "baseRefOid": "3333333333333333333333333333333333333333",
                    "updatedAt": "2026-07-24T00:00:00Z",
                    "mergeCommit": {"oid": MERGE_SHA},
                    "reviews": [],
                    "statusCheckRollup": []
                }),
            );
        }
        reader
    }

    #[test]
    fn real_runner_preserves_empty_comment_selection_and_preview_only_receipt() {
        let reader = fixture_reader(false);
        let report = run_corpus_pilot(
            &input_bytes(MERGE_SHA),
            &reader,
            "2026-07-24T00:00:00Z",
            preview_only_engine_receipt(),
            ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
        )
        .expect("exact 20 fixture rows run");
        assert_eq!(report.candidates_emitted, CORPUS_PILOT_SIZE);
        assert_eq!(report.disposition, "partial_preview_only");
        assert!(report
            .cases
            .iter()
            .all(|case| case.selected_comment_revisions.is_empty()));
        assert!(report
            .cases
            .iter()
            .all(|case| case.candidate_yield.identity_status == "preview_only"));
    }

    #[test]
    fn selected_comment_receipt_is_id_timestamp_and_body_hash_not_invented() {
        let reader = fixture_reader(true);
        let report = run_corpus_pilot(
            &input_bytes(MERGE_SHA),
            &reader,
            "2026-07-24T00:00:00Z",
            preview_only_engine_receipt(),
            ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
        )
        .expect("marker fixture rows run");
        let comment = report.cases[0]
            .selected_comment_revisions
            .first()
            .expect("the structured marker selects one real comment");
        assert_eq!(comment.comment_id, "comment-10000");
        assert_eq!(comment.updated_at, "2026-07-24T00:00:00Z");
        assert_eq!(comment.body_hash, sha256_hex(b"Related: #1"));
    }

    #[test]
    fn wrong_owner_pinned_merge_sha_refuses_before_candidate_emission() {
        let reader = fixture_reader(false);
        let err = run_corpus_pilot(
            &input_bytes("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            &reader,
            "2026-07-24T00:00:00Z",
            preview_only_engine_receipt(),
            ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
        )
        .expect_err("mismatched merge SHA must not emit an unpinned candidate");
        assert!(err.contains("merge SHA mismatch"), "{err}");
    }
}

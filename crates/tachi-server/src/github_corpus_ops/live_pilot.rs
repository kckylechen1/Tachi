//! Read-only execution/reporting harness for the owner-approved #1059 pilot.
//!
//! This module deliberately sits outside the pure adapter: it supplies
//! already-fetched GitHub JSON through GithubCorpusReader, pins every
//! owner-selected PR merge SHA, and writes a reviewable receipt. It never
//! establishes a candidate, writes GitHub, or reads secret material.

use std::collections::HashSet;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tachi_params::{
    lesson_identity_status, sha256_hex, LessonCandidateKindV1, LessonCandidateV1, LessonCoverageV1,
    LessonEngineReceiptV1,
};

use super::adapt::{adapt_corpus_case, CaseDraft, GithubCorpusCaseResult};
use super::parse::CaseCorpusBundle;
use super::pilot::{freeze_corpus_manifest, CorpusCaseV1, CorpusManifestV1, CORPUS_PILOT_SIZE};
use super::reader::{fetch_case_bundle, GithubCorpusReader};

pub const INPUT_SCHEMA_VERSION: &str = "github_corpus_exact20_input_v1";
pub const REPORT_SCHEMA_VERSION: &str = "github_corpus_exact20_report_v1";
/// The digest ratified with the exact 20-case owner manifest in PR #1416.
/// A real-model run must refuse any syntactically-valid substitute corpus.
pub const OWNER_APPROVED_EXACT20_MANIFEST_SHA256: &str =
    "224858da1443006fe9bdc84ce10aea84a2ccf2cec3da8aa2d8d041dc4df3fbe8";

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

/// Immutable fields copied from the preview receipt and checked against live
/// GitHub snapshots before a real-model client can spend. This intentionally
/// contains hashes and refs only: source prose is never made an execution
/// report field.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CorpusPilotProvenanceBaselineV1 {
    pub schema_version: String,
    pub manifest_sha256: String,
    pub cases: Vec<CorpusPilotProvenanceCaseV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CorpusPilotProvenanceCaseV1 {
    pub case_id: String,
    pub expected_merge_sha: String,
    pub observed_merge_sha: String,
    pub issue_snapshot_hash: String,
    pub pr_snapshot_hash: String,
}

/// The bounded source handed to a model client after the complete manifest and
/// every live immutable snapshot have passed preflight. It is never serialized
/// into a public report.
#[derive(Debug, Clone)]
pub struct CorpusPilotModelRequestV1 {
    pub case_id: String,
    pub selection_reason: String,
    pub reference_decision: String,
    pub cold_start_material_decision: String,
    pub bundle: CaseCorpusBundle,
}

/// Public-safe model receipt. The model text is deliberately split into a
/// typed draft below so report serialization cannot accidentally disclose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusPilotModelCompletionV1 {
    pub draft: CaseDraft,
    pub engine_receipt: LessonEngineReceiptV1,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<String>,
    pub cost_status: String,
    pub latency_ms: u128,
    pub truncated: bool,
}

/// Injectable production boundary. Tests supply a synthetic implementation;
/// the phase-3 CLI supplies the existing Tachi/tachi-llm provider path.
#[async_trait]
pub trait CorpusPilotModelClient: Send + Sync {
    async fn generate(
        &self,
        request: CorpusPilotModelRequestV1,
    ) -> Result<CorpusPilotModelCompletionV1, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusPilotExecutionV1 {
    pub report: CorpusPilotReportV1,
    /// Pending-only typed candidates for the caller's in-memory review path.
    /// They are not written to GitHub or established anywhere by this module.
    pub candidates: Vec<LessonCandidateV1>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// A provider that reports no price is represented explicitly as unknown;
    /// the pilot never fabricates a rate from a model label.
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
    pub engine_receipt: LessonEngineReceiptV1,
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
    pub model_invocations: usize,
    pub completed_cases: usize,
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

struct PreparedCorpusPilotV1 {
    input: CorpusPilotInputV1,
    manifest: CorpusManifestV1,
    manifest_sha256: String,
    cases: Vec<PreparedCorpusCaseV1>,
}

struct PreparedCorpusCaseV1 {
    owner_case: OwnerCorpusCaseV1,
    bundle: CaseCorpusBundle,
}

fn validate_provenance_baseline(
    input: &CorpusPilotInputV1,
    manifest_sha256: &str,
    baseline: &CorpusPilotProvenanceBaselineV1,
) -> Result<(), String> {
    if baseline.schema_version != REPORT_SCHEMA_VERSION {
        return Err("phase-3 baseline has an unsupported report schema".to_string());
    }
    if baseline.manifest_sha256 != manifest_sha256 {
        return Err("phase-3 baseline digest does not bind this exact manifest".to_string());
    }
    if baseline.cases.len() != CORPUS_PILOT_SIZE {
        return Err(format!(
            "phase-3 baseline must contain exactly {CORPUS_PILOT_SIZE} cases"
        ));
    }
    for owner_case in &input.cases {
        let Some(case) = baseline
            .cases
            .iter()
            .find(|case| case.case_id == owner_case.case_id)
        else {
            return Err(format!(
                "phase-3 baseline omits owner case {}",
                owner_case.case_id
            ));
        };
        if case.expected_merge_sha != owner_case.expected_merge_sha
            || case.observed_merge_sha != owner_case.expected_merge_sha
            || case.issue_snapshot_hash.trim().is_empty()
            || case.pr_snapshot_hash.trim().is_empty()
        {
            return Err(format!(
                "phase-3 baseline has invalid immutable provenance for case {}",
                owner_case.case_id
            ));
        }
    }
    Ok(())
}

/// Fetch and verify every selected source before a model client can be called.
/// This is deliberately a whole-corpus preflight: it prevents the first valid
/// rows from spending money when a later row has drifted.
fn prepare_corpus_pilot(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    required_manifest_sha256: Option<&str>,
    baseline: Option<&CorpusPilotProvenanceBaselineV1>,
) -> Result<PreparedCorpusPilotV1, String> {
    let input: CorpusPilotInputV1 = serde_json::from_slice(input_bytes)
        .map_err(|error| format!("parse corpus pilot input JSON: {error}"))?;
    let manifest = validate_input(&input)?;
    let manifest_sha256 = sha256_hex(input_bytes);
    if let Some(required) = required_manifest_sha256 {
        if manifest_sha256 != required {
            return Err(format!(
                "refusing model spend: manifest digest mismatch (expected owner-approved {required})"
            ));
        }
    }
    if let Some(baseline) = baseline {
        validate_provenance_baseline(&input, &manifest_sha256, baseline)?;
    }

    let mut cases = Vec::with_capacity(input.cases.len());
    for owner_case in &input.cases {
        let corpus_case = owner_case.as_corpus_case();
        let bundle = fetch_case_bundle(reader, &corpus_case, Vec::new(), captured_at)?;
        let pull_request = bundle.pull_request.as_ref().ok_or_else(|| {
            format!(
                "case {} expected PR #{} but reader returned no PR snapshot",
                owner_case.case_id, owner_case.pr_number
            )
        })?;
        check_expected_merge(owner_case, pull_request.merge_commit_sha.as_deref())?;
        checked_comment_receipts(
            &owner_case.case_id,
            &bundle.issue.selected_comment_revisions,
        )?;

        if let Some(baseline) = baseline {
            let expected = baseline
                .cases
                .iter()
                .find(|case| case.case_id == owner_case.case_id)
                .expect("validated above");
            if expected.issue_snapshot_hash != bundle.issue.issue_snapshot_hash
                || expected.pr_snapshot_hash != pull_request.pr_snapshot_hash
            {
                return Err(format!(
                    "refusing model spend: immutable provenance drift for case {}",
                    owner_case.case_id
                ));
            }
        }
        cases.push(PreparedCorpusCaseV1 {
            owner_case: owner_case.clone(),
            bundle,
        });
    }
    Ok(PreparedCorpusPilotV1 {
        input,
        manifest,
        manifest_sha256,
        cases,
    })
}

fn report_case(
    owner_case: &OwnerCorpusCaseV1,
    result: GithubCorpusCaseResult,
    engine_receipt: LessonEngineReceiptV1,
    cost_latency: CaseLatencyCostV1,
) -> CorpusPilotCaseReportV1 {
    let pull_request = result
        .pull_request
        .as_ref()
        .expect("preflight requires a PR for every owner case");
    let selected_comment_revisions = checked_comment_receipts(
        &owner_case.case_id,
        &result.issue.selected_comment_revisions,
    )
    .expect("preflight verified selected comment receipts");
    let identity_status = lesson_identity_status(result.candidate.engine_receipt.as_ref());

    CorpusPilotCaseReportV1 {
        case_id: owner_case.case_id.clone(),
        issue_ref: result.issue.issue_ref.clone(),
        pr_ref: pull_request.pr_ref.clone(),
        expected_merge_sha: owner_case.expected_merge_sha.clone(),
        observed_merge_sha: pull_request
            .merge_commit_sha
            .clone()
            .expect("preflight verified merge SHA"),
        issue_snapshot_hash: result.issue_snapshot_hash,
        pr_snapshot_hash: result
            .pr_snapshot_hash
            .expect("preflight requires a PR for every owner case"),
        selected_comment_revisions,
        source_coverage: result.candidate.coverage,
        engine_receipt,
        candidate_yield: CandidateYieldV1 {
            emitted: true,
            candidate_id: result.candidate.candidate_id,
            candidate_status: result.candidate.candidate_status.as_str().to_string(),
            identity_status: identity_status.to_string(),
            evidence_ref_count: result.evidence_refs.len(),
            outcome_evidence: result.outcome_evidence,
            overturn_appended: result.overturn_appended,
        },
        cost_latency,
        behavior_test_handoff: owner_case.behavior_test_handoff.clone(),
        follow_up_context: owner_case.follow_up_context.clone(),
    }
}

/// Preview-only compatibility path. It intentionally has no model client and
/// can therefore never complete the model pilot, even if a caller supplies a
/// configured-looking engine identity.
pub fn run_corpus_pilot(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    engine_receipt: LessonEngineReceiptV1,
    provider_resolution: ProviderResolutionReceiptV1,
) -> Result<CorpusPilotReportV1, String> {
    let prepared = prepare_corpus_pilot(input_bytes, reader, captured_at, None, None)?;
    run_preview_prepared(prepared, captured_at, engine_receipt, provider_resolution)
}

fn run_preview_prepared(
    prepared: PreparedCorpusPilotV1,
    captured_at: &str,
    engine_receipt: LessonEngineReceiptV1,
    provider_resolution: ProviderResolutionReceiptV1,
) -> Result<CorpusPilotReportV1, String> {
    let started = Instant::now();
    let mut cases = Vec::with_capacity(prepared.cases.len());
    for prepared_case in prepared.cases {
        let case_started = Instant::now();
        let result = adapt_corpus_case(
            &prepared.input.project,
            &prepared.manifest,
            &prepared_case.bundle,
            None,
            Some(engine_receipt.clone()),
        )
        .map_err(|error| format!("adapt {}: {error}", prepared_case.owner_case.case_id))?;
        cases.push(report_case(
            &prepared_case.owner_case,
            result,
            engine_receipt.clone(),
            CaseLatencyCostV1 {
                latency_ms: case_started.elapsed().as_millis(),
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cost_usd: None,
                cost_status: "not_applicable_no_model_invocation".to_string(),
            },
        ));
    }
    Ok(CorpusPilotReportV1 {
        schema_version: REPORT_SCHEMA_VERSION.to_string(),
        manifest_sha256: prepared.manifest_sha256,
        captured_at: captured_at.to_string(),
        disposition: "partial_preview_only".to_string(),
        engine_receipt,
        provider_resolution,
        model_invocations: 0,
        completed_cases: 0,
        candidates_emitted: cases.len(),
        total_latency_ms: started.elapsed().as_millis(),
        cases,
    })
}

/// No-spend phase-3 rehearsal: it validates the owner-pinned manifest and all
/// live immutable provenance before emitting only preview candidates. It never
/// constructs, resolves, or calls a model client.
pub fn dry_run_owner_approved_corpus_pilot(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    baseline: &CorpusPilotProvenanceBaselineV1,
) -> Result<CorpusPilotReportV1, String> {
    let prepared = prepare_corpus_pilot(
        input_bytes,
        reader,
        captured_at,
        Some(OWNER_APPROVED_EXACT20_MANIFEST_SHA256),
        Some(baseline),
    )?;
    run_preview_prepared(
        prepared,
        captured_at,
        preview_only_engine_receipt(),
        ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
    )
}

/// Real-model execution path. All manifest and immutable-provenance checks run
/// to completion before its first `model.generate` call. Candidates remain
/// pending by construction; unknown/degraded engine receipts remain preview
/// evidence and never increment `completed_cases`.
pub async fn run_owner_approved_corpus_pilot(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    baseline: &CorpusPilotProvenanceBaselineV1,
    provider_resolution: ProviderResolutionReceiptV1,
    model: &dyn CorpusPilotModelClient,
) -> Result<CorpusPilotExecutionV1, String> {
    run_corpus_pilot_with_model(
        input_bytes,
        reader,
        captured_at,
        baseline,
        OWNER_APPROVED_EXACT20_MANIFEST_SHA256,
        provider_resolution,
        model,
    )
    .await
}

async fn run_corpus_pilot_with_model(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    baseline: &CorpusPilotProvenanceBaselineV1,
    required_manifest_sha256: &str,
    provider_resolution: ProviderResolutionReceiptV1,
    model: &dyn CorpusPilotModelClient,
) -> Result<CorpusPilotExecutionV1, String> {
    let prepared = prepare_corpus_pilot(
        input_bytes,
        reader,
        captured_at,
        Some(required_manifest_sha256),
        Some(baseline),
    )?;
    let started = Instant::now();
    let mut cases = Vec::with_capacity(prepared.cases.len());
    let mut candidates = Vec::with_capacity(prepared.cases.len());
    let mut completed_cases = 0usize;

    for prepared_case in prepared.cases {
        let request = CorpusPilotModelRequestV1 {
            case_id: prepared_case.owner_case.case_id.clone(),
            selection_reason: prepared_case.owner_case.selection_reason.clone(),
            reference_decision: prepared_case.owner_case.reference_decision.clone(),
            cold_start_material_decision: prepared_case
                .owner_case
                .cold_start_material_decision
                .clone(),
            bundle: prepared_case.bundle.clone(),
        };
        let completion = model.generate(request).await.map_err(|_| {
            format!(
                "phase-3 model invocation failed for case {}; model output is withheld",
                prepared_case.owner_case.case_id
            )
        })?;
        let mut engine_receipt = completion.engine_receipt;
        if completion.truncated {
            engine_receipt.degraded = true;
            engine_receipt
                .fallback_chain
                .push("provider response was truncated".to_string());
        }
        let countable = engine_receipt.has_known_identity();
        let result = adapt_corpus_case(
            &prepared.input.project,
            &prepared.manifest,
            &prepared_case.bundle,
            Some(completion.draft),
            Some(engine_receipt.clone()),
        )
        .map_err(|error| format!("adapt {}: {error}", prepared_case.owner_case.case_id))?;
        if countable {
            completed_cases += 1;
        }
        candidates.push(result.candidate.clone());
        cases.push(report_case(
            &prepared_case.owner_case,
            result,
            engine_receipt,
            CaseLatencyCostV1 {
                latency_ms: completion.latency_ms,
                prompt_tokens: completion.prompt_tokens,
                completion_tokens: completion.completion_tokens,
                total_tokens: completion.total_tokens,
                cost_usd: completion.cost_usd,
                cost_status: completion.cost_status,
            },
        ));
    }

    let complete = completed_cases == CORPUS_PILOT_SIZE;
    let engine_receipt = if complete {
        cases[0].engine_receipt.clone()
    } else {
        preview_only_engine_receipt()
    };
    Ok(CorpusPilotExecutionV1 {
        report: CorpusPilotReportV1 {
            schema_version: REPORT_SCHEMA_VERSION.to_string(),
            manifest_sha256: prepared.manifest_sha256,
            captured_at: captured_at.to_string(),
            disposition: if complete {
                "complete".to_string()
            } else {
                "partial_preview_only".to_string()
            },
            engine_receipt,
            provider_resolution,
            model_invocations: cases.len(),
            completed_cases,
            candidates_emitted: cases.len(),
            total_latency_ms: started.elapsed().as_millis(),
            cases,
        },
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::github_corpus_ops::reader::{FixtureCorpusReader, MutationProbe};

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

    fn baseline_for_fixture(
        input_bytes: &[u8],
        reader: &FixtureCorpusReader,
    ) -> CorpusPilotProvenanceBaselineV1 {
        let input: CorpusPilotInputV1 =
            serde_json::from_slice(input_bytes).expect("fixture input parses");
        let cases = input
            .cases
            .iter()
            .map(|owner_case| {
                let bundle = fetch_case_bundle(
                    reader,
                    &owner_case.as_corpus_case(),
                    Vec::new(),
                    "2026-07-24T00:00:00Z",
                )
                .expect("fixture source reads");
                let pull_request = bundle.pull_request.expect("fixture has PR");
                CorpusPilotProvenanceCaseV1 {
                    case_id: owner_case.case_id.clone(),
                    expected_merge_sha: owner_case.expected_merge_sha.clone(),
                    observed_merge_sha: pull_request.merge_commit_sha.expect("fixture merge SHA"),
                    issue_snapshot_hash: bundle.issue.issue_snapshot_hash,
                    pr_snapshot_hash: pull_request.pr_snapshot_hash,
                }
            })
            .collect();
        CorpusPilotProvenanceBaselineV1 {
            schema_version: REPORT_SCHEMA_VERSION.to_string(),
            manifest_sha256: sha256_hex(input_bytes),
            cases,
        }
    }

    #[derive(Default)]
    struct SyntheticModel {
        calls: std::sync::atomic::AtomicUsize,
        known_identity: bool,
        marker: String,
    }

    #[async_trait::async_trait]
    impl CorpusPilotModelClient for SyntheticModel {
        async fn generate(
            &self,
            request: CorpusPilotModelRequestV1,
        ) -> Result<CorpusPilotModelCompletionV1, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let known = self.known_identity;
            Ok(CorpusPilotModelCompletionV1 {
                draft: CaseDraft {
                    situation: format!(
                        "{}\n{}",
                        request.bundle.issue.title, request.bundle.issue.body
                    ),
                    proposed_ruling: request.reference_decision,
                    why: if self.marker.is_empty() {
                        request.selection_reason
                    } else {
                        self.marker.clone()
                    },
                    how_to_apply: request.cold_start_material_decision,
                },
                engine_receipt: LessonEngineReceiptV1 {
                    requested_role: "github_corpus_exact20".to_string(),
                    effective_provider: known.then(|| "fixture-provider".to_string()),
                    effective_model: known.then(|| "fixture-model".to_string()),
                    effective_version: known.then(|| "fixture-version".to_string()),
                    fallback_chain: Vec::new(),
                    degraded: false,
                },
                prompt_tokens: Some(11),
                completion_tokens: Some(7),
                total_tokens: Some(18),
                cost_usd: None,
                cost_status: "provider_price_not_reported".to_string(),
                latency_ms: 9,
                truncated: false,
            })
        }
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

    /// RED before phase 2: a configured-looking receipt is not evidence that
    /// a model actually generated this pilot. The pre-phase-2 runner accepted
    /// the synthetic identity and mislabeled the adapter-only result complete.
    #[test]
    fn known_but_uninvoked_engine_cannot_complete_the_pilot() {
        let reader = fixture_reader(false);
        let report = run_corpus_pilot(
            &input_bytes(MERGE_SHA),
            &reader,
            "2026-07-24T00:00:00Z",
            LessonEngineReceiptV1 {
                requested_role: "github_corpus_adapter".to_string(),
                effective_provider: Some("fixture-provider".to_string()),
                effective_model: Some("fixture-model".to_string()),
                effective_version: Some("fixture-version".to_string()),
                fallback_chain: Vec::new(),
                degraded: false,
            },
            ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
        )
        .expect("the deterministic adapter fixture runs");

        assert_eq!(
            report.disposition, "partial_preview_only",
            "an engine identity without an invocation receipt is not a completed model pilot"
        );
    }

    #[tokio::test]
    async fn manifest_or_immutable_provenance_drift_blocks_spend_before_model_calls() {
        let reader = fixture_reader(false);
        let input = input_bytes(MERGE_SHA);
        let baseline = baseline_for_fixture(&input, &reader);
        let model = SyntheticModel {
            known_identity: true,
            ..Default::default()
        };

        let err = run_corpus_pilot_with_model(
            &input,
            &reader,
            "2026-07-24T00:00:00Z",
            &baseline,
            "not-the-fixture-digest",
            ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
            &model,
        )
        .await
        .expect_err("wrong manifest digest must stop before model spend");
        assert!(err.contains("manifest digest mismatch"), "{err}");
        assert_eq!(
            model.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "manifest failure must occur before the model client is reachable"
        );

        let mut drifted = baseline.clone();
        drifted.cases[0].issue_snapshot_hash = "drifted".to_string();
        let err = run_corpus_pilot_with_model(
            &input,
            &reader,
            "2026-07-24T00:00:00Z",
            &drifted,
            &sha256_hex(&input),
            ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
            &model,
        )
        .await
        .expect_err("immutable snapshot drift must stop before model spend");
        assert!(err.contains("immutable provenance drift"), "{err}");
        assert_eq!(
            model.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "provenance drift must occur before the model client is reachable"
        );
    }

    #[tokio::test]
    async fn synthetic_model_receipt_is_pending_only_counted_only_when_known_and_redacted() {
        let input = input_bytes(MERGE_SHA);
        let fixture = fixture_reader(false);
        let baseline = baseline_for_fixture(&input, &fixture);
        let reader = MutationProbe::new(fixture);
        let model = SyntheticModel {
            known_identity: true,
            marker: "RAW_MODEL_OUTPUT_MUST_NOT_APPEAR_IN_REPORT".to_string(),
            ..Default::default()
        };

        let execution = run_corpus_pilot_with_model(
            &input,
            &reader,
            "2026-07-24T00:00:00Z",
            &baseline,
            &sha256_hex(&input),
            ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
            &model,
        )
        .await
        .expect("fully attested synthetic model run");

        reader.assert_no_mutations();
        assert_eq!(execution.report.model_invocations, CORPUS_PILOT_SIZE);
        assert_eq!(execution.report.completed_cases, CORPUS_PILOT_SIZE);
        assert_eq!(execution.report.disposition, "complete");
        assert!(execution.candidates.iter().all(|candidate| {
            candidate.candidate_status.as_str() == "pending" && !candidate.refs.is_empty()
        }));
        assert!(execution.report.cases.iter().all(|case| {
            case.engine_receipt.has_known_identity()
                && case.cost_latency.total_tokens == Some(18)
                && case.candidate_yield.identity_status == "known"
        }));
        let public_report = serde_json::to_string(&execution.report).expect("report serializes");
        assert!(
            !public_report.contains("RAW_MODEL_OUTPUT_MUST_NOT_APPEAR_IN_REPORT"),
            "public report must not serialize raw model output"
        );
    }

    #[tokio::test]
    async fn unknown_engine_receipt_stays_preview_only_and_cannot_complete() {
        let input = input_bytes(MERGE_SHA);
        let reader = fixture_reader(false);
        let baseline = baseline_for_fixture(&input, &reader);
        let model = SyntheticModel::default();

        let execution = run_corpus_pilot_with_model(
            &input,
            &reader,
            "2026-07-24T00:00:00Z",
            &baseline,
            &sha256_hex(&input),
            ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
            &model,
        )
        .await
        .expect("synthetic unknown-identity run");

        assert_eq!(execution.report.model_invocations, CORPUS_PILOT_SIZE);
        assert_eq!(execution.report.completed_cases, 0);
        assert_eq!(execution.report.disposition, "partial_preview_only");
        assert!(execution
            .report
            .cases
            .iter()
            .all(|case| case.candidate_yield.identity_status == "preview_only"));
    }
}

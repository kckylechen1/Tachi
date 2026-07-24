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
/// SHA-256 of the exact preview report bytes ratified for phase-3 preflight.
/// Rebaselining is an explicit owner action that updates this pin.
pub const OWNER_APPROVED_EXACT20_BASELINE_SHA256: &str =
    "3bbb067f90c57009eeb45e9129becf7bf28062a718735686f088761e68e2765e";
pub const CHECKPOINT_SCHEMA_VERSION: &str = "github_corpus_exact20_checkpoint_v1";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusPilotProvenanceBaselineV1 {
    pub schema_version: String,
    pub manifest_sha256: String,
    pub cases: Vec<CorpusPilotProvenanceCaseV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub cost_basis: Option<String>,
    pub cost_version: Option<String>,
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

pub struct ResolvedCorpusPilotModelV1 {
    pub provider_resolution: ProviderResolutionReceiptV1,
    pub model: Box<dyn CorpusPilotModelClient>,
}

/// Resolver construction is deliberately deferred until the complete frozen
/// manifest, pinned baseline bytes, and all live provenance pass preflight.
pub trait CorpusPilotModelResolver: Send + Sync {
    fn resolve(&self) -> Result<ResolvedCorpusPilotModelV1, String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusPilotCheckpointV1 {
    pub schema_version: String,
    pub manifest_sha256: String,
    pub baseline_sha256: String,
    pub provider_resolution: Option<ProviderResolutionReceiptV1>,
    pub cases: Vec<CorpusPilotCheckpointCaseV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusPilotCheckpointCaseV1 {
    pub checkpoint_key: String,
    pub case_id: String,
    pub preflight: CorpusPilotCheckpointPreflightV1,
    pub attempts: Vec<CorpusPilotCheckpointAttemptV1>,
    pub fully_attested_completion_attempt: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusPilotCheckpointPreflightV1 {
    pub expected_merge_sha: String,
    pub observed_merge_sha: String,
    pub issue_snapshot_hash: String,
    pub pr_snapshot_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusPilotCheckpointAttemptV1 {
    pub attempt: usize,
    pub started_at: String,
    pub outcome: Option<CorpusPilotCheckpointOutcomeV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CorpusPilotCheckpointOutcomeV1 {
    Failure {
        failure_class: String,
        latency_ms: u128,
    },
    Candidate {
        fully_attested: bool,
        report: Box<CorpusPilotCaseReportV1>,
        candidate: Box<LessonCandidateV1>,
    },
}

/// Atomicity belongs to the concrete store. The executor never calls a model
/// until the attempt record has been durably saved.
pub trait CorpusPilotCheckpointStore: Send + Sync {
    fn load(&self) -> Result<Option<CorpusPilotCheckpointV1>, String>;
    fn save_atomic(&self, checkpoint: &CorpusPilotCheckpointV1) -> Result<(), String>;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedCommentReceiptV1 {
    pub comment_id: String,
    pub updated_at: String,
    pub body_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateYieldV1 {
    pub emitted: bool,
    pub candidate_id: String,
    pub candidate_status: String,
    pub identity_status: String,
    pub evidence_ref_count: usize,
    pub outcome_evidence: bool,
    pub overturn_appended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_basis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_coverage: Option<LessonCoverageV1>,
    pub engine_receipt: LessonEngineReceiptV1,
    pub candidate_yield: CandidateYieldV1,
    pub cost_latency: CaseLatencyCostV1,
    pub behavior_test_handoff: String,
    pub follow_up_context: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusPilotReportV1 {
    pub schema_version: String,
    pub manifest_sha256: String,
    pub baseline_sha256: Option<String>,
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

/// Opaque proof that every no-spend gate completed. The only constructor runs
/// pinned baseline-byte verification before any live GitHub read.
pub struct CorpusPilotPreflightV1 {
    prepared: PreparedCorpusPilotV1,
    baseline_sha256: String,
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

fn preflight_corpus_pilot(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    required_manifest_sha256: &str,
    baseline_bytes: &[u8],
    required_baseline_sha256: &str,
) -> Result<CorpusPilotPreflightV1, String> {
    let baseline_sha256 = sha256_hex(baseline_bytes);
    if baseline_sha256 != required_baseline_sha256 {
        return Err(format!(
            "refusing model spend: baseline artifact digest mismatch (expected owner-approved {required_baseline_sha256})"
        ));
    }
    let baseline: CorpusPilotProvenanceBaselineV1 = serde_json::from_slice(baseline_bytes)
        .map_err(|_| "parse pinned baseline report provenance fields".to_string())?;
    let prepared = prepare_corpus_pilot(
        input_bytes,
        reader,
        captured_at,
        Some(required_manifest_sha256),
        Some(&baseline),
    )?;
    Ok(CorpusPilotPreflightV1 {
        prepared,
        baseline_sha256,
    })
}

pub fn preflight_owner_approved_corpus_pilot(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    baseline_bytes: &[u8],
    requested_baseline_sha256: &str,
) -> Result<CorpusPilotPreflightV1, String> {
    if requested_baseline_sha256 != OWNER_APPROVED_EXACT20_BASELINE_SHA256 {
        return Err(format!(
            "refusing model spend: CLI baseline digest is not the owner-approved pin {OWNER_APPROVED_EXACT20_BASELINE_SHA256}"
        ));
    }
    preflight_corpus_pilot(
        input_bytes,
        reader,
        captured_at,
        OWNER_APPROVED_EXACT20_MANIFEST_SHA256,
        baseline_bytes,
        OWNER_APPROVED_EXACT20_BASELINE_SHA256,
    )
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
        source_coverage: Some(result.candidate.coverage),
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
        failure_class: None,
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
                cost_basis: None,
                cost_version: None,
            },
        ));
    }
    Ok(CorpusPilotReportV1 {
        schema_version: REPORT_SCHEMA_VERSION.to_string(),
        manifest_sha256: prepared.manifest_sha256,
        baseline_sha256: None,
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
    baseline_bytes: &[u8],
    requested_baseline_sha256: &str,
) -> Result<CorpusPilotReportV1, String> {
    let preflight = preflight_owner_approved_corpus_pilot(
        input_bytes,
        reader,
        captured_at,
        baseline_bytes,
        requested_baseline_sha256,
    )?;
    let baseline_sha256 = preflight.baseline_sha256;
    let mut report = run_preview_prepared(
        preflight.prepared,
        captured_at,
        preview_only_engine_receipt(),
        ProviderResolutionReceiptV1::unknown_without_engine_invocation(),
    )?;
    report.baseline_sha256 = Some(baseline_sha256);
    Ok(report)
}

/// Real-model execution path. All manifest and immutable-provenance checks run
/// to completion before its first `model.generate` call. Candidates remain
/// pending by construction; unknown/degraded engine receipts remain preview
/// evidence and never increment `completed_cases`.
pub async fn run_owner_approved_corpus_pilot(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    baseline_bytes: &[u8],
    requested_baseline_sha256: &str,
    checkpoint_store: &dyn CorpusPilotCheckpointStore,
    resolver: &dyn CorpusPilotModelResolver,
) -> Result<CorpusPilotExecutionV1, String> {
    let preflight = preflight_owner_approved_corpus_pilot(
        input_bytes,
        reader,
        captured_at,
        baseline_bytes,
        requested_baseline_sha256,
    )?;
    execute_preflighted_corpus_pilot(preflight, captured_at, checkpoint_store, resolver).await
}

#[cfg(test)]
async fn run_corpus_pilot_with_resolver(
    input_bytes: &[u8],
    reader: &dyn GithubCorpusReader,
    captured_at: &str,
    required_manifest_sha256: &str,
    baseline_bytes: &[u8],
    required_baseline_sha256: &str,
    checkpoint_store: &dyn CorpusPilotCheckpointStore,
    resolver: &dyn CorpusPilotModelResolver,
) -> Result<CorpusPilotExecutionV1, String> {
    let preflight = preflight_corpus_pilot(
        input_bytes,
        reader,
        captured_at,
        required_manifest_sha256,
        baseline_bytes,
        required_baseline_sha256,
    )?;
    execute_preflighted_corpus_pilot(preflight, captured_at, checkpoint_store, resolver).await
}

fn checkpoint_key(manifest_sha256: &str, baseline_sha256: &str, case_id: &str) -> String {
    sha256_hex(format!("{manifest_sha256}:{baseline_sha256}:{case_id}").as_bytes())
}

fn checkpoint_preflight(prepared_case: &PreparedCorpusCaseV1) -> CorpusPilotCheckpointPreflightV1 {
    let pull_request = prepared_case
        .bundle
        .pull_request
        .as_ref()
        .expect("whole-corpus preflight requires every PR");
    CorpusPilotCheckpointPreflightV1 {
        expected_merge_sha: prepared_case.owner_case.expected_merge_sha.clone(),
        observed_merge_sha: pull_request
            .merge_commit_sha
            .clone()
            .expect("whole-corpus preflight requires every merge SHA"),
        issue_snapshot_hash: prepared_case.bundle.issue.issue_snapshot_hash.clone(),
        pr_snapshot_hash: pull_request.pr_snapshot_hash.clone(),
    }
}

fn new_checkpoint(preflight: &CorpusPilotPreflightV1) -> CorpusPilotCheckpointV1 {
    CorpusPilotCheckpointV1 {
        schema_version: CHECKPOINT_SCHEMA_VERSION.to_string(),
        manifest_sha256: preflight.prepared.manifest_sha256.clone(),
        baseline_sha256: preflight.baseline_sha256.clone(),
        provider_resolution: None,
        cases: preflight
            .prepared
            .cases
            .iter()
            .map(|prepared_case| CorpusPilotCheckpointCaseV1 {
                checkpoint_key: checkpoint_key(
                    &preflight.prepared.manifest_sha256,
                    &preflight.baseline_sha256,
                    &prepared_case.owner_case.case_id,
                ),
                case_id: prepared_case.owner_case.case_id.clone(),
                preflight: checkpoint_preflight(prepared_case),
                attempts: Vec::new(),
                fully_attested_completion_attempt: None,
            })
            .collect(),
    }
}

fn validate_checkpoint(
    checkpoint: &CorpusPilotCheckpointV1,
    preflight: &CorpusPilotPreflightV1,
) -> Result<(), String> {
    if checkpoint.schema_version != CHECKPOINT_SCHEMA_VERSION
        || checkpoint.manifest_sha256 != preflight.prepared.manifest_sha256
        || checkpoint.baseline_sha256 != preflight.baseline_sha256
        || checkpoint.cases.len() != preflight.prepared.cases.len()
    {
        return Err(
            "checkpoint does not match the immutable manifest and baseline binding".to_string(),
        );
    }
    for prepared_case in &preflight.prepared.cases {
        let Some(saved) = checkpoint
            .cases
            .iter()
            .find(|saved| saved.case_id == prepared_case.owner_case.case_id)
        else {
            return Err("checkpoint omits an owner-approved case".to_string());
        };
        let expected_key = checkpoint_key(
            &checkpoint.manifest_sha256,
            &checkpoint.baseline_sha256,
            &saved.case_id,
        );
        let completion_valid = saved
            .fully_attested_completion_attempt
            .is_none_or(|completed| {
                saved.attempts.iter().any(|attempt| {
                    attempt.attempt == completed
                        && matches!(
                            attempt.outcome.as_ref(),
                            Some(CorpusPilotCheckpointOutcomeV1::Candidate {
                                fully_attested: true,
                                ..
                            })
                        )
                })
            });
        if saved.checkpoint_key != expected_key
            || saved.preflight != checkpoint_preflight(prepared_case)
            || !completion_valid
        {
            return Err("checkpoint case binding or completion receipt is invalid".to_string());
        }
    }
    Ok(())
}

fn actual_cost_is_attested(cost: &CaseLatencyCostV1) -> bool {
    cost.cost_status == "provider_reported_actual"
        && cost
            .cost_usd
            .as_deref()
            .and_then(|value| value.parse::<f64>().ok())
            .is_some_and(|value| value.is_finite() && value >= 0.0)
        && cost
            .cost_basis
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && cost
            .cost_version
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn failure_report_case(
    prepared_case: &PreparedCorpusCaseV1,
    failure_class: &str,
    latency_ms: u128,
) -> CorpusPilotCaseReportV1 {
    let pull_request = prepared_case
        .bundle
        .pull_request
        .as_ref()
        .expect("whole-corpus preflight requires every PR");
    CorpusPilotCaseReportV1 {
        case_id: prepared_case.owner_case.case_id.clone(),
        issue_ref: prepared_case.bundle.issue.issue_ref.clone(),
        pr_ref: pull_request.pr_ref.clone(),
        expected_merge_sha: prepared_case.owner_case.expected_merge_sha.clone(),
        observed_merge_sha: pull_request
            .merge_commit_sha
            .clone()
            .expect("whole-corpus preflight requires every merge SHA"),
        issue_snapshot_hash: prepared_case.bundle.issue.issue_snapshot_hash.clone(),
        pr_snapshot_hash: pull_request.pr_snapshot_hash.clone(),
        selected_comment_revisions: checked_comment_receipts(
            &prepared_case.owner_case.case_id,
            &prepared_case.bundle.issue.selected_comment_revisions,
        )
        .expect("whole-corpus preflight verified selected comments"),
        source_coverage: None,
        engine_receipt: preview_only_engine_receipt(),
        candidate_yield: CandidateYieldV1 {
            emitted: false,
            candidate_id: String::new(),
            candidate_status: "not_emitted".to_string(),
            identity_status: "preview_only".to_string(),
            evidence_ref_count: 0,
            outcome_evidence: false,
            overturn_appended: false,
        },
        cost_latency: CaseLatencyCostV1 {
            latency_ms,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cost_usd: None,
            cost_status: "unknown_model_invocation_failed".to_string(),
            cost_basis: None,
            cost_version: None,
        },
        behavior_test_handoff: prepared_case.owner_case.behavior_test_handoff.clone(),
        follow_up_context: prepared_case.owner_case.follow_up_context.clone(),
        failure_class: Some(failure_class.to_string()),
    }
}

fn completed_attempt(
    checkpoint_case: &CorpusPilotCheckpointCaseV1,
) -> Option<&CorpusPilotCheckpointAttemptV1> {
    let completed = checkpoint_case.fully_attested_completion_attempt?;
    checkpoint_case
        .attempts
        .iter()
        .find(|attempt| attempt.attempt == completed)
}

fn latest_candidate_attempt(
    checkpoint_case: &CorpusPilotCheckpointCaseV1,
) -> Option<&CorpusPilotCheckpointAttemptV1> {
    checkpoint_case.attempts.iter().rev().find(|attempt| {
        matches!(
            attempt.outcome.as_ref(),
            Some(CorpusPilotCheckpointOutcomeV1::Candidate { .. })
        )
    })
}

async fn execute_preflighted_corpus_pilot(
    preflight: CorpusPilotPreflightV1,
    captured_at: &str,
    checkpoint_store: &dyn CorpusPilotCheckpointStore,
    resolver: &dyn CorpusPilotModelResolver,
) -> Result<CorpusPilotExecutionV1, String> {
    let mut checkpoint = match checkpoint_store.load()? {
        Some(checkpoint) => {
            validate_checkpoint(&checkpoint, &preflight)?;
            checkpoint
        }
        None => {
            let checkpoint = new_checkpoint(&preflight);
            checkpoint_store.save_atomic(&checkpoint)?;
            checkpoint
        }
    };

    let mut resolved: Option<ResolvedCorpusPilotModelV1> = None;
    let started = Instant::now();

    for (index, prepared_case) in preflight.prepared.cases.iter().enumerate() {
        if checkpoint.cases[index]
            .fully_attested_completion_attempt
            .is_some()
        {
            continue;
        }

        let attempt = checkpoint.cases[index].attempts.len() + 1;
        checkpoint.cases[index]
            .attempts
            .push(CorpusPilotCheckpointAttemptV1 {
                attempt,
                started_at: captured_at.to_string(),
                outcome: None,
            });
        checkpoint_store.save_atomic(&checkpoint)?;

        if resolved.is_none() {
            match resolver.resolve() {
                Ok(model) => {
                    checkpoint.provider_resolution = Some(model.provider_resolution.clone());
                    checkpoint_store.save_atomic(&checkpoint)?;
                    resolved = Some(model);
                }
                Err(_) => {
                    checkpoint.cases[index].attempts[attempt - 1].outcome =
                        Some(CorpusPilotCheckpointOutcomeV1::Failure {
                            failure_class: "model_resolver_failed".to_string(),
                            latency_ms: 0,
                        });
                    checkpoint_store.save_atomic(&checkpoint)?;
                    break;
                }
            }
        }

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
        let call_started = Instant::now();
        let completion = match resolved
            .as_ref()
            .expect("resolver succeeded above")
            .model
            .generate(request)
            .await
        {
            Ok(completion) => completion,
            Err(_) => {
                checkpoint.cases[index].attempts[attempt - 1].outcome =
                    Some(CorpusPilotCheckpointOutcomeV1::Failure {
                        failure_class: "model_invocation_failed".to_string(),
                        latency_ms: call_started.elapsed().as_millis(),
                    });
                checkpoint_store.save_atomic(&checkpoint)?;
                continue;
            }
        };
        let mut engine_receipt = completion.engine_receipt;
        if completion.truncated {
            engine_receipt.degraded = true;
            engine_receipt
                .fallback_chain
                .push("provider response was truncated".to_string());
        }
        let result = match adapt_corpus_case(
            &preflight.prepared.input.project,
            &preflight.prepared.manifest,
            &prepared_case.bundle,
            Some(completion.draft),
            Some(engine_receipt.clone()),
        ) {
            Ok(result) => result,
            Err(_) => {
                checkpoint.cases[index].attempts[attempt - 1].outcome =
                    Some(CorpusPilotCheckpointOutcomeV1::Failure {
                        failure_class: "candidate_adaptation_failed".to_string(),
                        latency_ms: call_started.elapsed().as_millis(),
                    });
                checkpoint_store.save_atomic(&checkpoint)?;
                continue;
            }
        };
        let cost_latency = CaseLatencyCostV1 {
            latency_ms: completion.latency_ms,
            prompt_tokens: completion.prompt_tokens,
            completion_tokens: completion.completion_tokens,
            total_tokens: completion.total_tokens,
            cost_usd: completion.cost_usd,
            cost_status: completion.cost_status,
            cost_basis: completion.cost_basis,
            cost_version: completion.cost_version,
        };
        let fully_attested = engine_receipt.has_known_identity()
            && result.candidate.coverage.is_full()
            && actual_cost_is_attested(&cost_latency);
        let candidate = result.candidate.clone();
        let report = report_case(
            &prepared_case.owner_case,
            result,
            engine_receipt,
            cost_latency,
        );
        checkpoint.cases[index].attempts[attempt - 1].outcome =
            Some(CorpusPilotCheckpointOutcomeV1::Candidate {
                fully_attested,
                report: Box::new(report),
                candidate: Box::new(candidate),
            });
        if fully_attested {
            checkpoint.cases[index].fully_attested_completion_attempt = Some(attempt);
        }
        checkpoint_store.save_atomic(&checkpoint)?;
    }

    let mut reports = Vec::with_capacity(preflight.prepared.cases.len());
    let mut candidates = Vec::with_capacity(preflight.prepared.cases.len());
    for (index, prepared_case) in preflight.prepared.cases.iter().enumerate() {
        let checkpoint_case = &checkpoint.cases[index];
        let selected = completed_attempt(checkpoint_case)
            .or_else(|| latest_candidate_attempt(checkpoint_case))
            .and_then(|attempt| attempt.outcome.as_ref());
        match selected {
            Some(CorpusPilotCheckpointOutcomeV1::Candidate {
                report, candidate, ..
            }) => {
                reports.push(report.as_ref().clone());
                candidates.push(candidate.as_ref().clone());
            }
            _ => {
                let (failure_class, latency_ms) = checkpoint_case
                    .attempts
                    .last()
                    .and_then(|attempt| attempt.outcome.as_ref())
                    .and_then(|outcome| match outcome {
                        CorpusPilotCheckpointOutcomeV1::Failure {
                            failure_class,
                            latency_ms,
                        } => Some((failure_class.as_str(), *latency_ms)),
                        CorpusPilotCheckpointOutcomeV1::Candidate { .. } => None,
                    })
                    .unwrap_or(("attempt_interrupted_before_receipt", 0));
                reports.push(failure_report_case(
                    prepared_case,
                    failure_class,
                    latency_ms,
                ));
            }
        }
    }

    let completed_cases = checkpoint
        .cases
        .iter()
        .filter(|case| case.fully_attested_completion_attempt.is_some())
        .count();
    let model_invocations = checkpoint
        .cases
        .iter()
        .flat_map(|case| &case.attempts)
        .filter(|attempt| attempt.outcome.is_some())
        .count();
    let complete = completed_cases == CORPUS_PILOT_SIZE;
    let engine_receipt = if complete {
        reports[0].engine_receipt.clone()
    } else {
        preview_only_engine_receipt()
    };
    let receipt_latency_ms: u128 = reports
        .iter()
        .map(|report| report.cost_latency.latency_ms)
        .sum();

    Ok(CorpusPilotExecutionV1 {
        report: CorpusPilotReportV1 {
            schema_version: REPORT_SCHEMA_VERSION.to_string(),
            manifest_sha256: preflight.prepared.manifest_sha256,
            baseline_sha256: Some(preflight.baseline_sha256),
            captured_at: captured_at.to_string(),
            disposition: if complete {
                "complete".to_string()
            } else {
                "partial_preview_only".to_string()
            },
            engine_receipt,
            provider_resolution: checkpoint
                .provider_resolution
                .unwrap_or_else(ProviderResolutionReceiptV1::unknown_without_engine_invocation),
            model_invocations,
            completed_cases,
            candidates_emitted: candidates.len(),
            total_latency_ms: receipt_latency_ms.max(started.elapsed().as_millis()),
            cases: reports,
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

    #[derive(Clone)]
    struct SyntheticModel {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        known_identity: bool,
        full_coverage: bool,
        actual_cost: bool,
        fail_case: Option<String>,
        marker: String,
    }

    impl Default for SyntheticModel {
        fn default() -> Self {
            Self {
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                known_identity: false,
                full_coverage: false,
                actual_cost: false,
                fail_case: None,
                marker: String::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl CorpusPilotModelClient for SyntheticModel {
        async fn generate(
            &self,
            request: CorpusPilotModelRequestV1,
        ) -> Result<CorpusPilotModelCompletionV1, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_case.as_deref() == Some(request.case_id.as_str()) {
                return Err(format!(
                    "RAW_MODEL_FAILURE_BODY {} {}",
                    request.case_id, request.bundle.issue.body
                ));
            }
            let known = self.known_identity;
            let mut situation = format!(
                "{}\n{}",
                request.bundle.issue.title, request.bundle.issue.body
            );
            if self.full_coverage {
                for comment in &request.bundle.issue.selected_comment_revisions {
                    situation.push('\n');
                    situation.push_str(&comment.body);
                }
                if let Some(pull_request) = &request.bundle.pull_request {
                    situation.push('\n');
                    situation.push_str(&pull_request.title);
                    situation.push('\n');
                    situation.push_str(&pull_request.body);
                }
            }
            Ok(CorpusPilotModelCompletionV1 {
                draft: CaseDraft {
                    situation,
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
                cost_usd: self.actual_cost.then(|| "0.001".to_string()),
                cost_status: if self.actual_cost {
                    "provider_reported_actual".to_string()
                } else {
                    "provider_price_not_reported".to_string()
                },
                cost_basis: self
                    .actual_cost
                    .then(|| "provider_response_usage_charge".to_string()),
                cost_version: self.actual_cost.then(|| "fixture-cost-v1".to_string()),
                latency_ms: 9,
                truncated: false,
            })
        }
    }

    struct SyntheticResolver {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        model: SyntheticModel,
    }

    impl SyntheticResolver {
        fn new(model: SyntheticModel) -> Self {
            Self {
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                model,
            }
        }
    }

    impl CorpusPilotModelResolver for SyntheticResolver {
        fn resolve(&self) -> Result<ResolvedCorpusPilotModelV1, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ResolvedCorpusPilotModelV1 {
                provider_resolution: ProviderResolutionReceiptV1 {
                    vault_status_checked: false,
                    provider_cache_loaded: Some(true),
                    identity_proof: "synthetic resolver receipt".to_string(),
                },
                model: Box::new(self.model.clone()),
            })
        }
    }

    #[derive(Default)]
    struct MemoryCheckpointStore {
        checkpoint: std::sync::Mutex<Option<CorpusPilotCheckpointV1>>,
        save_calls: std::sync::atomic::AtomicUsize,
        fail_on_save: std::sync::atomic::AtomicUsize,
    }

    impl MemoryCheckpointStore {
        fn fail_on_save(&self, call: usize) {
            self.fail_on_save
                .store(call, std::sync::atomic::Ordering::SeqCst);
        }

        fn snapshot(&self) -> CorpusPilotCheckpointV1 {
            self.checkpoint
                .lock()
                .expect("checkpoint lock")
                .clone()
                .expect("checkpoint saved")
        }
    }

    impl CorpusPilotCheckpointStore for MemoryCheckpointStore {
        fn load(&self) -> Result<Option<CorpusPilotCheckpointV1>, String> {
            Ok(self.checkpoint.lock().expect("checkpoint lock").clone())
        }

        fn save_atomic(&self, checkpoint: &CorpusPilotCheckpointV1) -> Result<(), String> {
            let call = self
                .save_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if self.fail_on_save.load(std::sync::atomic::Ordering::SeqCst) == call {
                return Err("synthetic atomic checkpoint failure".to_string());
            }
            *self.checkpoint.lock().expect("checkpoint lock") = Some(checkpoint.clone());
            Ok(())
        }
    }

    fn baseline_bytes_for_fixture(input: &[u8], reader: &FixtureCorpusReader) -> Vec<u8> {
        serde_json::to_vec(&baseline_for_fixture(input, reader)).expect("serialize baseline")
    }

    async fn run_fixture_with(
        input: &[u8],
        reader: &dyn GithubCorpusReader,
        baseline_bytes: &[u8],
        store: &dyn CorpusPilotCheckpointStore,
        resolver: &dyn CorpusPilotModelResolver,
    ) -> Result<CorpusPilotExecutionV1, String> {
        run_corpus_pilot_with_resolver(
            input,
            reader,
            "2026-07-24T00:00:00Z",
            &sha256_hex(input),
            baseline_bytes,
            &sha256_hex(baseline_bytes),
            store,
            resolver,
        )
        .await
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
    async fn manifest_baseline_or_live_drift_blocks_resolver_and_model_construction() {
        let reader = fixture_reader(false);
        let input = input_bytes(MERGE_SHA);
        let baseline = baseline_for_fixture(&input, &reader);
        let baseline_bytes = serde_json::to_vec(&baseline).expect("serialize baseline");
        let model = SyntheticModel {
            known_identity: true,
            ..Default::default()
        };
        let resolver = SyntheticResolver::new(model.clone());

        let err = run_corpus_pilot_with_resolver(
            &input,
            &reader,
            "2026-07-24T00:00:00Z",
            "not-the-fixture-digest",
            &baseline_bytes,
            &sha256_hex(&baseline_bytes),
            &MemoryCheckpointStore::default(),
            &resolver,
        )
        .await
        .expect_err("wrong manifest digest must stop before model spend");
        assert!(err.contains("manifest digest mismatch"), "{err}");
        assert_eq!(
            model.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "manifest failure must occur before the model client is reachable"
        );
        assert_eq!(
            resolver.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "manifest failure must not resolve or construct a model"
        );

        let err = run_corpus_pilot_with_resolver(
            &input,
            &reader,
            "2026-07-24T00:00:00Z",
            &sha256_hex(&input),
            &baseline_bytes,
            "not-the-baseline-digest",
            &MemoryCheckpointStore::default(),
            &resolver,
        )
        .await
        .expect_err("wrong baseline digest must stop before live provenance or spend");
        assert!(err.contains("baseline artifact digest mismatch"), "{err}");
        assert_eq!(
            resolver.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "baseline artifact failure must not resolve or construct a model"
        );

        let mut drifted = baseline.clone();
        drifted.cases[0].issue_snapshot_hash = "drifted".to_string();
        let drifted_bytes = serde_json::to_vec(&drifted).expect("serialize drifted baseline");
        let err = run_corpus_pilot_with_resolver(
            &input,
            &reader,
            "2026-07-24T00:00:00Z",
            &sha256_hex(&input),
            &drifted_bytes,
            &sha256_hex(&drifted_bytes),
            &MemoryCheckpointStore::default(),
            &resolver,
        )
        .await
        .expect_err("immutable snapshot drift must stop before model spend");
        assert!(err.contains("immutable provenance drift"), "{err}");
        assert_eq!(
            model.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "provenance drift must occur before the model client is reachable"
        );
        assert_eq!(
            resolver.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "live provenance drift must not resolve or construct a model"
        );
    }

    #[tokio::test]
    async fn synthetic_model_receipt_is_pending_only_counted_only_when_known_and_redacted() {
        let input = input_bytes(MERGE_SHA);
        let fixture = fixture_reader(false);
        let baseline_bytes = baseline_bytes_for_fixture(&input, &fixture);
        let reader = MutationProbe::new(fixture);
        let model = SyntheticModel {
            known_identity: true,
            full_coverage: true,
            actual_cost: true,
            marker: "RAW_MODEL_OUTPUT_MUST_NOT_APPEAR_IN_REPORT".to_string(),
            ..Default::default()
        };
        let resolver = SyntheticResolver::new(model);
        let store = MemoryCheckpointStore::default();

        let execution = run_fixture_with(&input, &reader, &baseline_bytes, &store, &resolver)
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
                && case.cost_latency.cost_status == "provider_reported_actual"
                && case
                    .source_coverage
                    .is_some_and(|coverage| coverage.is_full())
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
        let baseline_bytes = baseline_bytes_for_fixture(&input, &reader);
        let model = SyntheticModel {
            full_coverage: true,
            actual_cost: true,
            ..Default::default()
        };
        let resolver = SyntheticResolver::new(model);
        let store = MemoryCheckpointStore::default();

        let execution = run_fixture_with(&input, &reader, &baseline_bytes, &store, &resolver)
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

    #[tokio::test]
    async fn completion_requires_full_source_coverage_and_actual_cost_matrix() {
        let input = input_bytes(MERGE_SHA);
        let reader = fixture_reader(false);
        let baseline_bytes = baseline_bytes_for_fixture(&input, &reader);

        for (full_coverage, actual_cost, expected_complete) in [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            let resolver = SyntheticResolver::new(SyntheticModel {
                known_identity: true,
                full_coverage,
                actual_cost,
                ..Default::default()
            });
            let store = MemoryCheckpointStore::default();
            let execution = run_fixture_with(&input, &reader, &baseline_bytes, &store, &resolver)
                .await
                .expect("synthetic coverage/cost matrix run");

            assert_eq!(
                execution.report.completed_cases,
                if expected_complete {
                    CORPUS_PILOT_SIZE
                } else {
                    0
                },
                "full_coverage={full_coverage} actual_cost={actual_cost}"
            );
            assert_eq!(
                execution.report.disposition,
                if expected_complete {
                    "complete"
                } else {
                    "partial_preview_only"
                }
            );
        }
    }

    #[tokio::test]
    async fn checkpoint_preserves_partial_case_twenty_and_restart_skips_only_completed_cases() {
        let input = input_bytes(MERGE_SHA);
        let reader = fixture_reader(false);
        let baseline_bytes = baseline_bytes_for_fixture(&input, &reader);
        let store = MemoryCheckpointStore::default();

        let first_model = SyntheticModel {
            known_identity: true,
            full_coverage: true,
            actual_cost: true,
            fail_case: Some("owner-case-19".to_string()),
            ..Default::default()
        };
        let first_calls = first_model.calls.clone();
        let first_resolver = SyntheticResolver::new(first_model);
        let first = run_fixture_with(&input, &reader, &baseline_bytes, &store, &first_resolver)
            .await
            .expect("case twenty failure must still return a partial report");
        assert_eq!(first_calls.load(std::sync::atomic::Ordering::SeqCst), 20);
        assert_eq!(first.report.completed_cases, 19);
        assert_eq!(first.report.cases.len(), CORPUS_PILOT_SIZE);
        assert_eq!(first.report.candidates_emitted, 19);
        assert_eq!(first.report.disposition, "partial_preview_only");
        assert_eq!(
            first.report.cases[19].failure_class.as_deref(),
            Some("model_invocation_failed")
        );
        assert!(!serde_json::to_string(&first.report)
            .expect("serialize partial report")
            .contains("RAW_MODEL_FAILURE_BODY"));

        let first_checkpoint = store.snapshot();
        assert!(first_checkpoint.cases[..19]
            .iter()
            .all(|case| case.fully_attested_completion_attempt == Some(1)));
        assert!(matches!(
            first_checkpoint.cases[19].attempts[0].outcome.as_ref(),
            Some(CorpusPilotCheckpointOutcomeV1::Failure { failure_class, .. })
                if failure_class == "model_invocation_failed"
        ));

        let retry_model = SyntheticModel {
            known_identity: true,
            full_coverage: true,
            actual_cost: true,
            ..Default::default()
        };
        let retry_calls = retry_model.calls.clone();
        let retry_resolver = SyntheticResolver::new(retry_model);
        let retry = run_fixture_with(&input, &reader, &baseline_bytes, &store, &retry_resolver)
            .await
            .expect("restart completes only the unfinished case");
        assert_eq!(retry_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(retry.report.completed_cases, CORPUS_PILOT_SIZE);
        assert_eq!(retry.report.candidates_emitted, CORPUS_PILOT_SIZE);
        assert_eq!(retry.report.disposition, "complete");
        assert_eq!(store.snapshot().cases[19].attempts.len(), 2);

        let completed_model = SyntheticModel::default();
        let completed_calls = completed_model.calls.clone();
        let completed_resolver = SyntheticResolver::new(completed_model);
        let completed = run_fixture_with(
            &input,
            &reader,
            &baseline_bytes,
            &store,
            &completed_resolver,
        )
        .await
        .expect("fully completed matching checkpoint is idempotent");
        assert_eq!(completed.report.disposition, "complete");
        assert_eq!(completed_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            completed_resolver
                .calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an all-complete restart must not resolve Vault or construct a model"
        );

        store
            .checkpoint
            .lock()
            .expect("checkpoint lock")
            .as_mut()
            .expect("checkpoint saved")
            .baseline_sha256 = "mismatched-ledger-binding".to_string();
        let mismatch_model = SyntheticModel::default();
        let mismatch_resolver = SyntheticResolver::new(mismatch_model);
        let err = run_fixture_with(&input, &reader, &baseline_bytes, &store, &mismatch_resolver)
            .await
            .expect_err("mismatched ledger must be rejected");
        assert!(err.contains("checkpoint does not match"), "{err}");
        assert_eq!(
            mismatch_resolver
                .calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn checkpoint_failure_matrix_refuses_spend_or_stops_before_next_case() {
        let input = input_bytes(MERGE_SHA);
        let reader = fixture_reader(false);
        let baseline_bytes = baseline_bytes_for_fixture(&input, &reader);

        let attempt_store = MemoryCheckpointStore::default();
        attempt_store.fail_on_save(2);
        let attempt_model = SyntheticModel::default();
        let attempt_calls = attempt_model.calls.clone();
        let attempt_resolver = SyntheticResolver::new(attempt_model);
        run_fixture_with(
            &input,
            &reader,
            &baseline_bytes,
            &attempt_store,
            &attempt_resolver,
        )
        .await
        .expect_err("attempt checkpoint failure must be loud");
        assert_eq!(attempt_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            attempt_resolver
                .calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );

        for fail_case in [None, Some("owner-case-0".to_string())] {
            let receipt_store = MemoryCheckpointStore::default();
            receipt_store.fail_on_save(4);
            let receipt_model = SyntheticModel {
                known_identity: true,
                full_coverage: true,
                actual_cost: true,
                fail_case,
                ..Default::default()
            };
            let receipt_calls = receipt_model.calls.clone();
            let receipt_resolver = SyntheticResolver::new(receipt_model);
            run_fixture_with(
                &input,
                &reader,
                &baseline_bytes,
                &receipt_store,
                &receipt_resolver,
            )
            .await
            .expect_err("completion/failure receipt checkpoint failure must be loud");
            assert_eq!(receipt_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }
}

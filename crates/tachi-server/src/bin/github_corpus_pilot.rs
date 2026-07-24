//! Owner-operated, read-only launcher for the #1059 exact-20 corpus pilot.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use async_trait::async_trait;
use clap::Parser;
use serde::Deserialize;
use serde_json::{json, Value};
use tachi_server::github_corpus_ops::live_pilot::{
    dry_run_owner_approved_corpus_pilot, run_owner_approved_corpus_pilot, CorpusPilotModelClient,
    CorpusPilotModelCompletionV1, CorpusPilotModelRequestV1, CorpusPilotProvenanceBaselineV1,
    CorpusPilotReportV1, ProviderResolutionReceiptV1,
};
use tachi_server::github_corpus_ops::GithubCorpusReader;

const ISSUE_FIELDS: &str = "number,title,body,state,labels,milestone,updatedAt,comments";
const PR_FIELDS: &str =
    "number,title,body,state,headRefOid,baseRefOid,updatedAt,mergeCommit,reviews,statusCheckRollup";

#[derive(Debug, Parser)]
#[command(about = "Run the read-only #1059 exact-20 GitHub corpus pilot")]
struct Args {
    /// Owner-approved manifest, committed in this repository.
    #[arg(long)]
    manifest: PathBuf,
    /// JSON PilotReport output. The runner writes no GitHub or database state.
    #[arg(long)]
    report: PathBuf,
    /// Committed preview receipt whose immutable snapshot hashes are checked
    /// against every live GitHub read before a model can be invoked.
    #[arg(long)]
    baseline_report: PathBuf,
    /// Immutable capture time recorded on every adapter receipt.
    #[arg(long)]
    captured_at: String,
    /// Explicitly permit the bounded, real-model phase after a fresh cold
    /// review. Omission is a no-spend dry run that still validates manifest and
    /// live immutable provenance drift.
    #[arg(long)]
    execute: bool,
}

struct GhCliReader;

impl GhCliReader {
    fn run_json(args: &[&str]) -> Result<Value, String> {
        let output = Command::new("gh")
            .args(args)
            .output()
            .map_err(|error| format!("launch read-only gh command: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "read-only gh command exited {}",
                output.status.code().unwrap_or(-1)
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("parse read-only gh JSON response: {error}"))
    }
}

impl GithubCorpusReader for GhCliReader {
    fn read_issue_json(&self, repo: &str, issue_number: u64) -> Result<Value, String> {
        let number = issue_number.to_string();
        Self::run_json(&[
            "issue",
            "view",
            &number,
            "--repo",
            repo,
            "--json",
            ISSUE_FIELDS,
        ])
    }

    fn read_pr_json(&self, repo: &str, pr_number: u64) -> Result<Value, String> {
        let number = pr_number.to_string();
        Self::run_json(&["pr", "view", &number, "--repo", repo, "--json", PR_FIELDS])
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelDraftV1 {
    situation: String,
    proposed_ruling: String,
    why: String,
    how_to_apply: String,
}

struct TachiReasoningModelClient {
    llm: tachi_llm::LlmClient,
}

impl TachiReasoningModelClient {
    fn from_existing_tachi_resolver() -> Result<(Self, ProviderResolutionReceiptV1), String> {
        // The existing resolver and materializer own every credential read and
        // injection. This binary receives only the boolean count outcome, never
        // Vault entries, environment values, or key material.
        let (llm, provider_cache_loaded) = tachi_server::resolve_standalone_tachi_model_client()
            .map_err(|_| "Tachi provider resolver or secret materializer failed".to_string())?;
        Ok((
            Self { llm },
            ProviderResolutionReceiptV1 {
                vault_status_checked: true,
                provider_cache_loaded: Some(provider_cache_loaded),
                identity_proof: "existing Tachi resolver and secret materializer completed; only a provider response may attest effective provider/model/version".to_string(),
            },
        ))
    }

    fn prompt(request: &CorpusPilotModelRequestV1) -> Result<String, String> {
        // The prompt stays in process and is never written into PilotReport.
        serde_json::to_string(&json!({
            "task": "Return JSON only with situation, proposed_ruling, why, and how_to_apply. Preserve owner-selected evidence; never establish authority or propose GitHub mutation.",
            "case_id": request.case_id,
            "selection_reason": request.selection_reason,
            "reference_decision": request.reference_decision,
            "cold_start_material_decision": request.cold_start_material_decision,
            "issue": request.bundle.issue,
            "pull_request": request.bundle.pull_request,
            "provenance_events": request.bundle.events,
        }))
        .map_err(|_| "serialize model request".to_string())
    }

    fn parse_draft(raw: &str) -> Result<tachi_server::github_corpus_ops::adapt::CaseDraft, String> {
        let payload = tachi_llm::LlmClient::extract_json_payload(raw)
            .map_err(|_| "model response did not contain a JSON object".to_string())?;
        let draft: ModelDraftV1 = serde_json::from_str(payload)
            .map_err(|_| "model response schema was invalid".to_string())?;
        if [
            draft.situation.as_str(),
            draft.proposed_ruling.as_str(),
            draft.why.as_str(),
            draft.how_to_apply.as_str(),
        ]
        .iter()
        .any(|field| field.trim().is_empty())
        {
            return Err("model response contained an empty required draft field".to_string());
        }
        Ok(tachi_server::github_corpus_ops::adapt::CaseDraft {
            situation: draft.situation,
            proposed_ruling: draft.proposed_ruling,
            why: draft.why,
            how_to_apply: draft.how_to_apply,
        })
    }
}

#[async_trait]
impl CorpusPilotModelClient for TachiReasoningModelClient {
    async fn generate(
        &self,
        request: CorpusPilotModelRequestV1,
    ) -> Result<CorpusPilotModelCompletionV1, String> {
        let prompt = Self::prompt(&request)?;
        let outcome = self
            .llm
            .call_reasoning_llm_provider_only_with_receipt(
                "You are a bounded evidence distiller. Return only the requested JSON object.",
                &prompt,
                None,
                0.0,
                800,
            )
            .await
            .map_err(|_| "provider invocation failed".to_string())?;
        let draft = Self::parse_draft(&outcome.text)?;
        Ok(CorpusPilotModelCompletionV1 {
            draft,
            engine_receipt: tachi_params::LessonEngineReceiptV1 {
                requested_role: "github_corpus_exact20".to_string(),
                effective_provider: Some(outcome.receipt.effective_provider),
                effective_model: outcome.receipt.effective_model,
                effective_version: outcome.receipt.effective_version,
                fallback_chain: outcome.receipt.fallback_chain,
                degraded: outcome.receipt.degraded,
            },
            prompt_tokens: outcome.receipt.prompt_tokens,
            completion_tokens: outcome.receipt.completion_tokens,
            total_tokens: outcome.receipt.total_tokens,
            cost_usd: None,
            cost_status: "provider_price_not_reported".to_string(),
            latency_ms: outcome.receipt.latency_ms,
            truncated: outcome.truncated,
        })
    }
}

fn write_report(path: &std::path::Path, report: &CorpusPilotReportV1) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("serialize pilot report: {error}"))?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("report path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create report directory {}: {error}", parent.display()))?;
    std::fs::write(path, body).map_err(|error| format!("write report {}: {error}", path.display()))
}

async fn run(args: Args) -> Result<(CorpusPilotReportV1, bool), String> {
    let input = std::fs::read(&args.manifest)
        .map_err(|error| format!("read manifest {}: {error}", args.manifest.display()))?;
    let baseline_bytes = std::fs::read(&args.baseline_report).map_err(|error| {
        format!(
            "read baseline report {}: {error}",
            args.baseline_report.display()
        )
    })?;
    let baseline: CorpusPilotProvenanceBaselineV1 = serde_json::from_slice(&baseline_bytes)
        .map_err(|_| "parse baseline report provenance fields".to_string())?;
    let (report, executed) = if args.execute {
        let (model, provider_resolution) =
            TachiReasoningModelClient::from_existing_tachi_resolver()?;
        (
            run_owner_approved_corpus_pilot(
                &input,
                &GhCliReader,
                &args.captured_at,
                &baseline,
                provider_resolution,
                &model,
            )
            .await?
            .report,
            true,
        )
    } else {
        (
            dry_run_owner_approved_corpus_pilot(
                &input,
                &GhCliReader,
                &args.captured_at,
                &baseline,
            )?,
            false,
        )
    };
    write_report(&args.report, &report)?;
    Ok((report, executed))
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok((report, executed)) => {
            println!(
                "#1059 pilot: disposition={} candidates={} manifest_sha256={} executed={} effective_engine_gate={} report contains no prompt, model output, or credential values",
                report.disposition,
                report.candidates_emitted,
                report.manifest_sha256,
                executed,
                if executed { "attested_per_case" } else { "blocked_no_execute" },
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("github-corpus-pilot: {error}");
            ExitCode::from(1)
        }
    }
}

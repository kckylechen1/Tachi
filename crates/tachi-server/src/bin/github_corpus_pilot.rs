//! Owner-operated, read-only launcher for the #1059 exact-20 corpus pilot.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

use async_trait::async_trait;
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tachi_server::github_corpus_ops::live_pilot::{
    dry_run_owner_approved_corpus_pilot, rebaseline_owner_approved_corpus_pilot,
    run_owner_approved_corpus_pilot, CorpusPilotCheckpointStore, CorpusPilotCheckpointV1,
    CorpusPilotFailureClassV1, CorpusPilotModelClient, CorpusPilotModelCompletionV1,
    CorpusPilotModelFailureV1, CorpusPilotModelRequestV1, CorpusPilotModelResolver,
    CorpusPilotReportV1, ProviderResolutionReceiptV1, ResolvedCorpusPilotModelV1,
};
use tachi_server::github_corpus_ops::GithubCorpusReader;

const ISSUE_FIELDS: &str = "number,title,body,state,labels,milestone,updatedAt,comments";
const PR_FIELDS: &str =
    "number,title,body,state,headRefOid,baseRefOid,updatedAt,mergeCommit,reviews,statusCheckRollup";

#[derive(Debug, Parser)]
#[command(about = "Run the read-only #1059 exact-20 GitHub corpus pilot")]
struct Args {
    /// Owner-approved manifest, committed in this repository.
    #[arg(long, required_unless_present = "auth_clearance_probe")]
    manifest: Option<PathBuf>,
    /// JSON PilotReport output. The runner writes no GitHub or database state.
    #[arg(long, required_unless_present = "auth_clearance_probe")]
    report: Option<PathBuf>,
    /// Committed preview receipt whose immutable snapshot hashes are checked
    /// against every live GitHub read before a model can be invoked. During
    /// --rebaseline it is protected from output aliasing but never read.
    #[arg(long, required_unless_present = "auth_clearance_probe")]
    baseline_report: Option<PathBuf>,
    /// Owner-approved SHA-256 of the exact baseline report bytes.
    #[arg(
        long,
        required_unless_present_any = ["rebaseline", "auth_clearance_probe"]
    )]
    baseline_sha256: Option<String>,
    /// Durable, atomically replaced partial-spend checkpoint. Required with
    /// --execute; a dry run never creates it.
    #[arg(long)]
    checkpoint: Option<PathBuf>,
    /// Immutable capture time recorded on every adapter receipt.
    #[arg(long, required_unless_present = "auth_clearance_probe")]
    captured_at: Option<String>,
    /// Explicitly permit the bounded, real-model phase after a fresh cold
    /// review. Omission is a no-spend dry run that still validates manifest and
    /// live immutable provenance drift.
    #[arg(long)]
    execute: bool,
    /// Recompute the preview-only provenance baseline from the exact manifest
    /// and live read-only GitHub metadata. Never reads the old baseline or
    /// constructs a model resolver. Output must be a distinct temporary path.
    #[arg(long, conflicts_with = "execute")]
    rebaseline: bool,
    /// Resolve the normal reasoning provider credential read-only and perform
    /// one official, non-generating auth/model-list GET. This branch runs
    /// before any manifest, baseline, checkpoint, report, or GitHub access.
    #[arg(long, conflicts_with_all = ["execute", "rebaseline"])]
    auth_clearance_probe: bool,
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

struct TachiModelResolver;

#[derive(Debug, Serialize)]
struct AuthClearanceReceipt {
    #[serde(flatten)]
    probe: tachi_llm::ProviderAuthProbeResult,
    vault_status_checked: bool,
    provider_cache_loaded: bool,
}

async fn run_auth_clearance_probe() -> Result<AuthClearanceReceipt, String> {
    let (llm, provider_cache_loaded) = tachi_server::resolve_standalone_tachi_model_client()
        .map_err(|_| "Tachi provider resolver or secret materializer failed".to_string())?;
    Ok(AuthClearanceReceipt {
        probe: llm.probe_reasoning_auth_no_content().await,
        vault_status_checked: true,
        provider_cache_loaded,
    })
}

impl CorpusPilotModelResolver for TachiModelResolver {
    fn resolve(&self) -> Result<ResolvedCorpusPilotModelV1, String> {
        let (model, provider_resolution) =
            TachiReasoningModelClient::from_existing_tachi_resolver()?;
        Ok(ResolvedCorpusPilotModelV1 {
            provider_resolution,
            model: Box::new(model),
        })
    }
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
    ) -> Result<CorpusPilotModelCompletionV1, CorpusPilotModelFailureV1> {
        let prompt = Self::prompt(&request).map_err(|_| CorpusPilotModelFailureV1 {
            failure_class: CorpusPilotFailureClassV1::LaneOutage,
            provider_attempts: 0,
            latency_ms: 0,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
        })?;
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
            .map_err(|failure| CorpusPilotModelFailureV1 {
                failure_class: match failure.class {
                    tachi_llm::ProviderInvocationFailureClass::AuthFailed => {
                        CorpusPilotFailureClassV1::AuthFailed
                    }
                    tachi_llm::ProviderInvocationFailureClass::ProviderExhausted => {
                        CorpusPilotFailureClassV1::ProviderExhausted
                    }
                    tachi_llm::ProviderInvocationFailureClass::Transient => {
                        CorpusPilotFailureClassV1::Transient
                    }
                    tachi_llm::ProviderInvocationFailureClass::LaneOutage => {
                        CorpusPilotFailureClassV1::LaneOutage
                    }
                },
                provider_attempts: failure.provider_attempts,
                latency_ms: failure.latency_ms,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
            })?;
        let draft = Self::parse_draft(&outcome.text).map_err(|_| CorpusPilotModelFailureV1 {
            failure_class: CorpusPilotFailureClassV1::LaneOutage,
            provider_attempts: 1,
            latency_ms: outcome.receipt.latency_ms,
            prompt_tokens: outcome.receipt.prompt_tokens,
            completion_tokens: outcome.receipt.completion_tokens,
            total_tokens: outcome.receipt.total_tokens,
        })?;
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
            provider_attempts: 1,
            prompt_tokens: outcome.receipt.prompt_tokens,
            completion_tokens: outcome.receipt.completion_tokens,
            total_tokens: outcome.receipt.total_tokens,
            cost_usd: None,
            cost_status: "provider_price_not_reported".to_string(),
            cost_basis: None,
            cost_version: None,
            latency_ms: outcome.receipt.latency_ms,
            truncated: outcome.truncated,
        })
    }
}

struct FileCheckpointStore {
    path: PathBuf,
}

impl FileCheckpointStore {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn temporary_path(&self) -> PathBuf {
        self.path.with_extension(format!(
            "checkpoint-tmp-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }
}

impl CorpusPilotCheckpointStore for FileCheckpointStore {
    fn load(&self) -> Result<Option<CorpusPilotCheckpointV1>, String> {
        if !self.path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&self.path)
            .map_err(|error| format!("read checkpoint {}: {error}", self.path.display()))?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "checkpoint JSON is invalid".to_string())
    }

    fn save_atomic(&self, checkpoint: &CorpusPilotCheckpointV1) -> Result<(), String> {
        let body = serde_json::to_vec_pretty(checkpoint)
            .map_err(|_| "serialize checkpoint".to_string())?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "checkpoint path has no parent".to_string())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create checkpoint directory: {error}"))?;
        let temporary = self.temporary_path();
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("create checkpoint temporary file: {error}"))?;
        file.write_all(&body)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("persist checkpoint temporary file: {error}"))?;
        std::fs::rename(&temporary, &self.path)
            .map_err(|error| format!("atomically replace checkpoint: {error}"))?;
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync checkpoint directory: {error}"))?;
        Ok(())
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

fn normalized_path_for_comparison(path: &Path) -> Result<PathBuf, String> {
    let original = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("resolve current directory: {error}"))?
            .join(path)
    };
    if let Ok(canonical) = std::fs::canonicalize(&original) {
        return Ok(canonical);
    }

    let mut resolved = PathBuf::new();
    let mut unresolved = false;
    for component in original.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if unresolved {
                    return Err(format!(
                        "cannot safely resolve path identity through `..` after a nonexistent component: {}",
                        path.display()
                    ));
                }
                resolved = std::fs::canonicalize(resolved.join("..")).map_err(|error| {
                    format!(
                        "cannot safely resolve parent traversal in {}: {error}",
                        path.display()
                    )
                })?;
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                if !matches!(component, Component::Normal(_)) {
                    resolved.push(component.as_os_str());
                    continue;
                }
                if unresolved {
                    resolved.push(component.as_os_str());
                    continue;
                }
                let candidate = resolved.join(component.as_os_str());
                match std::fs::canonicalize(&candidate) {
                    Ok(canonical) => resolved = canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        match std::fs::symlink_metadata(&candidate) {
                            Ok(metadata) if metadata.file_type().is_symlink() => {
                                return Err(format!(
                                    "cannot safely resolve dangling symlink identity: {}",
                                    candidate.display()
                                ));
                            }
                            Ok(_) => {
                                return Err(format!(
                                    "cannot safely resolve existing path identity: {}",
                                    candidate.display()
                                ));
                            }
                            Err(metadata_error)
                                if metadata_error.kind() == std::io::ErrorKind::NotFound =>
                            {
                                unresolved = true;
                                resolved.push(component.as_os_str());
                            }
                            Err(metadata_error) => {
                                return Err(format!(
                                    "cannot inspect path identity {}: {metadata_error}",
                                    candidate.display()
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        return Err(format!(
                            "cannot safely resolve path identity {}: {error}",
                            candidate.display()
                        ));
                    }
                }
            }
        }
    }
    Ok(resolved)
}

#[cfg(unix)]
fn existing_paths_share_inode(left: &Path, right: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = |path: &Path| match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot inspect path identity {}: {error}",
            path.display()
        )),
    };
    let (Some(left), Some(right)) = (metadata(left)?, metadata(right)?) else {
        return Ok(false);
    };
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(not(unix))]
fn existing_paths_share_inode(_left: &Path, _right: &Path) -> Result<bool, String> {
    Ok(false)
}

fn refuse_baseline_report_alias(args: &Args) -> Result<(), String> {
    let baseline_report = args
        .baseline_report
        .as_deref()
        .ok_or_else(|| "--baseline-report is required for pilot modes".to_string())?;
    let report = args
        .report
        .as_deref()
        .ok_or_else(|| "--report is required for pilot modes".to_string())?;
    let baseline = normalized_path_for_comparison(baseline_report)?;
    let normalized_report = normalized_path_for_comparison(report)?;
    if baseline == normalized_report || existing_paths_share_inode(baseline_report, report)? {
        return Err("--baseline-report and --report must resolve to distinct paths".to_string());
    }
    Ok(())
}

async fn run(args: Args) -> Result<(CorpusPilotReportV1, bool), String> {
    refuse_baseline_report_alias(&args)?;
    let manifest = args
        .manifest
        .as_deref()
        .ok_or_else(|| "--manifest is required for pilot modes".to_string())?;
    let report_path = args
        .report
        .as_deref()
        .ok_or_else(|| "--report is required for pilot modes".to_string())?;
    let baseline_report = args
        .baseline_report
        .as_deref()
        .ok_or_else(|| "--baseline-report is required for pilot modes".to_string())?;
    let captured_at = args
        .captured_at
        .as_deref()
        .ok_or_else(|| "--captured-at is required for pilot modes".to_string())?;
    let input = std::fs::read(manifest)
        .map_err(|error| format!("read manifest {}: {error}", manifest.display()))?;
    if args.rebaseline {
        let report = rebaseline_owner_approved_corpus_pilot(&input, &GhCliReader, captured_at)?;
        write_report(report_path, &report)?;
        return Ok((report, false));
    }
    let baseline_bytes = std::fs::read(baseline_report).map_err(|error| {
        format!(
            "read baseline report {}: {error}",
            baseline_report.display()
        )
    })?;
    let baseline_sha256 = args
        .baseline_sha256
        .as_deref()
        .ok_or_else(|| "--baseline-sha256 is required unless --rebaseline is set".to_string())?;
    let (report, executed) = if args.execute {
        let checkpoint = args
            .checkpoint
            .ok_or_else(|| "--execute requires --checkpoint".to_string())?;
        let checkpoint_store = FileCheckpointStore::new(checkpoint);
        (
            run_owner_approved_corpus_pilot(
                &input,
                &GhCliReader,
                captured_at,
                &baseline_bytes,
                baseline_sha256,
                &checkpoint_store,
                &TachiModelResolver,
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
                captured_at,
                &baseline_bytes,
                baseline_sha256,
            )?,
            false,
        )
    };
    write_report(report_path, &report)?;
    Ok((report, executed))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    if args.auth_clearance_probe {
        return match run_auth_clearance_probe().await {
            Ok(receipt) => {
                let cleared = receipt.probe.clears_configured_model();
                match serde_json::to_string(&receipt) {
                    Ok(public_safe_json) => println!("{public_safe_json}"),
                    Err(_) => {
                        eprintln!("github-corpus-pilot: serialize public-safe auth probe receipt");
                        return ExitCode::from(1);
                    }
                }
                if cleared {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(error) => {
                eprintln!("github-corpus-pilot: {error}");
                ExitCode::from(1)
            }
        };
    }
    match run(args).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_clearance_probe_requires_no_corpus_arguments() {
        let parsed = Args::try_parse_from(["github-corpus-pilot", "--auth-clearance-probe"]);
        assert!(parsed.is_ok(), "{parsed:?}");
        assert!(parsed.expect("accepted probe CLI").auth_clearance_probe);
    }

    #[test]
    fn normal_pilot_still_requires_each_corpus_argument() {
        let complete = [
            "github-corpus-pilot",
            "--manifest",
            "manifest.json",
            "--report",
            "report.json",
            "--baseline-report",
            "baseline.json",
            "--baseline-sha256",
            "owner-approved-digest",
            "--captured-at",
            "2026-07-24T00:00:00Z",
        ];
        assert!(Args::try_parse_from(complete).is_ok());

        for required in [
            "--manifest",
            "--report",
            "--baseline-report",
            "--baseline-sha256",
            "--captured-at",
        ] {
            let mut incomplete = complete.to_vec();
            let position = incomplete
                .iter()
                .position(|argument| *argument == required)
                .expect("required argument in complete shape");
            incomplete.drain(position..=position + 1);
            let error = Args::try_parse_from(incomplete)
                .expect_err("normal pilot mode must require every corpus input");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument,
                "missing {required}"
            );
        }
    }

    #[tokio::test]
    async fn rebaseline_skips_old_baseline_bytes_and_pins_manifest_before_github() {
        let root = tempfile::tempdir().expect("tempdir");
        let manifest = root.path().join("wrong-manifest.json");
        std::fs::write(&manifest, b"{}").expect("write wrong manifest");
        let error = run(Args {
            manifest: Some(manifest),
            report: Some(root.path().join("new-baseline.json")),
            baseline_report: Some(root.path().join("old-baseline-must-not-be-read.json")),
            baseline_sha256: None,
            checkpoint: None,
            captured_at: Some("2026-07-24T04:56:05Z".to_string()),
            execute: false,
            rebaseline: true,
            auth_clearance_probe: false,
        })
        .await
        .expect_err("wrong manifest must stop rebaseline before GitHub reads");
        assert!(error.contains("manifest digest mismatch"), "{error}");
        assert!(!error.contains("read baseline report"), "{error}");
    }

    #[tokio::test]
    async fn report_alias_is_refused_before_any_input_read() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(root.path().join("nested")).expect("create nested directory");
        let error = run(Args {
            manifest: Some(root.path().join("manifest-must-not-be-read.json")),
            report: Some(root.path().join("result.json")),
            baseline_report: Some(root.path().join("./nested/../result.json")),
            baseline_sha256: Some("not-read".to_string()),
            checkpoint: None,
            captured_at: Some("2026-07-24T00:00:00Z".to_string()),
            execute: false,
            rebaseline: false,
            auth_clearance_probe: false,
        })
        .await
        .expect_err("report output must not alias the immutable baseline");
        assert!(error.contains("distinct"), "{error}");
        assert!(!error.contains("read manifest"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_parent_alias_is_refused_before_any_input_read() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        let real = root.path().join("real");
        std::fs::create_dir_all(real.join("subdir")).expect("create real/subdir");
        let report = real.join("result.json");
        std::fs::write(&report, b"owner-approved baseline").expect("write baseline");
        symlink(real.join("subdir"), root.path().join("link")).expect("create directory symlink");

        let error = run(Args {
            manifest: Some(root.path().join("manifest-must-not-be-read.json")),
            report: Some(report),
            baseline_report: Some(root.path().join("link/../result.json")),
            baseline_sha256: Some("not-read".to_string()),
            checkpoint: None,
            captured_at: Some("2026-07-24T00:00:00Z".to_string()),
            execute: false,
            rebaseline: false,
            auth_clearance_probe: false,
        })
        .await
        .expect_err("symlink/.. alias must not overwrite the baseline");
        assert!(error.contains("distinct"), "{error}");
        assert!(!error.contains("read manifest"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn nonexistent_output_uses_original_symlink_traversal_to_existing_ancestor() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        let real = root.path().join("real");
        std::fs::create_dir_all(real.join("subdir")).expect("create real/subdir");
        symlink(real.join("subdir"), root.path().join("link")).expect("create directory symlink");

        let resolved =
            normalized_path_for_comparison(&root.path().join("link/../new-output/result.json"))
                .expect("nearest existing ancestor resolves");
        assert_eq!(
            resolved,
            std::fs::canonicalize(real)
                .expect("canonical real directory")
                .join("new-output/result.json")
        );
    }

    #[test]
    fn unresolved_parent_traversal_is_refused_instead_of_guessed() {
        let root = tempfile::tempdir().expect("tempdir");
        let error =
            normalized_path_for_comparison(&root.path().join("missing-directory/../result.json"))
                .expect_err("parent traversal after an unresolved component is ambiguous");
        assert!(error.contains("cannot safely resolve"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hard_link_alias_is_refused_before_any_input_read() {
        let root = tempfile::tempdir().expect("tempdir");
        let baseline = root.path().join("baseline.json");
        let report = root.path().join("report.json");
        std::fs::write(&baseline, b"owner-approved baseline").expect("write baseline");
        std::fs::hard_link(&baseline, &report).expect("create hard link");

        let error = run(Args {
            manifest: Some(root.path().join("manifest-must-not-be-read.json")),
            report: Some(report),
            baseline_report: Some(baseline),
            baseline_sha256: Some("not-read".to_string()),
            checkpoint: None,
            captured_at: Some("2026-07-24T00:00:00Z".to_string()),
            execute: false,
            rebaseline: false,
            auth_clearance_probe: false,
        })
        .await
        .expect_err("hard-link alias must not overwrite the baseline");
        assert!(error.contains("distinct"), "{error}");
        assert!(!error.contains("read manifest"), "{error}");
    }
}

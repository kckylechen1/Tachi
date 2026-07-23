//! Owner-operated, read-only launcher for the #1059 exact-20 corpus pilot.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use clap::Parser;
use serde_json::Value;
use tachi_server::github_corpus_ops::live_pilot::{
    preview_only_engine_receipt, run_corpus_pilot, CorpusPilotReportV1, ProviderResolutionReceiptV1,
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
    /// Immutable capture time recorded on every adapter receipt.
    #[arg(long)]
    captured_at: String,
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

/// This asks the existing Vault status surface only. It intentionally does not
/// call "tachi env", inspect configuration values, or print any credential.
fn provider_resolution_receipt() -> ProviderResolutionReceiptV1 {
    let output = match Command::new("tachi").args(["vault", "status"]).output() {
        Ok(output) if output.status.success() => output,
        Ok(_) | Err(_) => {
            return ProviderResolutionReceiptV1 {
                vault_status_checked: false,
                provider_cache_loaded: None,
                identity_proof: "read-only Vault status was unavailable; effective provider/model/version remain unproven".to_string(),
            };
        }
    };
    let parsed: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return ProviderResolutionReceiptV1 {
                vault_status_checked: false,
                provider_cache_loaded: None,
                identity_proof: "Vault status did not return parseable JSON; effective provider/model/version remain unproven".to_string(),
            };
        }
    };
    ProviderResolutionReceiptV1 {
        vault_status_checked: true,
        provider_cache_loaded: parsed
            .get("provider_cache")
            .and_then(|cache| cache.get("loaded"))
            .and_then(Value::as_bool),
        identity_proof: "Vault status can attest cache readiness only; this read-only adapter made no model invocation, so effective provider/model/version remain unproven".to_string(),
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

fn run(args: Args) -> Result<CorpusPilotReportV1, String> {
    let input = std::fs::read(&args.manifest)
        .map_err(|error| format!("read manifest {}: {error}", args.manifest.display()))?;
    let report = run_corpus_pilot(
        &input,
        &GhCliReader,
        &args.captured_at,
        preview_only_engine_receipt(),
        provider_resolution_receipt(),
    )?;
    write_report(&args.report, &report)?;
    Ok(report)
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(report) => {
            println!(
                "#1059 pilot: disposition={} candidates={} manifest_sha256={} report contains no credential values",
                report.disposition, report.candidates_emitted, report.manifest_sha256
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("github-corpus-pilot: {error}");
            ExitCode::from(1)
        }
    }
}

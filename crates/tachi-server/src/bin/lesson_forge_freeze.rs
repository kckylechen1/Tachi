use std::path::PathBuf;

use clap::Parser;
use tachi_lesson_forge::selection::{select_and_freeze_real_manifest_v1, PilotSelectionConfigV1};

#[derive(Debug, Parser)]
#[command(name = "lesson-forge-freeze")]
#[command(about = "Freeze a privacy-screened #1073 pilot manifest without model calls")]
struct Args {
    #[arg(long)]
    antigravity_db: PathBuf,
    #[arg(long)]
    hapi_db: PathBuf,
    #[arg(long)]
    capture_timestamp: String,
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    receipt: PathBuf,
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let config = PilotSelectionConfigV1 {
        antigravity_db: args.antigravity_db,
        hapi_db: args.hapi_db,
        capture_timestamp: args.capture_timestamp,
        manifest_path: args.manifest,
        receipt_path: args.receipt,
    };
    match select_and_freeze_real_manifest_v1(&config) {
        Ok(receipt) => {
            println!("manifest_contract_sha256={}", receipt.contract_digest);
            println!("rows={}", receipt.rows);
            println!(
                "sources=antigravity:{},hapi:{}",
                receipt.by_source[0], receipt.by_source[1]
            );
            println!(
                "kinds=narrative:{},structured_control:{}",
                receipt.by_kind[0], receipt.by_kind[1]
            );
            println!(
                "strata=correction_alignment:{},verification_recovery:{},routing_store_provenance:{}",
                receipt.by_stratum[0], receipt.by_stratum[1], receipt.by_stratum[2]
            );
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("phase-2a selection refused: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

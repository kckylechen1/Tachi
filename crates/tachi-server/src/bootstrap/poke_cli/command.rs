use serde_json::Value;
use std::path::Path;
use tachi_bootstrap::cli::PokeAction;

use super::super::print_pretty_json;
use super::report::print_poke_summary;
use super::suite::run_poke_smoke_suite;

pub(in crate::bootstrap) async fn run_poke_command(
    app_home: &Path,
    action: PokeAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        PokeAction::Run { suite, json } => {
            if !suite.eq_ignore_ascii_case("smoke") {
                return Err(format!("unsupported Poke suite '{suite}' (expected smoke)").into());
            }
            let report = run_poke_smoke_suite(app_home).await?;
            let status = report.get("status").and_then(Value::as_str);
            if json {
                print_pretty_json(&report)?;
            } else {
                print_poke_summary(&report);
            }
            if status != Some("passed") {
                let run_dir = report
                    .get("run_dir")
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)");
                return Err(format!("Poke smoke suite failed; see {run_dir}").into());
            }
            Ok(())
        }
    }
}

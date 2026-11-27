use serde_json::Value;
use std::path::Path;
use tachi_bootstrap::cli::PokeAction;

use super::super::print_pretty_json;
use super::env::PokeEnvGuard;
use super::report::print_poke_summary;
use super::suite::PokeSandbox;

/// Runs on the startup thread, before the daemon's runtime exists.
pub(in crate::bootstrap) fn run_poke_command(
    app_home: &Path,
    action: &PokeAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        PokeAction::Run { suite, json } => {
            if !suite.eq_ignore_ascii_case("smoke") {
                return Err(format!("unsupported Poke suite '{suite}' (expected smoke)").into());
            }
            // The suite swaps the process environment for its sandbox. That
            // happens here, while no runtime worker exists. The runtime that
            // runs the probes is built afterwards and is dropped before the
            // environment is restored.
            let sandbox = PokeSandbox::create(app_home)?;
            let _env = PokeEnvGuard::new(&sandbox.tachi_home, &sandbox.run_root);
            let report = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(sandbox.run())?;
            let status = report.get("status").and_then(Value::as_str);
            if *json {
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

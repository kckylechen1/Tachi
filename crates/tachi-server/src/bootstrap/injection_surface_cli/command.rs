use std::path::PathBuf;

use tachi_bootstrap::cli::InjectionSurfaceAction;

use super::print::print_report;
use super::report::build_report;

pub(in crate::bootstrap) async fn run_injection_surface_command(
    action: InjectionSurfaceAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        InjectionSurfaceAction::Doctor {
            registry,
            home,
            json,
        } => {
            let home_path: Option<PathBuf> = match home {
                Some(p) => Some(crate::utils::resolve_home_arg(Some(p))?),
                None => None,
            };
            let report = build_report(&registry, home_path.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_report(&report);
            }
            Ok(())
        }
    }
}

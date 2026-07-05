//! CLI contract and startup helpers for the Tachi server binary.

pub mod cli;

use clap::Parser;
use std::error::Error;

pub use cli::Cli;

pub fn parse_cli() -> Cli {
    Cli::parse()
}

pub type RunError = Box<dyn Error>;

pub fn run_cli_with<F, E>(run: F, exit_code_for_error: E)
where
    F: FnOnce(Cli) -> Result<(), RunError>,
    E: FnOnce(&(dyn Error + 'static)) -> Option<i32>,
{
    let cli = parse_cli();
    if let Err(error) = run(cli) {
        if let Some(code) = exit_code_for_error(error.as_ref()) {
            std::process::exit(code);
        }
        eprintln!("Fatal: {error}");
        std::process::exit(1);
    }
}

mod wt_clean;

use std::path::PathBuf;

use wt_clean::{OutputFormat, WtRemoveOptions};

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("error: {err}");
            2
        }
    };
    std::process::exit(code);
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_help();
        return Ok(());
    }

    let command = args.remove(0);
    match command.as_str() {
        "wt-remove" => run_wt_remove(args),
        _ => Err(format!("unknown command '{command}'. Expected: wt-remove")),
    }
}

fn run_wt_remove(args: Vec<String>) -> Result<(), String> {
    let mut force = false;
    let mut json = false;
    let mut path: Option<PathBuf> = None;

    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            "--dry-run" => force = false,
            "--json" => json = true,
            "-h" | "--help" => {
                print_wt_remove_help();
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option '{arg}'")),
            _ => {
                if path.replace(PathBuf::from(&arg)).is_some() {
                    return Err("wt-remove accepts exactly one path".to_string());
                }
            }
        }
    }

    let path = path.ok_or_else(|| "wt-remove requires a path".to_string())?;
    let options = WtRemoveOptions {
        path,
        force,
        output: if json {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        },
    };
    wt_clean::run_wt_remove(options)
}

fn print_help() {
    println!(
        "tachi-clean\n\nUsage:\n  tachi-clean wt-remove <path> [--dry-run|--force] [--json]\n"
    );
}

fn print_wt_remove_help() {
    println!(
        "Remove a Tachi-managed git worktree safely.\n\nUsage:\n  tachi-clean wt-remove <path> [--dry-run|--force] [--json]\n\nOptions:\n  --dry-run  Preview only (default)\n  --force    Actually remove after safety checks\n  --json     Print machine-readable JSON\n"
    );
}

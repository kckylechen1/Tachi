mod registry;
mod target_clean;
mod wt_clean;

use std::path::PathBuf;

use registry::{RegisterOptions, RegisterOutputFormat};
use target_clean::TargetCleanOptions;
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
        "target" | "target-clean" => run_target_clean(args),
        "wt-register" => run_wt_register(args),
        "wt-remove" => run_wt_remove(args),
        _ => Err(format!(
            "unknown command '{command}'. Expected: target, wt-register, or wt-remove"
        )),
    }
}

fn run_target_clean(args: Vec<String>) -> Result<(), String> {
    let mut force = false;
    let mut json = false;
    let mut path: Option<PathBuf> = None;

    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            "--dry-run" => force = false,
            "--json" => json = true,
            "-h" | "--help" => {
                print_target_help();
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option '{arg}'")),
            _ => {
                if path.replace(PathBuf::from(&arg)).is_some() {
                    return Err("target accepts at most one path".to_string());
                }
            }
        }
    }

    target_clean::run_target_clean(TargetCleanOptions {
        path: path.unwrap_or_else(|| PathBuf::from(".")),
        force,
        output: if json {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        },
    })
}

fn run_wt_register(args: Vec<String>) -> Result<(), String> {
    let mut json = false;
    let mut path: Option<PathBuf> = None;
    let mut repo_root: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    let mut dispatch_id: Option<String> = None;
    let mut pr: Option<String> = None;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--repo" => {
                repo_root = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--repo requires a value".to_string())?,
                ));
            }
            "--branch" => {
                branch = Some(
                    iter.next()
                        .ok_or_else(|| "--branch requires a value".to_string())?,
                );
            }
            "--dispatch-id" => {
                dispatch_id = Some(
                    iter.next()
                        .ok_or_else(|| "--dispatch-id requires a value".to_string())?,
                );
            }
            "--pr" => {
                pr = Some(
                    iter.next()
                        .ok_or_else(|| "--pr requires a value".to_string())?,
                );
            }
            "-h" | "--help" => {
                print_wt_register_help();
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option '{arg}'")),
            _ => {
                if path.replace(PathBuf::from(&arg)).is_some() {
                    return Err("wt-register accepts exactly one path".to_string());
                }
            }
        }
    }

    registry::run_wt_register(RegisterOptions {
        path: path.ok_or_else(|| "wt-register requires a path".to_string())?,
        repo_root: repo_root.ok_or_else(|| "wt-register requires --repo".to_string())?,
        branch: branch.ok_or_else(|| "wt-register requires --branch".to_string())?,
        dispatch_id,
        pr,
        output: if json {
            RegisterOutputFormat::Json
        } else {
            RegisterOutputFormat::Text
        },
    })
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
        "tachi-clean\n\nUsage:\n  tachi-clean target [path] [--dry-run|--force] [--json]\n  tachi-clean wt-register <path> --repo <repo-root> --branch <branch> [--dispatch-id <id>] [--pr <number>] [--json]\n  tachi-clean wt-remove <path> [--dry-run|--force] [--json]\n"
    );
}

fn print_target_help() {
    println!(
        "Clean Cargo target build artifacts while keeping top-level release outputs.\n\nUsage:\n  tachi-clean target [path] [--dry-run|--force] [--json]\n\nOptions:\n  --dry-run  Preview only (default)\n  --force    Remove target/debug and release build subdirectories\n  --json     Print machine-readable JSON\n"
    );
}

fn print_wt_register_help() {
    println!(
        "Register a Tachi-managed git worktree.\n\nUsage:\n  tachi-clean wt-register <path> --repo <repo-root> --branch <branch> [--dispatch-id <id>] [--pr <number>] [--json]\n\nOptions:\n  --repo <path>       Repository root/main worktree\n  --branch <name>     Worktree branch name\n  --dispatch-id <id>  Optional dispatch identifier\n  --pr <number>       Optional pull request number\n  --json              Print machine-readable JSON\n"
    );
}

fn print_wt_remove_help() {
    println!(
        "Remove a Tachi-managed git worktree safely.\n\nUsage:\n  tachi-clean wt-remove <path> [--dry-run|--force] [--json]\n\nOptions:\n  --dry-run  Preview only (default)\n  --force    Actually remove after safety checks\n  --json     Print machine-readable JSON\n"
    );
}

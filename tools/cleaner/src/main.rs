mod registry;
mod sweep;
mod tachi_clean;
mod target_clean;
mod wt_clean;
mod wt_open;

use std::path::PathBuf;

use registry::{RegisterOptions, RegisterOutputFormat};
use sweep::{SweepOptions, DEFAULT_SWEEP_MAX_AGE_DAYS};
use tachi_clean::TachiCleanOptions;
use target_clean::TargetCleanOptions;
use wt_clean::{OutputFormat, WtRemoveOptions};
use wt_open::OpenOptions;

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
        "sweep" | "wt-sweep" => run_sweep(args),
        "tachi" | "tachi-clean" => run_tachi_clean(args),
        "target" | "target-clean" => run_target_clean(args),
        "wt-register" => run_wt_register(args),
        "wt-remove" => run_wt_remove(args),
        "wt-open" => run_wt_open(args),
        "wt-list" => run_wt_list(args),
        _ => Err(format!(
            "unknown command '{command}'. Expected: sweep, tachi, target, wt-register, wt-remove, wt-open, or wt-list"
        )),
    }
}

fn run_sweep(args: Vec<String>) -> Result<(), String> {
    let mut force = false;
    let mut json = false;
    let mut max_age_days = DEFAULT_SWEEP_MAX_AGE_DAYS;
    let mut roots = Vec::new();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--force" => force = true,
            "--dry-run" => force = false,
            "--json" => json = true,
            "--root" => roots.push(PathBuf::from(
                iter.next()
                    .ok_or_else(|| "--root requires a value".to_string())?,
            )),
            "--max-age-days" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| "--max-age-days requires a value".to_string())?;
                max_age_days = raw
                    .parse::<u64>()
                    .map_err(|_| "--max-age-days must be a positive integer".to_string())?;
            }
            "-h" | "--help" => {
                print_sweep_help();
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option '{arg}'")),
            _ => return Err("sweep accepts only options; use --root <path>".to_string()),
        }
    }

    sweep::run_sweep(SweepOptions {
        roots,
        max_age_days,
        force,
        output: if json {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        },
    })
}

fn run_tachi_clean(args: Vec<String>) -> Result<(), String> {
    let mut force = false;
    let mut json = false;
    let mut home: Option<PathBuf> = None;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--force" => force = true,
            "--dry-run" => force = false,
            "--json" => json = true,
            "--home" => {
                home = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--home requires a value".to_string())?,
                ));
            }
            "-h" | "--help" => {
                print_tachi_help();
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option '{arg}'")),
            _ => return Err("tachi accepts only options; use --home <path>".to_string()),
        }
    }

    tachi_clean::run_tachi_clean(TachiCleanOptions {
        home,
        force,
        output: if json {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        },
    })
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

fn run_wt_open(args: Vec<String>) -> Result<(), String> {
    let mut dry_run = false;
    let mut json = false;
    let mut repo_root: Option<PathBuf> = None;
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    let mut base: Option<String> = None;
    let mut task: Option<String> = None;
    let mut role: Option<String> = None;
    let mut dispatch_id: Option<String> = None;
    let mut name: Option<String> = None;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--json" => json = true,
            "--repo" => {
                repo_root = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--repo requires a value".to_string())?,
                ));
            }
            "--path" => {
                path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--path requires a value".to_string())?,
                ));
            }
            "--branch" => {
                branch = Some(
                    iter.next()
                        .ok_or_else(|| "--branch requires a value".to_string())?,
                );
            }
            "--base" => {
                base = Some(
                    iter.next()
                        .ok_or_else(|| "--base requires a value".to_string())?,
                );
            }
            "--task" => {
                task = Some(
                    iter.next()
                        .ok_or_else(|| "--task requires a value".to_string())?,
                );
            }
            "--role" => {
                role = Some(
                    iter.next()
                        .ok_or_else(|| "--role requires a value".to_string())?,
                );
            }
            "--dispatch-id" => {
                dispatch_id = Some(
                    iter.next()
                        .ok_or_else(|| "--dispatch-id requires a value".to_string())?,
                );
            }
            "--name" => {
                name = Some(
                    iter.next()
                        .ok_or_else(|| "--name requires a value".to_string())?,
                );
            }
            "-h" | "--help" => {
                print_wt_open_help();
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option '{arg}'")),
            _ => return Err(format!("unexpected argument '{arg}'")),
        }
    }

    wt_open::run_wt_open(OpenOptions {
        repo_root: repo_root.ok_or_else(|| "wt-open requires --repo".to_string())?,
        path,
        branch,
        base,
        task,
        role,
        dispatch_id,
        name,
        dry_run,
        output: if json {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        },
    })
}

fn run_wt_list(args: Vec<String>) -> Result<(), String> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                print_wt_list_help();
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option '{arg}'")),
            _ => return Err(format!("unexpected argument '{arg}'")),
        }
    }

    let listed = registry::list_registered_worktrees()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&listed)
                .map_err(|err| format!("serialize list: {err}"))?
        );
    } else if listed.is_empty() {
        println!("no registered Tachi-managed worktrees");
    } else {
        println!("tachi-clean wt-list ({} entries)", listed.len());
        for item in listed {
            let exists = if item.path_exists { "exists" } else { "missing" };
            println!(
                "  [{exists}] {}  branch={}  repo={}",
                item.path, item.branch, item.repo_root
            );
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "tachi-clean\n\nUsage:\n  tachi-clean sweep [--root <path>] [--max-age-days <days>] [--dry-run|--force] [--json]\n  tachi-clean tachi [--home <path>] [--dry-run|--force] [--json]\n  tachi-clean target [path] [--dry-run|--force] [--json]\n  tachi-clean wt-register <path> --repo <repo-root> --branch <branch> [--dispatch-id <id>] [--pr <number>] [--json]\n  tachi-clean wt-remove <path> [--dry-run|--force] [--json]\n  tachi-clean wt-open --repo <repo-root> [--branch <name>] [--base <ref>] [--task <id>] [--role <name>] [--path <path>] [--dry-run] [--json]\n  tachi-clean wt-list [--json]\n"
    );
}

fn print_wt_open_help() {
    println!(
        "Open a Tachi-managed git worktree outside Desktop/repo (#484).\n\nUsage:\n  tachi-clean wt-open --repo <repo-root> [--branch <name>] [--base <ref>] [--task <id>] [--role <name>] [--name <leaf>] [--path <path>] [--dispatch-id <id>] [--dry-run] [--json]\n\nDefault path: $TACHI_WORKTREES_ROOT/<repo-slug>/<task>-<role>-<id>\n  or ~/.cache/tachi/worktrees/<repo-slug>/...\nRefuses Desktop, iCloud, and paths inside the primary repo.\n"
    );
}

fn print_wt_list_help() {
    println!(
        "List registered Tachi-managed worktrees.\n\nUsage:\n  tachi-clean wt-list [--json]\n"
    );
}

fn print_sweep_help() {
    println!(
        "Sweep stale Tachi-managed worktrees with .tachi-worktree.json markers.\n\nUsage:\n  tachi-clean sweep [--root <path>] [--max-age-days <days>] [--dry-run|--force] [--json]\n\nOptions:\n  --root <path>          Root to scan (repeatable; default: TMPDIR, /private/tmp, temp_dir)\n  --max-age-days <days>  Minimum worktree age (default: 7)\n  --dry-run              Preview only (default)\n  --force                Remove candidates with git worktree remove --force\n  --json                 Print machine-readable JSON\n"
    );
}

fn print_tachi_help() {
    println!(
        "Clean Tachi self-maintenance artifacts.\n\nUsage:\n  tachi-clean tachi [--home <path>] [--dry-run|--force] [--json]\n\nOptions:\n  --home <path>  Tachi home (default: TACHI_HOME or ~/.tachi)\n  --dry-run      Preview only (default)\n  --force        Remove old logs, runs, Claude Code runs, and stale cleanup backups\n  --json         Print machine-readable JSON\n"
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

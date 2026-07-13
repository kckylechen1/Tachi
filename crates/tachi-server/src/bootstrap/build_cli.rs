//! `tachi build` — the CLI surface of the build broker (#894 S2c).
//!
//! This is the one way work reaches the machine's single serialized executor
//! seat: `submit` writes an immutable ticket, `run` drains the queue through the
//! seat, `status` shows who holds the slot, and `abandon` is the explicit
//! crash-recovery path (quarantine the dead build's target, then free the slot).
//!
//! ## The seat's layout
//!
//! Rooted at `$TACHI_BUILD_SEAT_ROOT` (default `~/.cache/tachi/build-seat/<repo-slug>`):
//!
//! ```text
//!   checkout/          one fixed clean checkout; every ticket is built here on
//!                      a detached HEAD at the ticket's source sha
//!   target-resident/   the main-lineage target dir
//!   target-scratch/    the one fork target dir
//! ```
//!
//! Two target dirs, not N: the seat reuses the resident target for anything on
//! one line of history (that is where incremental builds come from) and drops
//! forks into scratch, so a diverged tree can never write into the resident
//! target's fingerprints.

use std::path::{Path, PathBuf};

use memcore::MemoryStore;
use tachi_bootstrap::cli::BuildAction;

use crate::build_broker::{
    self,
    runner::ProcessBuildRunner,
    target::GitLineage,
    ticket::{BuildCommand, BuildTicket, SourceIdentity},
    ExecutorSeat,
};

pub(crate) async fn run_build_command(
    action: BuildAction,
) -> Result<(), Box<dyn std::error::Error>> {
    run_build_command_sync(action).map_err(|err| err.into())
}

fn run_build_command_sync(action: BuildAction) -> Result<(), String> {
    match action {
        BuildAction::Submit {
            repo,
            head,
            base,
            env_id,
            dispatch_id,
            ticket_id,
            command,
            json,
        } => {
            let repo_root = canonical(&repo)?;
            let head_sha = resolve_commit(&repo_root, &head)?;
            let base_sha = match &base {
                Some(base) => resolve_commit(&repo_root, base)?,
                None => head_sha.clone(),
            };
            let command = parse_command(command);
            let ticket = BuildTicket::new(
                ticket_id.unwrap_or_else(|| format!("bt-{}", uuid::Uuid::new_v4().simple())),
                SourceIdentity {
                    repo_root: repo_root.display().to_string(),
                    base_sha,
                    head_sha,
                },
                command,
                env_id,
                dispatch_id,
            )?;
            let store = open_global_store()?;
            build_broker::ticket::submit_ticket(&store, &ticket)?;

            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "action": "build.submit",
                        "ticket_id": ticket.ticket_id,
                        "head_sha": ticket.source.head_sha,
                        "queued": build_broker::pending_tickets(&store)?.len(),
                    })
                );
            } else {
                println!("submitted build ticket {}", ticket.ticket_id);
                println!("  source:  {}", ticket.source.head_sha);
                println!(
                    "  command: {} {}",
                    ticket.command.program,
                    ticket.command.args.join(" ")
                );
                println!(
                    "  queued:  {} ticket(s) pending",
                    build_broker::pending_tickets(&store)?.len()
                );
            }
            Ok(())
        }

        BuildAction::Run { repo, max, json } => {
            let repo_root = canonical(&repo)?;
            let seat = resolve_seat(&repo_root)?;
            ensure_seat_checkout(&repo_root, &seat)?;
            let mut store = open_global_store()?;
            let runner = ProcessBuildRunner;
            let lineage = GitLineage;

            let limit = max.unwrap_or(usize::MAX);
            let mut receipts = Vec::new();
            for _ in 0..limit {
                match build_broker::run_next(&mut store, &seat, &runner, &lineage)? {
                    Some(receipt) => receipts.push(receipt),
                    None => break,
                }
            }

            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "action": "build.run",
                        "ran": receipts.len(),
                        "receipts": receipts,
                        "pending": build_broker::pending_tickets(&store)?.len(),
                    }))
                    .map_err(|e| e.to_string())?
                );
            } else if receipts.is_empty() {
                match build_broker::slot::current_holder(&store)? {
                    Some(holder) => println!(
                        "nothing ran: the executor slot is held by ticket {} (pid {})",
                        holder.ticket_id, holder.pid
                    ),
                    None => println!("nothing to build (queue empty)"),
                }
            } else {
                for receipt in &receipts {
                    println!(
                        "{}: {:?} (exit {:?}) on {} target {}{}",
                        receipt.ticket_id,
                        receipt.outcome,
                        receipt.exit_code,
                        receipt.target_slot,
                        receipt.target_path,
                        if receipt.cleared_target {
                            " [cleared first]"
                        } else {
                            ""
                        }
                    );
                    if let Some(target) = &receipt.quarantined_target {
                        println!("  !! interrupted; target QUARANTINED: {target}");
                        println!("     the next build must clear it before reuse");
                    }
                }
            }
            Ok(())
        }

        BuildAction::Status { repo, json } => {
            let repo_root = canonical(&repo)?;
            let seat = resolve_seat(&repo_root)?;
            let store = open_global_store()?;
            let holder = build_broker::slot::current_holder(&store)?;
            let pending = build_broker::pending_tickets(&store)?;
            let resident = build_broker::target::read_generation(
                &store,
                &seat.resident_target.display().to_string(),
            )?;
            let scratch = build_broker::target::read_generation(
                &store,
                &seat.scratch_target.display().to_string(),
            )?;

            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "action": "build.status",
                        "seat": {
                            "checkout": seat.checkout.display().to_string(),
                            "resident_target": seat.resident_target.display().to_string(),
                            "scratch_target": seat.scratch_target.display().to_string(),
                        },
                        "holder": holder,
                        "pending": pending.iter().map(|t| &t.ticket_id).collect::<Vec<_>>(),
                        "resident_generation": resident,
                        "scratch_generation": scratch,
                    }))
                    .map_err(|e| e.to_string())?
                );
            } else {
                match &holder {
                    Some(h) => println!(
                        "executor slot: HELD by {} (pid {}, since {})",
                        h.ticket_id, h.pid, h.acquired_at
                    ),
                    None => println!("executor slot: free"),
                }
                println!("pending: {} ticket(s)", pending.len());
                for t in &pending {
                    println!("  {} -> {}", t.ticket_id, t.source.head_sha);
                }
                print_generation("resident", &seat.resident_target, resident.as_ref());
                print_generation("scratch", &seat.scratch_target, scratch.as_ref());
            }
            Ok(())
        }

        BuildAction::Abandon { reason, json } => {
            let mut store = open_global_store()?;
            let abandoned = build_broker::abandon_stale_slot(&mut store, &reason)?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "action": "build.abandon", "abandoned": abandoned })
                );
            } else if abandoned {
                println!(
                    "executor slot forced open; the abandoned build's target dir is QUARANTINED \
                     and must be cleared before any build reuses it"
                );
            } else {
                println!("executor slot was already free; nothing to abandon");
            }
            Ok(())
        }
    }
}

fn print_generation(
    label: &str,
    path: &Path,
    generation: Option<&build_broker::target::TargetGeneration>,
) {
    match generation {
        Some(gen) => println!(
            "{label} target ({}): last driven by {} (ticket {})",
            path.display(),
            gen.head_sha,
            gen.ticket_id
        ),
        None => println!("{label} target ({}): virgin", path.display()),
    }
}

/// Default `cargo build --workspace` when the caller passed no command.
fn parse_command(raw: Vec<String>) -> BuildCommand {
    if raw.is_empty() {
        return BuildCommand {
            program: "cargo".to_string(),
            args: vec!["build".to_string(), "--workspace".to_string()],
            features: vec![],
        };
    }
    let mut iter = raw.into_iter();
    let program = iter.next().unwrap_or_else(|| "cargo".to_string());
    let args: Vec<String> = iter.collect();
    let features = args
        .iter()
        .skip_while(|a| a.as_str() != "--features")
        .nth(1)
        .map(|f| f.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    BuildCommand {
        program,
        args,
        features,
    }
}

fn open_global_store() -> Result<MemoryStore, String> {
    let global_db = crate::path_utils::tachi_home()
        .join("global")
        .join("memory.db");
    if let Some(parent) = global_db.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let db_str = global_db
        .to_str()
        .ok_or("global db path is not valid UTF-8")?;
    MemoryStore::open_with_label(db_str, "global").map_err(|e| {
        format!("build broker needs the global store (it owns the executor slot): {e}")
    })
}

/// Where this machine's executor seat lives for a given repo.
fn resolve_seat(repo_root: &Path) -> Result<ExecutorSeat, String> {
    let root = match std::env::var_os("TACHI_BUILD_SEAT_ROOT") {
        Some(raw) if !raw.is_empty() => {
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(
                    "TACHI_BUILD_SEAT_ROOT must be an absolute path: a relative seat root would \
                     float with the caller's cwd, and the seat is a machine-level singleton"
                        .to_string(),
                );
            }
            path
        }
        _ => {
            let home =
                dirs::home_dir().ok_or("cannot determine HOME for the default build-seat root")?;
            home.join(".cache")
                .join("tachi")
                .join("build-seat")
                .join(tachi_clean::wt_open::repo_slug(repo_root))
        }
    };
    Ok(ExecutorSeat {
        checkout: root.join("checkout"),
        resident_target: root.join("target-resident"),
        scratch_target: root.join("target-scratch"),
    })
}

/// Create the seat's fixed checkout on first use (a detached linked worktree of
/// the repo). Never touches it once it exists — `prepare_checkout` refuses to
/// check out over a dirty seat, so an operator poking around in there gets a
/// loud failure rather than having their work clobbered.
fn ensure_seat_checkout(repo_root: &Path, seat: &ExecutorSeat) -> Result<(), String> {
    if seat.checkout.exists() {
        return Ok(());
    }
    if let Some(parent) = seat.checkout.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("worktree")
        .arg("add")
        .arg("--detach")
        .arg(&seat.checkout)
        .arg("HEAD")
        .status()
        .map_err(|e| format!("git worktree add (build seat): {e}"))?;
    if !status.success() {
        return Err(format!(
            "could not create the executor seat checkout at {} ({status}); create it by hand \
             with: git -C {} worktree add --detach {} HEAD",
            seat.checkout.display(),
            repo_root.display(),
            seat.checkout.display()
        ));
    }
    Ok(())
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|e| format!("resolve {}: {e}", path.display()))
}

/// Resolve a ref to its commit object id. The ticket only ever carries object
/// ids — a ref would let the tree the seat builds drift out from under a ticket
/// that has already been planned against a target generation.
fn resolve_commit(repo_root: &Path, reference: &str) -> Result<String, String> {
    if reference.starts_with('-') {
        return Err(format!(
            "refusing to resolve '{reference}': a ref starting with '-' is an argument-injection \
             surface"
        ));
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg("--verify")
        .arg("--quiet")
        .arg(format!("{reference}^{{commit}}"))
        .output()
        .map_err(|e| format!("git rev-parse: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "'{reference}' is not a commit in {}",
            repo_root.display()
        ));
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(format!("git rev-parse returned nothing for '{reference}'"));
    }
    Ok(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ref_that_looks_like_a_flag_is_refused() {
        // `git rev-parse --upload-pack=...` style injection: refused before git
        // ever sees it.
        let err = resolve_commit(Path::new("/repo"), "--upload-pack=touch /tmp/pwn").unwrap_err();
        assert!(err.contains("injection"), "got: {err}");
    }

    #[test]
    fn an_empty_command_defaults_to_a_workspace_build() {
        let cmd = parse_command(vec![]);
        assert_eq!(cmd.program, "cargo");
        assert_eq!(cmd.args, vec!["build", "--workspace"]);
    }

    #[test]
    fn features_are_lifted_out_of_the_arg_vector() {
        let cmd = parse_command(
            ["cargo", "test", "--features", "admin,vec"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        assert_eq!(cmd.program, "cargo");
        assert_eq!(cmd.features, vec!["admin", "vec"]);
    }

    #[test]
    fn the_seat_root_must_be_absolute() {
        std::env::set_var("TACHI_BUILD_SEAT_ROOT", "relative/seat");
        let err = resolve_seat(Path::new("/repo")).unwrap_err();
        std::env::remove_var("TACHI_BUILD_SEAT_ROOT");
        assert!(err.contains("absolute"), "got: {err}");
    }
}

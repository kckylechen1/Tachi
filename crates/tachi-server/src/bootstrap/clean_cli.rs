use std::path::PathBuf;
use tachi_bootstrap::cli::{CleanAction, WorktreeAction};
use tachi_clean::sweep::SweepOptions;
use tachi_clean::tachi_clean::TachiCleanOptions;
use tachi_clean::target_clean::TargetCleanOptions;
use tachi_clean::wt_clean::{OutputFormat, WtRemoveOptions};
use tachi_clean::wt_open::{self, OpenOptions};

pub(crate) async fn run_clean_command(
    action: CleanAction,
) -> Result<(), Box<dyn std::error::Error>> {
    run_clean_command_sync(action).map_err(|err| err.into())
}

pub(crate) async fn run_worktree_command(
    action: WorktreeAction,
) -> Result<(), Box<dyn std::error::Error>> {
    run_worktree_command_sync(action).map_err(|err| err.into())
}

fn run_worktree_command_sync(action: WorktreeAction) -> Result<(), String> {
    match action {
        WorktreeAction::Open {
            repo,
            path,
            branch,
            base,
            task,
            role,
            dispatch_id,
            name,
            dry_run,
            json,
        } => provision_managed_env_cli(
            crate::exec_env_ops::ProvisionEnvOptions {
                repo_root: repo,
                path,
                branch,
                base,
                task,
                role,
                dispatch_id,
                name,
                dry_run,
            },
            output_format(json),
        ),
        WorktreeAction::Close {
            path,
            force,
            dry_run: _,
            json,
        } => tachi_clean::wt_clean::run_wt_remove(WtRemoveOptions {
            path,
            force,
            output: output_format(json),
        }),
        WorktreeAction::List { json } => {
            let listed = tachi_clean::registry::list_registered_worktrees()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&listed)
                        .map_err(|err| format!("serialize list: {err}"))?
                );
            } else if listed.is_empty() {
                println!("no registered Tachi-managed worktrees");
            } else {
                println!("tachi worktree list ({} entries)", listed.len());
                for item in listed {
                    let exists = if item.path_exists {
                        "exists"
                    } else {
                        "missing"
                    };
                    println!(
                        "  [{exists}] {}  branch={}  repo={}",
                        item.path, item.branch, item.repo_root
                    );
                }
            }
            Ok(())
        }
    }
}

fn output_format(json: bool) -> OutputFormat {
    if json {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    }
}

/// CLI wrapper over the single provisioning entrypoint (#894 S1): opens the
/// managed worktree AND records a daemon-owned `exec_envs` lease through
/// `exec_env_ops::provision_managed_env`, so the CLI and the daemon share one
/// source of provisioning logic instead of a divergent copy.
///
/// If the global store cannot be opened (e.g. no `TACHI_HOME` in this context),
/// falls back to the lease-less worktree open so the CLI never regresses.
fn provision_managed_env_cli(
    opts: crate::exec_env_ops::ProvisionEnvOptions,
    output: OutputFormat,
) -> Result<(), String> {
    let global_db = crate::path_utils::tachi_home()
        .join("global")
        .join("memory.db");
    if let Some(parent) = global_db.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let db_str = global_db
        .to_str()
        .ok_or("global db path is not valid UTF-8")?;

    match memcore::MemoryStore::open_with_label(db_str, "global") {
        Ok(store) => {
            let provisioned =
                crate::exec_env_ops::provision_managed_env(store.connection(), &opts)?;
            wt_open::emit_open_report(&provisioned.report, output)?;
            // Surface the lease id (#894 S1) so callers learn what to pass as
            // `env_id` on a later dispatch. `emit_open_report` only knows the
            // vendor-neutral `OpenReport` (no lease concept), so the lease id
            // is reported here rather than threaded into that shared struct.
            match &provisioned.env_id {
                Some(env_id) => {
                    if matches!(output, OutputFormat::Text) {
                        println!("  env_id: {env_id}");
                    }
                    tracing::info!(env_id = %env_id, "provisioned exec_env lease");
                }
                None => {
                    tracing::debug!(
                        "worktree provisioned without an exec_env lease (see report warnings)"
                    );
                }
            }
            if provisioned.report.errors.is_empty() {
                Ok(())
            } else {
                Err(provisioned.report.errors.join("; "))
            }
        }
        Err(err) => {
            // Fail-open on the lease record only: still provision the worktree
            // (lease-less) so the CLI stays usable without a daemon DB.
            tracing::warn!(
                error = %err,
                "exec_env lease store unavailable; opening worktree without a lease"
            );
            wt_open::run_wt_open_with_emit(OpenOptions {
                repo_root: opts.repo_root,
                path: opts.path,
                branch: opts.branch,
                base: opts.base,
                task: opts.task,
                role: opts.role,
                dispatch_id: opts.dispatch_id,
                name: opts.name,
                dry_run: opts.dry_run,
                output,
            })
        }
    }
}

fn run_clean_command_sync(action: CleanAction) -> Result<(), String> {
    match action {
        CleanAction::Target {
            path,
            force,
            dry_run: _,
            json,
        } => tachi_clean::target_clean::run_target_clean(TargetCleanOptions {
            path: path.unwrap_or_else(|| PathBuf::from(".")),
            force,
            output: output_format(json),
        }),
        CleanAction::Worktree {
            path,
            force,
            dry_run: _,
            json,
        } => tachi_clean::wt_clean::run_wt_remove(WtRemoveOptions {
            path,
            force,
            output: output_format(json),
        }),
        CleanAction::Sweep {
            root,
            max_age_days,
            force,
            dry_run: _,
            json,
        } => tachi_clean::sweep::run_sweep(SweepOptions {
            roots: root,
            max_age_days,
            force,
            output: output_format(json),
        }),
        CleanAction::Tachi {
            home,
            force,
            dry_run: _,
            json,
        } => tachi_clean::tachi_clean::run_tachi_clean(TachiCleanOptions {
            home,
            force,
            output: output_format(json),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_target_defaults_to_dry_run_and_json_output() {
        let root = unique_temp_dir("tachi-clean-cli-target");
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/test-bin"), "debug").unwrap();

        run_clean_command_sync(CleanAction::Target {
            path: Some(root.clone()),
            force: false,
            dry_run: false,
            json: true,
        })
        .unwrap();

        assert!(root.join("target/debug/test-bin").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clean_target_force_removes_debug_artifacts() {
        let root = unique_temp_dir("tachi-clean-cli-target-force");
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/test-bin"), "debug").unwrap();

        run_clean_command_sync(CleanAction::Target {
            path: Some(root.clone()),
            force: true,
            dry_run: false,
            json: true,
        })
        .unwrap();

        assert!(!root.join("target/debug").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

use std::path::PathBuf;
use tachi_bootstrap::cli::{CleanAction, WorktreeAction};

use crate::exec_env_reaper::ReapOptions;
use tachi_clean::sweep::SweepOptions;
use tachi_clean::tachi_clean::TachiCleanOptions;
use tachi_clean::target_clean::TargetCleanOptions;
use tachi_clean::wt_clean::{OutputFormat, WtRemoveOptions};
use tachi_clean::wt_open::{self, CargoTargetPolicy, OpenOptions};

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
        // The variant is boxed (the flags are ~260 bytes and the other variants
        // are ~30); unbox once, here, so the rest is plain field moves.
        WorktreeAction::Open(args) => {
            let args = *args;
            let env_class = memcore::EnvClass::parse(&args.env_class).map_err(|e| e.to_string())?;
            // An approval is only ever constructed from an explicit flag — the
            // gate in `validate_provision_request` refuses `build-private`
            // without one, and `register_env_resources` refuses to allocate the
            // private target without one to BOOK.
            let reserved_bytes = args.reserve_bytes.unwrap_or(0);
            let private_target_approval = args.approve_private_target.map(|approved_by| {
                crate::exec_env_ops::PrivateTargetApproval {
                    approved_by,
                    reserved_bytes,
                }
            });
            provision_managed_env_cli(
                crate::exec_env_ops::ProvisionEnvOptions {
                    repo_root: args.repo,
                    path: args.path,
                    branch: args.branch,
                    base: args.base,
                    task: args.task,
                    role: args.role,
                    dispatch_id: args.dispatch_id,
                    name: args.name,
                    env_class,
                    private_target_approval,
                    private_target_dir: None,
                    dry_run: args.dry_run,
                },
                output_format(args.json),
            )
        }
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
        if let Err(error) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %error, path = %parent.display(), "failed to create global db parent directory");
        }
    }
    let db_str = global_db
        .to_str()
        .ok_or("global db path is not valid UTF-8")?;

    match memcore::MemoryStore::open_with_label(db_str, "global") {
        Ok(mut store) => {
            let provisioned =
                crate::exec_env_ops::provision_managed_env(store.connection_mut(), &opts)?;
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
            //
            // The class policy is NOT relaxed on this path: without a lease
            // there is nowhere to record a resource binding or book a disk
            // reservation, so the only classes that can be honored are the ones
            // that allocate no target dir. A `build-private` request here would
            // write a private target config for bytes nothing is tracking —
            // refuse it instead. (`edit-only` and `build-ticketed` allocate no
            // target of their own, so they are safe lease-less.)
            tracing::warn!(
                error = %err,
                "exec_env lease store unavailable; opening worktree without a lease"
            );
            if opts.env_class.allocates_build_target() {
                return Err(format!(
                    "env_class '{}' needs the exec_env lease store to record its build-target \
                     resource and book its disk reservation, and the global store could not be \
                     opened ({err}); re-run with the default 'edit-only' class or fix the store \
                     (#894 S2c)",
                    opts.env_class.as_str()
                ));
            }
            wt_open::run_wt_open_with_emit(OpenOptions {
                repo_root: opts.repo_root,
                path: opts.path,
                branch: opts.branch,
                base: opts.base,
                task: opts.task,
                role: opts.role,
                dispatch_id: opts.dispatch_id,
                name: opts.name,
                cargo_target: CargoTargetPolicy::Unallocated,
                dry_run: opts.dry_run,
                output,
            })
        }
    }
}

/// CLI wrapper for the stale-lease sweep backstop (#1029). Opens the global
/// store and, **gated on the same `force` flag as the worktree sweep**, either
/// previews (read-only) or reclaims any `active` exec_env lease whose worktree
/// is gone. Fail-open: an absent / unopenable global store is surfaced as a
/// one-line warning and skipped, never an error — the stale-lease sweep is a
/// best-effort backstop, not the caller's contract (#1029 D2).
///
/// `force == false` (preview / dry-run, the default) lists the stale leases and
/// reports the count a real sweep WOULD reclaim, but never mutates — preview
/// must not change state (#1029 D1). `force == true` runs the mutate path.
/// NOTE (#1029 review): opening the store here runs pending schema migrations
/// like every other read command in this binary (briefing, status, search).
/// That open-on-read behavior is systemic, not a sweep-specific mutation; the
/// destructive gate this function owns is lease reclaim, and that is strictly
/// `force`-gated below.
fn sweep_stale_exec_env_leases_cli(force: bool) {
    let global_db = crate::path_utils::tachi_home()
        .join("global")
        .join("memory.db");
    let db_str = match global_db.to_str() {
        Some(s) => s,
        None => {
            eprintln!(
                "exec_env lease sweep: warning: global db path is not valid UTF-8; backstop skipped"
            );
            tracing::debug!("global db path is not valid UTF-8; skipping stale-lease sweep");
            return;
        }
    };
    match memcore::MemoryStore::open_with_label(db_str, "global") {
        Ok(mut store) => {
            if !force {
                // Preview / dry-run: read-only point-count. List what a real
                // (`--force`) sweep would reclaim and report it, but do NOT
                // mutate — preview must not change state (#1029 D1).
                match list_stale_exec_env_leases(store.connection()) {
                    Ok(stale) => eprintln!(
                        "exec_env lease sweep (preview): would reclaim {} stale lease(s)",
                        stale.len()
                    ),
                    Err(err) => {
                        eprintln!(
                            "exec_env lease sweep: warning: listing stale leases failed: {err}"
                        );
                        tracing::warn!(error = %err, "stale exec_env lease listing failed");
                    }
                }
                return;
            }
            match sweep_stale_exec_env_leases(store.connection_mut()) {
                Ok(reclaimed) => {
                    eprintln!("exec_env lease sweep (force): reclaimed {reclaimed} stale lease(s)");
                    if reclaimed > 0 {
                        tracing::info!(reclaimed, "sweep reclaimed stale exec_env leases");
                    }
                }
                Err(err) => {
                    eprintln!("exec_env lease sweep: warning: reclaim failed: {err}");
                    tracing::warn!(error = %err, "stale exec_env lease sweep failed");
                }
            }
        }
        // #1029 D2: fail-open on a missing/unopenable global store so the
        // worktree sweep it rides along with never fails, but make the skipped
        // backstop observable with a one-line warning instead of a silent
        // `debug!` that made `clean sweep` look all-green while the backstop was
        // quietly not running.
        Err(err) => {
            eprintln!(
                "exec_env lease sweep: warning: lease store unavailable, backstop skipped ({err})"
            );
            tracing::warn!(
                error = %err,
                "exec_env lease store unavailable; skipping stale-lease sweep"
            );
        }
    }
}

/// Read-only: list `active` exec_env leases whose worktree path no longer exists
/// on disk (#1029 sweep backstop, preview half). Never mutates — the preview /
/// dry-run path calls this and reports the count WITHOUT reclaiming, preserving
/// the "preview does not change state" contract (#1029 D1).
///
/// Active leases whose path still exists are left out: only a crash / kill -9 /
/// any non-`safe_merge` exit leaves an `active` lease pointing at a worktree that
/// is already gone, and `find_active_exec_env_by_path` would otherwise hand that
/// stale lease to a later dispatch (#976).
fn list_stale_exec_env_leases(
    conn: &rusqlite::Connection,
) -> Result<Vec<memcore::ExecEnvLease>, String> {
    let active = memcore::list_exec_envs(conn, Some(memcore::ExecEnvState::Active))
        .map_err(|e| e.to_string())?;
    Ok(active
        .into_iter()
        .filter(|lease| !std::path::Path::new(&lease.path).exists())
        .collect())
}

/// Write: reclaim the given stale leases (#1029 sweep backstop, mutate half).
/// Returns the count reclaimed.
///
/// This is a *backstop*, never the owning transition: wiring `cancel` /
/// terminal-state through the reclaim path is a #894 S2 policy call and is out of
/// scope here.
///
/// Each stale lease is reclaimed **by env_id, not by path**: `exec_envs.path`
/// has no UNIQUE constraint and the path-based reclaim selector resolves a single
/// row (LIMIT 1), so a path carrying multiple `active` leases must be drained
/// row-by-row or the extra leases would leak.
fn reclaim_stale_exec_env_leases(
    conn: &mut rusqlite::Connection,
    stale: &[memcore::ExecEnvLease],
) -> Result<usize, String> {
    let mut reclaimed = 0usize;
    for lease in stale {
        // Re-check right before the write: the path may have reappeared
        // between the list (read) half and this reclaim (write) half — e.g. a
        // provision recreating the same leaf. A lease whose worktree exists
        // again is no longer stale and must not be reclaimed (TOCTOU guard).
        if std::path::Path::new(&lease.path).exists() {
            continue;
        }
        let outcome = memcore::reclaim_exec_env(
            conn,
            &memcore::ExecEnvSelector::EnvId(lease.env_id.clone()),
            Some("sweep_stale"),
        )
        .map_err(|e| e.to_string())?;
        if matches!(outcome, memcore::ReclaimOutcome::Reclaimed { .. }) {
            reclaimed += 1;
        }
    }
    Ok(reclaimed)
}

/// Mutate path = list (preview half) then reclaim (mutate half). Returns the
/// count reclaimed. Used by the `--force` CLI branch and the sweep tests; the
/// preview branch calls `list_stale_exec_env_leases` alone so it never reaches
/// the reclaim half (#1029 D1).
fn sweep_stale_exec_env_leases(conn: &mut rusqlite::Connection) -> Result<usize, String> {
    let stale = list_stale_exec_env_leases(conn)?;
    reclaim_stale_exec_env_leases(conn, &stale)
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
        } => {
            // Backstop reclaim of stale exec_env leases (#1029) rides along with
            // the age-based worktree sweep, gated on the SAME `force` flag: the
            // worktree sweep only removes with `--force` (preview otherwise), so
            // the lease backstop must too — without `--force` it previews
            // read-only and never reclaims (D1). Fail-open: a missing/unopenable
            // global store must never fail the worktree sweep it accompanies (D2).
            sweep_stale_exec_env_leases_cli(force);
            tachi_clean::sweep::run_sweep(SweepOptions {
                roots: root,
                max_age_days,
                force,
                output: output_format(json),
            })
        }
        CleanAction::Orphans {
            root,
            max_age_days,
            force,
            dry_run: _,
            json,
        } => run_orphan_reap_cli(
            ReapOptions {
                roots: if root.is_empty() {
                    crate::exec_env_reaper::default_orphan_roots()
                } else {
                    root
                },
                max_age_days,
                force,
            },
            output_format(json),
        ),
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

/// CLI wrapper for the orphan build-artifact reaper (#894 S2b).
///
/// # `--force` is CERTIFIED (#1062). This command can now delete.
///
/// Was `--force` is REFUSED, unconditionally, before this function opened the ledger,
/// stats'd a directory, or read the process table: an adversarial audit (`codex-g6f99`)
/// found the delete path unsafe, so [`crate::exec_env_reaper::certify_destructive`]
/// rejected every `--force` request at the gate and the CLI printed the reason and
/// exited non-zero. #1062 (owner-ratified 1A, 2026-07-17: the kill-test matrix was
/// executed and its receipt checked in) flipped `DESTRUCTIVE_CERTIFIED` to `true`, and
/// [`crate::exec_env_reaper::certify_destructive`] now returns `Ok(())` for `--force`
/// too — this function proceeds past the gate below, opens the real ledger, and (on a
/// healthy scan) genuinely reclaims. The gate call stays: the day certification is
/// REVOKED, this is the one place that starts refusing `force` again, and every caller
/// already asks it instead of reading the constant directly.
///
/// Without `--force` it does what it has always done: a full accounting of the dead
/// build artifacts on this machine — where they are, how big, how old, who (if anyone)
/// holds them, and everything the scan could not see.
///
/// An unopenable ledger is a hard error rather than a warning, because the byte report
/// is grouped out of that ledger and a report that silently loses half its history is
/// the off-the-books reclaim #1029 called out, in reverse.
///
/// Exit status comes from [`crate::exec_env_reaper::reap_exit_status`]: a run whose scan
/// left `incomplete-or-error` units (a named root that is not there, a subtree `read_dir`
/// could not open) or whose protected set could not be fully resolved (`ps` unavailable,
/// `HOME` unset) exits non-zero, because it cannot honestly say it saw its whole scope —
/// `--force` waives this no more than it waives any other fence (see
/// `cli_force_still_refuses_a_scan_that_cannot_see_its_whole_scope` below).
fn run_orphan_reap_cli(opts: ReapOptions, output: OutputFormat) -> Result<(), String> {
    // The protected set's sources are read from the process environment HERE — at the
    // edge, once — and handed to the reaper as a value. The reaper itself reads no
    // ambient state, which is what keeps "HOME is unset" a property of one run instead of
    // a property of the process (see `ProtectionSources`). This is the ONLY production
    // call site of `from_process_env` in this function's call graph; everything below
    // this line is `run_orphan_reap_cli_with_sources`, which a test may call with a
    // different, deterministic `ProtectionSources` instead (see
    // `cli_force_still_refuses_a_scan_that_cannot_see_its_whole_scope` and
    // `ProtectionSources::deterministic_for_cli_test`, #1196).
    let sources = crate::exec_env_reaper::ProtectionSources::from_process_env();
    run_orphan_reap_cli_with_sources(opts, output, &sources)
}

fn run_orphan_reap_cli_with_sources(
    opts: ReapOptions,
    output: OutputFormat,
    sources: &crate::exec_env_reaper::ProtectionSources<'_>,
) -> Result<(), String> {
    // The first gate, above everything: no ledger, no filesystem, no process table.
    // #1062: DESTRUCTIVE_CERTIFIED is true, so this is `Ok(())` for both `force`
    // values today — it stays the first thing asked so a future REVOCATION only
    // has to flip the constant, not re-wire every caller.
    if let Err(refusal) = crate::exec_env_reaper::certify_destructive(opts.force) {
        crate::exec_env_reaper::emit_destructive_refusal(&refusal, output)?;
        return Err(refusal.to_string());
    }

    let global_db = crate::path_utils::tachi_home()
        .join("global")
        .join("memory.db");
    if let Some(parent) = global_db.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %error, path = %parent.display(), "failed to create global db parent directory");
        }
    }
    let db_str = global_db
        .to_str()
        .ok_or("global db path is not valid UTF-8")?;
    let mut store = memcore::MemoryStore::open_with_label(db_str, "global")
        .map_err(|err| format!("exec_env resource ledger unavailable: {err}"))?;

    // #1062: `force` reaches here now (the gate above passes it). The `Err` arm is
    // still live — it is `certify_destructive`'s call inside `run_orphan_reap`
    // itself, the second of the two gates that "cannot be routed around" (see that
    // function's doc comment) — but on THIS build it never fires, for the same
    // reason the gate above didn't.
    let report = crate::exec_env_reaper::run_orphan_reap(
        store.connection_mut(),
        &opts,
        sources,
        std::time::SystemTime::now(),
        &crate::exec_env_reaper::lsof_holder_probe,
    )
    .map_err(|refusal| refusal.to_string())?;
    crate::exec_env_reaper::emit_reap_report(&report, output)?;
    // The report is emitted first, THEN the exit status is derived from it — an
    // incomplete run must still show the operator what it did see. `reap_exit_status`
    // is the single place that rule lives (sol's frozen accounting invariant: a run
    // that could not examine its whole authorized scope may not exit 0, `--force`
    // waives it no more than any other gate).
    crate::exec_env_reaper::reap_exit_status(&report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tachi clean orphans --force` still must not exit 0 on a scan that cannot see
    /// its whole authorized scope — `--force` waives that no more than it waives any
    /// other fence (#894 S2b's BUG 3 accounting invariant, `reap_exit_status`).
    ///
    /// Renamed from `orphan_reap_cli_refuses_force_with_a_nonzero_exit`, which pinned
    /// a DIFFERENT property: that `certify_destructive` refused every `--force`
    /// request outright, before this function ever opened the ledger, stat'd a
    /// directory, or read the process table (audit `codex-g6f99`) — a property that
    /// no longer holds. #1062 (owner-ratified 1A) flipped `DESTRUCTIVE_CERTIFIED` to
    /// `true`, so `certify_destructive(true)` now returns `Ok(())` and this function
    /// falls through the gate into the real ledger open and scan. See git blame /
    /// #1062 for the old reading.
    ///
    /// ## Why this could not be reconciled in place, and what changed instead
    ///
    /// Once `--force` falls through the gate, `run_orphan_reap_cli` opens
    /// `tachi_home()/global/memory.db` — the OPERATOR'S REAL global ledger — via
    /// `MemoryStore::open_with_label`'s fail-closed default (#1119:
    /// `OpenExisting` + `Deny`). On the machine this reconciliation was done on,
    /// that real DB is stamped at an OLDER schema than this binary's
    /// `EXPECTED_SCHEMA_VERSION` (v20, #1066/#1186 — landed on `main`, not
    /// something this branch introduced; confirmed no diff in
    /// `crates/memcore/src/db/migrations.rs` against `origin/main`), so the old
    /// fixture's `.expect_err(...)` still happened to hold — but the error was
    /// `SchemaMigrationOptInRequired`, not the certification refusal, and the old
    /// assertions on `"report-only"` / `"not certified"` / `"codex-g6f99"` would
    /// have failed. That outcome is a coincidence of THIS machine's ambient
    /// `~/.tachi` state, not a property of the code: a machine with no `~/.tachi`
    /// yet builds fresh at v20 and proceeds past the ledger open entirely; a
    /// machine already migrated to v20 does too. A test whose pass/fail depends on
    /// an operator's unrelated local DB state — while genuinely touching that real
    /// DB from a `cargo test` run — is exactly the hazard the NOTE below already
    /// called out for the report path, now true of the force path too.
    ///
    /// So this version isolates `TACHI_HOME` via [`crate::test_support::with_tachi_home`]
    /// (panic-safe, lock-guarded, the same helper three other call sites in this
    /// crate already share) before calling in: `tachi clean orphans --force` against
    /// a temp home has no ledger yet, opens fresh at the current schema with no
    /// migration decision to make, and reaches the scan — which is handed a root
    /// that does not exist, so `reap_exit_status` refuses on "scan incomplete", the
    /// one property this test can now prove hermetically and deterministically: a
    /// certified `--force` still does not exit 0 on a scan that cannot account for
    /// its whole scope.
    ///
    /// It calls `run_orphan_reap_cli_with_sources` — the exact same production body
    /// `run_orphan_reap_cli` runs — with `ProtectionSources::deterministic_for_cli_test()`
    /// rather than going through `run_orphan_reap_cli` (which reads the REAL process
    /// environment and shells out to the REAL `ps`). #1196: the real `ps -Awwo command=`
    /// scan sees every process on the machine, and a live build is not the only thing
    /// that can put the substring `CARGO_TARGET_DIR=` on a command line — a concurrent
    /// `grep`/`cat`/agent-shell invocation mentioning it does too, and the naive
    /// whitespace-tokenizing parser in `target_dirs_from_process_line` cannot tell them
    /// apart. When that noise is present the CLI's OTHER fail-closed gate (protected-set
    /// incomplete, BUG 3's sibling) fires first and starves the "scan incomplete" property
    /// this test exists to pin — a real assertion, just not the one this test names, and
    /// whether it happens to fire is a fact about the test MACHINE at the moment `cargo
    /// test` runs, not about this code. Injecting a deterministic, ambient-free
    /// `ProtectionSources` closes that gap the same way `exec_env_reaper`'s own module
    /// tests already do (see `ProtectionSources`'s doc comment on why sources are an
    /// explicit argument, never a read of ambient state) — this is that same seam,
    /// extended one call further out to reach the CLI entry point.
    #[test]
    fn cli_force_still_refuses_a_scan_that_cannot_see_its_whole_scope() {
        crate::test_support::with_tachi_home(|_home| {
            let sources = crate::exec_env_reaper::ProtectionSources::deterministic_for_cli_test();
            let err = run_orphan_reap_cli_with_sources(
                ReapOptions {
                    roots: vec![PathBuf::from(
                        "/tachi-reaper-this-root-must-never-be-scanned",
                    )],
                    max_age_days: 7,
                    force: true,
                },
                OutputFormat::Text,
                &sources,
            )
            .expect_err(
                "a scan that cannot see its whole authorized scope must not exit 0, even \
                 under --force, once certified",
            );

            assert!(
                err.contains("scan incomplete"),
                "the non-zero exit must carry the BUG 3 accounting refusal, not some \
                 incidental error: {err}"
            );
            assert!(
                err.contains("missing root"),
                "and it must name why the scan could not see its whole scope: {err}"
            );
        });
    }

    // NOTE: there is deliberately no CLI-level test of a *healthy* --force run (one
    // that reaches the delete path) here. It would need a real target dir under a
    // real scan root, and while `with_tachi_home` above isolates the LEDGER, it does
    // not sandbox the FILESYSTEM the CLI would then walk and delete from — a unit
    // test still has no business creating and deleting real directories through this
    // entry point when `exec_env_reaper`'s own tests can drive the identical
    // certified entry point (`run_orphan_reap`, via `reap_sealed`) against a fully
    // temp-dir'd store AND a temp-dir'd scan root. See
    // `force_reclaims_at_the_entry_point_and_a_broken_fence_still_refuses_it` in
    // that module for the genuinely-deletes proof, and its second half for the proof
    // that certification did not remove the other fences.

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

    fn open_lease_store(dir: &std::path::Path) -> memcore::MemoryStore {
        let db = dir.join("memory.db");
        memcore::MemoryStore::open(db.to_str().unwrap()).unwrap()
    }

    fn insert_active_lease(store: &memcore::MemoryStore, env_id: &str, path: &str) {
        memcore::insert_exec_env(
            store.connection(),
            &memcore::NewExecEnvLease {
                env_id: env_id.to_string(),
                kind: "worktree".to_string(),
                path: path.to_string(),
                repo_root: "/repo".to_string(),
                branch: "tachi/1029/w".to_string(),
                base_sha: "abc123".to_string(),
                dispatch_id: None,
                env_class: memcore::EnvClass::EditOnly,
                created_at: String::new(),
            },
        )
        .unwrap();
    }

    fn lease_state(store: &memcore::MemoryStore, env_id: &str) -> memcore::ExecEnvLease {
        memcore::get_exec_env(store.connection(), env_id)
            .unwrap()
            .expect("lease present")
    }

    #[test]
    fn sweep_reclaims_lease_whose_worktree_is_gone() {
        let dir = unique_temp_dir("tachi-clean-cli-lease-gone");
        let mut store = open_lease_store(&dir);
        // A path that does not exist on disk (the worktree was deleted / never
        // survived a crash) — the stale lease must be reclaimed.
        let gone = dir.join("does-not-exist-wt");
        insert_active_lease(&store, "env-gone", gone.to_str().unwrap());

        let n = sweep_stale_exec_env_leases(store.connection_mut()).unwrap();
        assert_eq!(n, 1);

        let lease = lease_state(&store, "env-gone");
        assert_eq!(lease.state, memcore::ExecEnvState::Reclaimed);
        assert_eq!(lease.reclaim_reason.as_deref(), Some("sweep_stale"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sweep_leaves_lease_whose_worktree_exists() {
        let dir = unique_temp_dir("tachi-clean-cli-lease-live");
        let mut store = open_lease_store(&dir);
        // A live worktree directory: its lease must NOT be touched — only
        // safe_merge / #894 S2 own that transition, never the sweep.
        let live = dir.join("live-wt");
        std::fs::create_dir_all(&live).unwrap();
        insert_active_lease(&store, "env-live", live.to_str().unwrap());

        let n = sweep_stale_exec_env_leases(store.connection_mut()).unwrap();
        assert_eq!(n, 0);

        let lease = lease_state(&store, "env-live");
        assert_eq!(lease.state, memcore::ExecEnvState::Active);
        assert_eq!(lease.reclaim_reason, None);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sweep_drains_all_active_leases_sharing_a_gone_path() {
        let dir = unique_temp_dir("tachi-clean-cli-lease-dup");
        let mut store = open_lease_store(&dir);
        // `exec_envs.path` has no UNIQUE constraint and the path-based reclaim
        // is LIMIT 1; two active leases on one gone path must BOTH be reclaimed,
        // which only holds because the sweep reclaims by env_id row-by-row.
        let gone = dir.join("shared-gone-wt");
        insert_active_lease(&store, "env-dup-a", gone.to_str().unwrap());
        insert_active_lease(&store, "env-dup-b", gone.to_str().unwrap());

        let n = sweep_stale_exec_env_leases(store.connection_mut()).unwrap();
        assert_eq!(n, 2);

        assert_eq!(
            lease_state(&store, "env-dup-a").state,
            memcore::ExecEnvState::Reclaimed
        );
        assert_eq!(
            lease_state(&store, "env-dup-b").state,
            memcore::ExecEnvState::Reclaimed
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_lists_stale_lease_without_reclaiming() {
        let dir = unique_temp_dir("tachi-clean-cli-lease-preview");
        let store = open_lease_store(&dir);
        // Active lease whose worktree path is gone: the preview / dry-run path
        // must SEE it (so it can report the count) but must NOT reclaim it —
        // preview does not change state (#1029 D1). This is the read-only half
        // the `!force` CLI branch calls.
        let gone = dir.join("does-not-exist-preview-wt");
        insert_active_lease(&store, "env-preview", gone.to_str().unwrap());

        let stale = list_stale_exec_env_leases(store.connection()).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].env_id, "env-preview");

        // The lease is untouched: still active, no reclaim reason stamped.
        let lease = lease_state(&store, "env-preview");
        assert_eq!(lease.state, memcore::ExecEnvState::Active);
        assert_eq!(lease.reclaim_reason, None);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn force_path_reclaims_after_preview_leaves_active() {
        let dir = unique_temp_dir("tachi-clean-cli-lease-gate");
        let mut store = open_lease_store(&dir);
        // End-to-end gating: the same stale lease is left active by the preview
        // half, then reclaimed by the force/mutate half (#1029 D1 gate flip).
        let gone = dir.join("does-not-exist-gate-wt");
        insert_active_lease(&store, "env-gate", gone.to_str().unwrap());

        // Preview (read-only) sees it but leaves it active.
        let stale = list_stale_exec_env_leases(store.connection()).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(
            lease_state(&store, "env-gate").state,
            memcore::ExecEnvState::Active
        );

        // Force/mutate path reclaims the same lease.
        let reclaimed = sweep_stale_exec_env_leases(store.connection_mut()).unwrap();
        assert_eq!(reclaimed, 1);
        let lease = lease_state(&store, "env-gate");
        assert_eq!(lease.state, memcore::ExecEnvState::Reclaimed);
        assert_eq!(lease.reclaim_reason.as_deref(), Some("sweep_stale"));

        let _ = std::fs::remove_dir_all(dir);
    }
}

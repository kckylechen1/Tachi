use std::path::PathBuf;

use serde_json::json;

use super::super::{atty_stdout, print_pretty_json};
use super::apply::execute_tidy_apply;
use super::migration::{build_migration_plan, execute_tidy_migrations, MigrationConfig};
use super::render::{
    render_migration_plan, render_tidy_apply_summary, render_tidy_execute_summary,
    render_tidy_report,
};
use super::report::build_tidy_report;

pub(in crate::bootstrap) async fn run_tidy_command(
    json_output: bool,
    apply: bool,
    dry_run: bool,
    execute: bool,
    yes: bool,
    target_db_override: Option<PathBuf>,
    home: &std::path::Path,
    app_home: &std::path::Path,
    roots: Vec<PathBuf>,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = build_tidy_report(&roots, git_root)?;

    // --execute: fragment-DB consolidation pipeline.
    if execute {
        let target_db = target_db_override
            .unwrap_or_else(|| app_home.join("global").join(memcore::MEMORY_DB_FILENAME));

        // Acquire and hold BOTH the scoped (daemon-<hash>.lock, matching
        // `target_db`) and legacy (daemon.lock) daemon locks for the entire
        // migration to prevent concurrent writes from a live daemon under
        // either naming scheme, which would corrupt the DB. Probing only
        // the legacy path (the old behavior) was invisible to a daemon
        // running under the current scoped-lock scheme.
        let _lock = match crate::daemon_lock::DualDaemonLock::acquire(app_home, &target_db) {
            Err(crate::daemon_lock::DualLockError::ScopedRunning { pid }) => {
                return Err(format!(
                    "refusing to run tachi tidy --execute while tachi daemon is running (pid {pid}, scoped lock for {}); stop it first",
                    target_db.display()
                )
                .into());
            }
            Err(crate::daemon_lock::DualLockError::LegacyRunning { pid }) => {
                return Err(format!(
                    "refusing to run tachi tidy --execute while tachi daemon is running (pid {pid}, legacy lock); stop it first"
                )
                .into());
            }
            Err(crate::daemon_lock::DualLockError::Io(e)) => {
                return Err(format!("daemon lock probe failed: {e}").into());
            }
            Ok(lock) => lock,
        };

        let archive_root = app_home
            .join("archive")
            .join(chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string());
        let plan = build_migration_plan(&report, &target_db, &archive_root, home);

        let cfg = MigrationConfig {
            target_db,
            manifest_path: crate::manifest::Manifest::default_path(home),
            dry_run: false,
            interactive: !yes && atty_stdout(),
            app_home: app_home.to_path_buf(),
        };

        let summary = execute_tidy_migrations(&plan, &cfg)?;
        drop(_lock); // explicitly release after migrations complete

        if json_output {
            return print_pretty_json(&json!({
                "report": report,
                "execute_summary": summary,
            }));
        }
        println!("{}", render_tidy_report(&report));
        println!();
        println!("{}", render_tidy_execute_summary(&summary));
        return Ok(());
    }

    // Legacy --apply path: conservative confirm-only summary.
    let apply_summary = if apply {
        Some(execute_tidy_apply(app_home, &report)?)
    } else {
        None
    };

    // --dry-run (or default): also include the migration plan preview when
    // there are any migration candidates, but make no writes.
    let target_db_preview = target_db_override
        .unwrap_or_else(|| app_home.join("global").join(memcore::MEMORY_DB_FILENAME));
    let archive_preview = app_home.join("archive").join("<timestamp>");
    let plan_preview = build_migration_plan(&report, &target_db_preview, &archive_preview, home);

    if json_output {
        if let Some(summary) = apply_summary.as_ref() {
            return print_pretty_json(&json!({
                "report": report,
                "apply_summary": summary,
                "migration_plan": plan_preview,
                "dry_run": dry_run || !apply,
            }));
        }
        return print_pretty_json(&json!({
            "report": report,
            "migration_plan": plan_preview,
            "dry_run": true,
        }));
    }

    println!("{}", render_tidy_report(&report));
    if !plan_preview.is_empty() {
        println!();
        println!("{}", render_migration_plan(&plan_preview));
    }
    if let Some(summary) = apply_summary.as_ref() {
        println!();
        println!("{}", render_tidy_apply_summary(summary));
    }
    if !execute {
        println!();
        println!(
            "(no writes performed — pass --execute to migrate, or --apply for the conservative path)"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fresh_home() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let app_home = home.join(".tachi");
        std::fs::create_dir_all(&app_home).unwrap();
        (dir, home, app_home)
    }

    #[tokio::test]
    async fn execute_refuses_when_scoped_lock_is_held() {
        let (_dir, home, app_home) = fresh_home();
        let target_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        let scoped_path = crate::daemon_lock::scoped_daemon_lock_path(&app_home, &target_db);
        let _holder = crate::daemon_lock::DaemonLock::acquire(&scoped_path)
            .expect("pre-acquire scoped lock to simulate a live daemon");

        let result =
            run_tidy_command(false, false, false, true, true, None, &home, &app_home, vec![], None)
                .await;

        let err = result.err().expect("must refuse while scoped lock is held");
        assert!(
            err.to_string().contains("scoped lock"),
            "error must name the scoped lock, got: {err}"
        );
    }

    #[tokio::test]
    async fn execute_refuses_when_legacy_lock_is_held() {
        let (_dir, home, app_home) = fresh_home();
        let legacy_path = crate::daemon_lock::legacy_daemon_lock_path(&app_home);
        let _holder = crate::daemon_lock::DaemonLock::acquire(&legacy_path)
            .expect("pre-acquire legacy lock to simulate an un-upgraded live daemon");

        let result =
            run_tidy_command(false, false, false, true, true, None, &home, &app_home, vec![], None)
                .await;

        let err = result.err().expect("must refuse while legacy lock is held");
        assert!(
            err.to_string().contains("legacy lock"),
            "error must name the legacy lock, got: {err}"
        );
    }

    #[tokio::test]
    async fn execute_proceeds_when_no_lock_is_held() {
        let (_dir, home, app_home) = fresh_home();

        let result =
            run_tidy_command(true, false, false, true, true, None, &home, &app_home, vec![], None)
                .await;

        assert!(
            result.is_ok(),
            "must proceed when neither lock is held: {result:?}"
        );
    }
}

//! R6 — `VACUUM INTO` then atomic swap. Requires daemon stopped.

use std::path::Path;

use rusqlite::Connection;
use serde_json::json;

use crate::daemon_lock::{DualDaemonLock, DualLockError};
use crate::manifest::Manifest;

use super::{backup_db, inventory::resolve_one};

pub async fn run_vacuum_cli(
    db: &str,
    apply: bool,
    app_home: &Path,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path = if app_home.join("manifest.json").exists() {
        app_home.join("manifest.json")
    } else {
        Manifest::default_path(&dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
    };
    let manifest = Manifest::load_or_empty(&manifest_path);
    let entry = match resolve_one(&manifest, db) {
        Some(e) => e,
        None => {
            return Err(format!("--db '{db}' did not resolve to exactly one manifest DB").into())
        }
    };
    crate::path_utils::manifest_db_leaf_exists(&entry)?;
    let path = std::path::PathBuf::from(&entry.path);

    if !apply {
        let body = json!({
            "db": entry.path,
            "apply": false,
            "would_do": "VACUUM INTO + atomic swap (per-DB backup taken first)",
        });
        if json_out {
            println!("{}", serde_json::to_string_pretty(&body)?);
        } else {
            println!("[!] DRY-RUN: would VACUUM INTO + swap {}", entry.path);
            println!("    Re-run with --apply (daemon must be stopped).");
        }
        return Ok(());
    }

    // Acquire BOTH the scoped (daemon-<hash>.lock, matching `path`) and
    // legacy (daemon.lock) daemon locks exclusively. Probing only the
    // legacy path (the old behavior) was invisible to a daemon running
    // under the current scoped-lock scheme, letting VACUUM race a live
    // daemon's writes.
    let _lock = match DualDaemonLock::acquire(app_home, &path) {
        Ok(l) => l,
        Err(DualLockError::ScopedRunning { pid }) => {
            return Err(format!(
                "cannot VACUUM {}: tachi daemon is running (pid {pid}, scoped lock). Stop the daemon first: `tachi daemon kill`",
                path.display()
            )
            .into());
        }
        Err(DualLockError::LegacyRunning { pid }) => {
            return Err(format!(
                "cannot VACUUM {}: tachi daemon is running (pid {pid}, legacy lock). Stop the daemon first: `tachi daemon kill`",
                path.display()
            )
            .into());
        }
        Err(DualLockError::Io(e)) => {
            return Err(format!("daemon lock probe failed for VACUUM: {e}").into());
        }
    };

    // Backup first.
    let backup = backup_db(&path)?;
    eprintln!("[OK] backed up {} -> {}", path.display(), backup.display());

    // VACUUM INTO temp file.
    let tmp = sibling(&path, "vacuumed.tmp");
    if tmp.exists() {
        std::fs::remove_file(&tmp)?;
    }
    {
        let conn = Connection::open(&path)?;
        let tmp_str = tmp.display().to_string();
        // SQLite expects a string literal; parameter binding for VACUUM INTO works.
        conn.execute("VACUUM INTO ?1", [&tmp_str])?;
    }

    // Atomic rename: tmp -> path. On unix, std::fs::rename is atomic on the same FS.
    std::fs::rename(&tmp, &path)?;

    let body = json!({
        "db": entry.path,
        "backup": backup.display().to_string(),
        "apply": true,
        "ok": true,
    });
    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!("[OK] VACUUM complete for {}", entry.path);
    }
    Ok(())
}

fn sibling(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(suffix);
    std::path::PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{DbEntry, DbRole, Manifest};
    use tempfile::tempdir;

    /// Fresh app_home + a manifest with exactly one resolvable "tachi" DB
    /// entry pointing at an on-disk file, so `resolve_one` succeeds and the
    /// lock-acquisition path (the only thing exercised before any real
    /// VACUUM I/O) is reached deterministically.
    fn fresh_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let app_home = dir.path().join(".tachi");
        std::fs::create_dir_all(&app_home).unwrap();
        let db_path = dir.path().join("project").join("memory.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, b"not-a-real-sqlite-file").unwrap();

        let mut manifest = Manifest::empty();
        manifest.dbs.push(DbEntry {
            path: db_path.display().to_string(),
            role: DbRole::Project,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "test-fixture".to_string(),
            notes: String::new(),
        });
        manifest.save(&app_home.join("manifest.json")).unwrap();

        (dir, app_home, db_path)
    }

    #[tokio::test]
    async fn vacuum_refuses_when_scoped_lock_is_held() {
        let (_dir, app_home, db_path) = fresh_fixture();
        let scoped_path = crate::daemon_lock::scoped_daemon_lock_path(&app_home, &db_path);
        let _holder = crate::daemon_lock::DaemonLock::acquire(&scoped_path)
            .expect("pre-acquire scoped lock to simulate a live daemon");

        let db_arg = db_path.display().to_string();
        let result = run_vacuum_cli(&db_arg, true, &app_home, false).await;

        let err = result.expect_err("must refuse while scoped lock is held");
        assert!(
            err.to_string().contains("scoped lock"),
            "error must name the scoped lock, got: {err}"
        );
    }

    #[tokio::test]
    async fn vacuum_refuses_when_legacy_lock_is_held() {
        let (_dir, app_home, db_path) = fresh_fixture();
        let legacy_path = crate::daemon_lock::legacy_daemon_lock_path(&app_home);
        let _holder = crate::daemon_lock::DaemonLock::acquire(&legacy_path)
            .expect("pre-acquire legacy lock to simulate an un-upgraded live daemon");

        let db_arg = db_path.display().to_string();
        let result = run_vacuum_cli(&db_arg, true, &app_home, false).await;

        let err = result.expect_err("must refuse while legacy lock is held");
        assert!(
            err.to_string().contains("legacy lock"),
            "error must name the legacy lock, got: {err}"
        );
    }

    #[tokio::test]
    async fn dry_run_does_not_touch_locks() {
        // --apply=false must short-circuit before any lock acquisition —
        // even with a live-looking scoped lock present, dry-run must not
        // error out on it.
        let (_dir, app_home, db_path) = fresh_fixture();
        let scoped_path = crate::daemon_lock::scoped_daemon_lock_path(&app_home, &db_path);
        let _holder =
            crate::daemon_lock::DaemonLock::acquire(&scoped_path).expect("pre-acquire scoped lock");

        let db_arg = db_path.display().to_string();
        let result = run_vacuum_cli(&db_arg, false, &app_home, false).await;

        assert!(result.is_ok(), "dry-run must not touch locks: {result:?}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn vacuum_refuses_manifest_project_symlink_without_following_foreign_db() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, app_home, foreign_db) = fresh_fixture();
        let project_link = app_home.join("linked-project.db");
        std::os::unix::fs::symlink(&foreign_db, &project_link).expect("project symlink");
        let mut manifest = Manifest::load(&app_home.join("manifest.json")).unwrap();
        manifest.dbs[0].path = project_link.to_string_lossy().into_owned();
        manifest.dbs[0].scope_hint = "project:linked".into();
        manifest.save(&app_home.join("manifest.json")).unwrap();
        let foreign_metadata = std::fs::symlink_metadata(&foreign_db).unwrap();
        let foreign_identity = (foreign_metadata.dev(), foreign_metadata.ino());
        let foreign_before = std::fs::read(&foreign_db).unwrap();
        let link_metadata = std::fs::symlink_metadata(&project_link).unwrap();
        let link_identity = (link_metadata.dev(), link_metadata.ino());

        let error = run_vacuum_cli("project:linked", false, &app_home, false)
            .await
            .expect_err("manifest project symlink must refuse vacuum");

        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "unexpected refusal: {error}"
        );
        let link_after = std::fs::symlink_metadata(&project_link).unwrap();
        assert_eq!((link_after.dev(), link_after.ino()), link_identity);
        assert_eq!(std::fs::read_link(&project_link).unwrap(), foreign_db);
        let foreign_after = std::fs::symlink_metadata(&foreign_db).unwrap();
        assert_eq!((foreign_after.dev(), foreign_after.ino()), foreign_identity);
        assert_eq!(std::fs::read(&foreign_db).unwrap(), foreign_before);
    }
}

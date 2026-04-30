//! R6 — `VACUUM INTO` then atomic swap. Requires daemon stopped.

use std::path::Path;

use rusqlite::Connection;
use serde_json::json;

use crate::daemon_lock::DaemonLock;
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

    // Acquire daemon.lock exclusively. If a live daemon is using it, refuse.
    let lock_path = app_home.join("daemon.lock");
    let _lock = match DaemonLock::acquire(&lock_path) {
        Ok(l) => l,
        Err(e) => {
            return Err(format!(
                "cannot acquire daemon.lock for VACUUM ({e}). Stop the daemon first: `tachi daemon kill`"
            )
            .into());
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

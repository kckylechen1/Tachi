use chrono::Utc;
#[cfg(unix)]
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::classify::sidecar;
use super::{AutoFixAction, DbClassification, DoctorFinding};

// ─── Auto-fix ────────────────────────────────────────────────────────────────

/// Apply the SAFE auto-fix categories: quarantine placeholders, copy-checkpoint
/// WAL orphans. Returns the action log. `quarantine_root` is created if needed.
pub fn auto_fix_safe(findings: &[DoctorFinding], quarantine_root: &Path) -> Vec<AutoFixAction> {
    let mut actions = Vec::new();
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();

    for f in findings {
        match f.classification {
            DbClassification::Placeholder => {
                let dest_dir = quarantine_root.join("placeholders").join(&ts);
                let action = quarantine_placeholder(&f.path, &dest_dir);
                actions.push(action);
            }
            DbClassification::WalOrphan => {
                let action = checkpoint_wal_copy(&f.path);
                actions.push(action);
            }
            _ => {}
        }
    }
    actions.extend(plan_c_alias_retirement_actions(findings.iter().filter(
        |finding| plan_c_alias_retirement_allowed(finding.classification),
    )));
    actions
}

fn plan_c_alias_retirement_allowed(classification: DbClassification) -> bool {
    matches!(classification, DbClassification::Healthy)
}

#[cfg(unix)]
fn plan_c_alias_retirement_actions<'a>(
    findings: impl IntoIterator<Item = &'a DoctorFinding>,
) -> Vec<AutoFixAction> {
    let mut actions = Vec::new();
    let mut seen = HashSet::new();

    for finding in findings {
        let path = PathBuf::from(&finding.path);
        let Ok(canonical) = fs::canonicalize(&path) else {
            continue;
        };
        let Some(project_root) = crate::path_utils::plan_c_project_root_from_local_db(&canonical)
        else {
            continue;
        };
        if !seen.insert(canonical.clone()) {
            continue;
        }
        actions.extend(reconcile_plan_c_alias_for_local_db(
            &canonical,
            &project_root,
        ));
    }

    actions
}

#[cfg(not(unix))]
fn plan_c_alias_retirement_actions<'a>(
    _findings: impl IntoIterator<Item = &'a DoctorFinding>,
) -> Vec<AutoFixAction> {
    Vec::new()
}

#[cfg(unix)]
fn reconcile_plan_c_alias_for_local_db(local_db: &Path, project_root: &Path) -> Vec<AutoFixAction> {
    let mut actions = Vec::new();
    let Some(current_name) = crate::path_utils::plan_c_dir_name_from_root(project_root) else {
        return actions;
    };
    let Some(legacy_name) = crate::path_utils::plan_c_legacy_dir_name_from_root(project_root)
    else {
        return actions;
    };
    let current_db = crate::path_utils::plan_c_global_db_path(&current_name);

    let projects_root = crate::path_utils::tachi_home().join("projects");
    let Ok(entries) = fs::read_dir(&projects_root) else {
        return actions;
    };
    let mut old_hash_aliases = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == current_name || name == legacy_name {
            continue;
        }
        if !looks_like_old_plan_c_hash_alias(name, &legacy_name) {
            continue;
        }
        let candidate = entry.path().join("memory.db");
        if !old_hash_alias_points_to_local_db(&candidate, local_db) {
            continue;
        }
        old_hash_aliases.push(candidate);
    }
    if old_hash_aliases.is_empty() {
        return actions;
    }

    match ensure_current_hashed_alias(local_db, &current_db) {
        Ok(Some(action)) => actions.push(action),
        Ok(None) => {}
        Err(action) => {
            actions.push(action);
            return actions;
        }
    }

    for candidate in old_hash_aliases {
        actions.push(retire_old_hash_alias(
            &candidate,
            local_db,
            &current_db,
            &current_name,
        ));
    }

    actions
}

#[cfg(unix)]
fn retire_old_hash_alias(
    candidate: &Path,
    local_db: &Path,
    current_db: &Path,
    current_name: &str,
) -> AutoFixAction {
    if !old_hash_alias_points_to_local_db(candidate, local_db) {
        return AutoFixAction {
            path: candidate.display().to_string(),
            action: "plan_c_alias_retire_old_hash".to_string(),
            outcome: "skipped".to_string(),
            note: "old-hash alias changed before removal; refused stale delete".to_string(),
            destination: Some(current_db.display().to_string()),
        };
    }
    match fs::remove_file(candidate) {
        Ok(()) => {
            if let Some(parent) = candidate.parent() {
                let _ = fs::remove_dir(parent);
            }
            AutoFixAction {
                path: candidate.display().to_string(),
                action: "plan_c_alias_retire_old_hash".to_string(),
                outcome: "ok".to_string(),
                note: format!(
                    "retired old-hash Plan C alias after creating current alias {current_name}"
                ),
                destination: Some(current_db.display().to_string()),
            }
        }
        Err(e) => AutoFixAction {
            path: candidate.display().to_string(),
            action: "plan_c_alias_retire_old_hash".to_string(),
            outcome: "error".to_string(),
            note: format!("remove old-hash alias symlink: {e}"),
            destination: Some(current_db.display().to_string()),
        },
    }
}

#[cfg(unix)]
fn ensure_current_hashed_alias(
    local_db: &Path,
    current_db: &Path,
) -> Result<Option<AutoFixAction>, AutoFixAction> {
    if current_db.is_symlink() {
        if fs::canonicalize(current_db)
            .map(|path| path == local_db)
            .unwrap_or(false)
        {
            return Ok(None);
        }
        return Err(AutoFixAction {
            path: current_db.display().to_string(),
            action: "plan_c_alias_create_hashed".to_string(),
            outcome: "error".to_string(),
            note: "current hashed alias already exists but points elsewhere; refusing retirement"
                .to_string(),
            destination: None,
        });
    }
    if current_db.exists() {
        return Err(AutoFixAction {
            path: current_db.display().to_string(),
            action: "plan_c_alias_create_hashed".to_string(),
            outcome: "error".to_string(),
            note: "current hashed alias path exists and is not a symlink; refusing retirement"
                .to_string(),
            destination: None,
        });
    }
    let Some(parent) = current_db.parent() else {
        return Err(AutoFixAction {
            path: current_db.display().to_string(),
            action: "plan_c_alias_create_hashed".to_string(),
            outcome: "error".to_string(),
            note: "current hashed alias path has no parent".to_string(),
            destination: None,
        });
    };
    if let Err(e) = fs::create_dir_all(parent) {
        return Err(AutoFixAction {
            path: current_db.display().to_string(),
            action: "plan_c_alias_create_hashed".to_string(),
            outcome: "error".to_string(),
            note: format!("create current hashed alias dir: {e}"),
            destination: None,
        });
    }
    if let Err(e) = std::os::unix::fs::symlink(local_db, current_db) {
        return Err(AutoFixAction {
            path: current_db.display().to_string(),
            action: "plan_c_alias_create_hashed".to_string(),
            outcome: "error".to_string(),
            note: format!("create current hashed alias symlink: {e}"),
            destination: None,
        });
    }
    Ok(Some(AutoFixAction {
        path: current_db.display().to_string(),
        action: "plan_c_alias_create_hashed".to_string(),
        outcome: "ok".to_string(),
        note: "created current hashed Plan C alias".to_string(),
        destination: Some(local_db.display().to_string()),
    }))
}

#[cfg(unix)]
fn old_hash_alias_points_to_local_db(candidate: &Path, local_db: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(candidate) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    fs::canonicalize(candidate)
        .map(|path| path == local_db)
        .unwrap_or(false)
}

#[cfg(unix)]
fn looks_like_old_plan_c_hash_alias(name: &str, legacy_name: &str) -> bool {
    let Some(suffix) = name.strip_prefix(&format!("{legacy_name}-")) else {
        return false;
    };
    suffix.len() == 8 && suffix.chars().all(|ch| ch.is_ascii_hexdigit())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn plan_c_alias_retirement_allows_only_valid_safe_targets() {
        assert!(plan_c_alias_retirement_allowed(DbClassification::Healthy));
        // WalOrphan excluded: doctor validates via immutable-read (WAL ignored),
        // and the alias points at the un-checkpointed original — not a proven-clean target.
        assert!(!plan_c_alias_retirement_allowed(
            DbClassification::WalOrphan
        ));
        assert!(!plan_c_alias_retirement_allowed(
            DbClassification::VecExtensionMissing
        ));
        assert!(!plan_c_alias_retirement_allowed(DbClassification::Corrupt));
        assert!(!plan_c_alias_retirement_allowed(
            DbClassification::LegacySchema
        ));
        assert!(!plan_c_alias_retirement_allowed(
            DbClassification::Placeholder
        ));
        assert!(!plan_c_alias_retirement_allowed(DbClassification::Backup));
    }

    #[test]
    fn retire_old_hash_alias_refuses_stale_delete_when_target_changes() {
        let dir = tempdir().unwrap();
        let local_db = dir.path().join("repo/.tachi/memory.db");
        let other_db = dir.path().join("other/.tachi/memory.db");
        let candidate = dir.path().join("projects/Sigil-94c144a9/memory.db");
        let current_db = dir.path().join("projects/Sigil-current/memory.db");
        fs::create_dir_all(local_db.parent().unwrap()).unwrap();
        fs::create_dir_all(other_db.parent().unwrap()).unwrap();
        fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        fs::write(&local_db, b"local").unwrap();
        fs::write(&other_db, b"other").unwrap();
        std::os::unix::fs::symlink(&other_db, &candidate).unwrap();

        let action = retire_old_hash_alias(&candidate, &local_db, &current_db, "Sigil-current");

        assert_eq!(action.action, "plan_c_alias_retire_old_hash");
        assert_eq!(action.outcome, "skipped");
        assert!(action.note.contains("refused stale delete"));
        assert_eq!(action.destination, Some(current_db.display().to_string()));
        assert!(candidate.is_symlink(), "stale alias must not be deleted");
        assert_eq!(
            fs::canonicalize(&candidate).unwrap(),
            fs::canonicalize(other_db).unwrap()
        );
    }

    #[test]
    fn ensure_current_hashed_alias_refuses_symlink_pointing_elsewhere() {
        let dir = tempdir().unwrap();
        let local_db = dir.path().join("repo/.tachi/memory.db");
        let other_db = dir.path().join("other/.tachi/memory.db");
        let current_db = dir.path().join("projects/Sigil-current/memory.db");
        fs::create_dir_all(local_db.parent().unwrap()).unwrap();
        fs::create_dir_all(other_db.parent().unwrap()).unwrap();
        fs::create_dir_all(current_db.parent().unwrap()).unwrap();
        fs::write(&local_db, b"local").unwrap();
        fs::write(&other_db, b"other").unwrap();
        std::os::unix::fs::symlink(&other_db, &current_db).unwrap();

        let action = ensure_current_hashed_alias(&local_db, &current_db).unwrap_err();

        assert_eq!(action.action, "plan_c_alias_create_hashed");
        assert_eq!(action.outcome, "error");
        assert!(action.note.contains("points elsewhere"));
        assert!(current_db.is_symlink());
        assert_eq!(
            fs::canonicalize(&current_db).unwrap(),
            fs::canonicalize(other_db).unwrap()
        );
    }

    #[test]
    fn ensure_current_hashed_alias_refuses_regular_file() {
        let dir = tempdir().unwrap();
        let local_db = dir.path().join("repo/.tachi/memory.db");
        let current_db = dir.path().join("projects/Sigil-current/memory.db");
        fs::create_dir_all(local_db.parent().unwrap()).unwrap();
        fs::create_dir_all(current_db.parent().unwrap()).unwrap();
        fs::write(&local_db, b"local").unwrap();
        fs::write(&current_db, b"regular file").unwrap();

        let action = ensure_current_hashed_alias(&local_db, &current_db).unwrap_err();

        assert_eq!(action.action, "plan_c_alias_create_hashed");
        assert_eq!(action.outcome, "error");
        assert!(action.note.contains("not a symlink"));
        assert!(current_db.is_file());
    }
}

fn quarantine_placeholder(src: &str, dest_dir: &Path) -> AutoFixAction {
    let src_path = Path::new(src);
    let basename = src_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("memory.db");
    if let Err(e) = fs::create_dir_all(dest_dir) {
        return AutoFixAction {
            path: src.to_string(),
            action: "quarantine_placeholder".to_string(),
            outcome: "error".to_string(),
            note: format!("create_dir_all({}): {e}", dest_dir.display()),
            destination: None,
        };
    }
    let dest = dest_dir.join(quarantine_dest_filename(src, basename));
    match fs::rename(src_path, &dest) {
        Ok(_) => AutoFixAction {
            path: src.to_string(),
            action: "quarantine_placeholder".to_string(),
            outcome: "ok".to_string(),
            note: format!("moved 0-byte placeholder to quarantine"),
            destination: Some(dest.display().to_string()),
        },
        Err(e) => {
            // Cross-device rename can fail; try copy+remove as fallback.
            match fs::copy(src_path, &dest).and_then(|_| fs::remove_file(src_path)) {
                Ok(_) => AutoFixAction {
                    path: src.to_string(),
                    action: "quarantine_placeholder".to_string(),
                    outcome: "ok".to_string(),
                    note: "moved 0-byte placeholder to quarantine (copy+remove)".to_string(),
                    destination: Some(dest.display().to_string()),
                },
                Err(e2) => AutoFixAction {
                    path: src.to_string(),
                    action: "quarantine_placeholder".to_string(),
                    outcome: "error".to_string(),
                    note: format!("rename: {e}; copy+remove: {e2}"),
                    destination: None,
                },
            }
        }
    }
}

pub(crate) fn quarantine_dest_filename(src: &str, basename: &str) -> String {
    let mut safe_src = src
        .trim_start_matches('/')
        .replace(['/', ':'], "_")
        .chars()
        .take(120)
        .collect::<String>();
    if safe_src.is_empty() {
        safe_src = "unknown".to_string();
    }
    let safe_basename = basename.chars().take(80).collect::<String>();
    format!("{safe_src}__{:016x}__{safe_basename}", stable_hash(src))
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn checkpoint_wal_copy(src: &str) -> AutoFixAction {
    let src_path = Path::new(src);

    // Liveness guard: if a daemon is currently holding this DB, copying
    // the three files (main + -wal + -shm) non-atomically produces a
    // torn snapshot, and running PRAGMA wal_checkpoint(TRUNCATE) on the
    // copy is undefined behavior on a partial WAL. Skip and explain.
    if daemon_owns_db(src_path) {
        return AutoFixAction {
            path: src.to_string(),
            action: "checkpoint_wal_copy".to_string(),
            outcome: "skipped".to_string(),
            note: "live daemon holds this DB; refuse to make a torn copy".to_string(),
            destination: None,
        };
    }

    // Timestamped destination so repeated `tachi doctor` runs do not
    // overwrite each other (previously: a single `<src>.checkpointed.db`
    // got clobbered or accumulated unbounded depending on path layout).
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let dest = {
        let mut s = src_path.as_os_str().to_owned();
        s.push(format!(".checkpointed.{ts}.db"));
        PathBuf::from(s)
    };
    let wal = sidecar(src_path, "-wal");
    let shm = sidecar(src_path, "-shm");

    // Copy main DB.
    if let Err(e) = fs::copy(src_path, &dest) {
        return AutoFixAction {
            path: src.to_string(),
            action: "checkpoint_wal_copy".to_string(),
            outcome: "error".to_string(),
            note: format!("copy main: {e}"),
            destination: None,
        };
    }
    // Copy sidecars so the new copy can replay WAL.
    if wal.exists() {
        let mut wal_dest = dest.as_os_str().to_owned();
        wal_dest.push("-wal");
        let _ = fs::copy(&wal, PathBuf::from(wal_dest));
    }
    if shm.exists() {
        let mut shm_dest = dest.as_os_str().to_owned();
        shm_dest.push("-shm");
        let _ = fs::copy(&shm, PathBuf::from(shm_dest));
    }

    // Open the COPY read-write and force a TRUNCATE checkpoint.
    let dest_str = dest.to_string_lossy().to_string();
    let result = match memcore::db::open_for_wal_checkpoint(&dest_str) {
        Ok(conn) => {
            // Best-effort; ignore returned WAL stats.
            match memcore::db::checkpoint_wal_truncate(&conn) {
                Ok(_) => AutoFixAction {
                    path: src.to_string(),
                    action: "checkpoint_wal_copy".to_string(),
                    outcome: "ok".to_string(),
                    note: "wrote .checkpointed.<ts>.db copy (original untouched)".to_string(),
                    destination: Some(dest_str),
                },
                Err(e) => AutoFixAction {
                    path: src.to_string(),
                    action: "checkpoint_wal_copy".to_string(),
                    outcome: "error".to_string(),
                    note: format!("wal_checkpoint failed on copy: {e}"),
                    destination: Some(dest_str),
                },
            }
        }
        Err(e) => AutoFixAction {
            path: src.to_string(),
            action: "checkpoint_wal_copy".to_string(),
            outcome: "error".to_string(),
            note: format!("open copy: {e}"),
            destination: Some(dest_str),
        },
    };

    // GC: keep at most the 3 most recent .checkpointed.*.db copies per
    // source DB. Older copies are silently removed (best-effort; log via
    // appended note on the result if anything fails).
    let gc_note = gc_old_checkpoint_copies(src_path, 3);
    if let Some(extra) = gc_note {
        let mut r = result;
        r.note = if r.note.is_empty() {
            extra
        } else {
            format!("{}; {extra}", r.note)
        };
        return r;
    }
    result
}

/// Best-effort detection: does any running tachi-tachi-server have an
/// open file handle on `db_path`? Uses `lsof` on Unix; on other platforms
/// (or if `lsof` is missing / errors) returns false (i.e. fall through to
/// the unguarded copy path — preserves prior behavior).
#[cfg(unix)]
fn daemon_owns_db(db_path: &Path) -> bool {
    use std::process::Command;
    let abs = match db_path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let abs_str = abs.to_string_lossy().to_string();
    // Using `--` to terminate options before the path argument so paths
    // beginning with `-` are treated literally.
    let output = Command::new("lsof").arg("--").arg(&abs_str).output();
    match output {
        Ok(o) if o.status.success() => {
            // lsof prints a header line + one line per holder; >1 line means held.
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.lines().count() > 1
        }
        _ => false,
    }
}

#[cfg(not(unix))]
fn daemon_owns_db(_db_path: &Path) -> bool {
    false
}

/// Remove all but the `keep` most-recent `<src>.checkpointed.*.db` (and
/// their `-wal`/`-shm` sidecars) sitting next to `src_path`. Returns
/// Some(note) only on partial failure so the caller can surface it.
fn gc_old_checkpoint_copies(src_path: &Path, keep: usize) -> Option<String> {
    let dir = src_path.parent()?;
    let basename = src_path.file_name()?.to_string_lossy().to_string();
    let prefix = format!("{basename}.checkpointed.");
    let suffix = ".db";

    let mut copies: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let entries = fs::read_dir(dir).ok()?;
    for ent in entries.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        // `ends_with(".db")` already excludes `.db-wal` / `.db-shm` sidecars
        // — those go away together with their parent below.
        if name.starts_with(&prefix) && name.ends_with(suffix) {
            if let Ok(meta) = ent.metadata() {
                if let Ok(mtime) = meta.modified() {
                    copies.push((mtime, ent.path()));
                }
            }
        }
    }
    if copies.len() <= keep {
        return None;
    }
    // Newest first; drop the tail.
    copies.sort_by(|a, b| b.0.cmp(&a.0));
    let to_remove = &copies[keep..];
    let mut errs = Vec::new();
    for (_ts, path) in to_remove {
        if let Err(e) = fs::remove_file(path) {
            errs.push(format!("rm {}: {e}", path.display()));
        }
        // Sidecars (best-effort). Reuse the existing `sidecar` helper.
        let _ = fs::remove_file(sidecar(path, "-wal"));
        let _ = fs::remove_file(sidecar(path, "-shm"));
    }
    if errs.is_empty() {
        None
    } else {
        Some(format!("gc: {}", errs.join(", ")))
    }
}

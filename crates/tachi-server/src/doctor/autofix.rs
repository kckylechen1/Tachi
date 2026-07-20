use chrono::Utc;
#[cfg(unix)]
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(all(test, unix))]
use std::cell::Cell;

use super::classify::sidecar;
use super::{AutoFixAction, DbClassification, DoctorFinding};

// Test inject: corrupt the exclusive dest before open/checkpoint so the
// failure path runs, and force `discard_incomplete_checkpoint` to leave the
// ordinary `.checkpointed.*.db` name in place (stuck cleanup).
#[cfg(all(test, unix))]
thread_local! {
    static INJECT_STUCK_FAILED_DEST: Cell<bool> = const { Cell::new(false) };
}

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
        // Check the canonical (post-#1132) filename first, then the legacy
        // one — an old-hash alias dir not yet touched by an `open()` call
        // since the rename shipped may still only have the legacy symlink.
        let candidate = [
            memcore::MEMORY_DB_FILENAME,
            memcore::LEGACY_MEMORY_DB_FILENAME,
        ]
        .into_iter()
        .map(|name| entry.path().join(name))
        .find(|candidate| old_hash_alias_points_to_local_db(candidate, local_db));
        let Some(candidate) = candidate else {
            continue;
        };
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
        .unwrap_or(memcore::MEMORY_DB_FILENAME);
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
    //
    // `Unknown` fails closed exactly like `Owned`: if we cannot tell
    // whether a daemon holds the file, we must not risk the torn-copy +
    // undefined-checkpoint failure mode either. Only a confirmed `NotOwned`
    // may fall through to the copy path below.
    match daemon_ownership(src_path) {
        DbOwnership::Owned => {
            return AutoFixAction {
                path: src.to_string(),
                action: "checkpoint_wal_copy".to_string(),
                outcome: "skipped".to_string(),
                note: "live daemon holds this DB; refuse to make a torn copy".to_string(),
                destination: None,
            };
        }
        DbOwnership::Unknown(reason) => {
            return AutoFixAction {
                path: src.to_string(),
                action: "checkpoint_wal_copy".to_string(),
                outcome: "skipped".to_string(),
                note: format!(
                    "daemon ownership undetermined ({reason}); refusing to risk a torn copy"
                ),
                destination: None,
            };
        }
        DbOwnership::NotOwned => {}
    }

    // Collision-resistant destination: timestamp (µs) + short uuid so two
    // runs in the same second cannot share a path. Pattern stays
    // `.checkpointed.<stamp>.db` for scan/GC recognition.
    let dest = match copy_main_exclusive(src_path) {
        Ok(p) => p,
        Err(e) => {
            return AutoFixAction {
                path: src.to_string(),
                action: "checkpoint_wal_copy".to_string(),
                outcome: "error".to_string(),
                note: format!("copy main: {e}"),
                destination: None,
            };
        }
    };
    let wal = sidecar(src_path, "-wal");
    let shm = sidecar(src_path, "-shm");
    let wal_dest = {
        let mut p = dest.as_os_str().to_owned();
        p.push("-wal");
        PathBuf::from(p)
    };
    let shm_dest = {
        let mut p = dest.as_os_str().to_owned();
        p.push("-shm");
        PathBuf::from(p)
    };

    // WAL is durability-critical: a present source WAL that cannot be copied
    // must block success. Do not open/checkpoint an incomplete main-only copy.
    if wal.exists() {
        if let Err(e) = fs::copy(&wal, &wal_dest) {
            let (action, skip_gc) = checkpoint_failure_action(
                src,
                &dest,
                format!("copy wal: {e}"),
            );
            return finish_checkpoint_action(src_path, action, skip_gc);
        }
    }

    // SHM is NOT durability-equivalent to WAL. Copy failure: drop any partial
    // shm dest and proceed with main(+wal); receipt must name the outcome
    // honestly if a partial shm cannot be removed.
    let shm_token = if !shm.exists() {
        "shm=absent".to_string()
    } else if let Err(copy_err) = fs::copy(&shm, &shm_dest) {
        shm_copy_failure_token(&shm_dest, &copy_err)
    } else {
        "shm=ok".to_string()
    };

    #[cfg(all(test, unix))]
    if INJECT_STUCK_FAILED_DEST.get() {
        // Force open/checkpoint failure while leaving a normal-looking dest name
        // for discard inject (see discard_incomplete_checkpoint).
        // Drop copied sidecars first — a leftover -wal can mask a corrupt main.
        let _ = fs::remove_file(&wal_dest);
        let _ = fs::remove_file(&shm_dest);
        fs::write(
            &dest,
            b"not-a-sqlite-db-for-stuck-discard-inject!!!!!",
        )
        .expect("inject corrupt dest");
    }

    // Open the COPY read-write and force a TRUNCATE checkpoint.
    // Success alone may advertise destination; open/checkpoint failures must
    // discard and return destination: None (never a usable action target).
    let dest_str = dest.to_string_lossy().to_string();
    let (result, skip_gc) = match memcore::db::open_for_wal_checkpoint(&dest_str) {
        Ok(conn) => {
            // Best-effort; ignore returned WAL stats.
            match memcore::db::checkpoint_wal_truncate(&conn) {
                Ok(_) => (
                    AutoFixAction {
                        path: src.to_string(),
                        action: "checkpoint_wal_copy".to_string(),
                        outcome: "ok".to_string(),
                        note: format!(
                            "wrote .checkpointed.<stamp>.db copy (original untouched); {shm_token}"
                        ),
                        destination: Some(dest_str),
                    },
                    false,
                ),
                Err(e) => checkpoint_failure_action(
                    src,
                    &dest,
                    format!("wal_checkpoint failed on copy: {e}; {shm_token}"),
                ),
            }
        }
        Err(e) => checkpoint_failure_action(
            src,
            &dest,
            format!("open copy: {e}; {shm_token}"),
        ),
    };

    finish_checkpoint_action(src_path, result, skip_gc)
}

/// GC after a checkpoint attempt, unless cleanup left a stuck ordinary
/// `.checkpointed.*.db` name (which GC would treat as a usable copy).
fn finish_checkpoint_action(
    src_path: &Path,
    result: AutoFixAction,
    skip_gc: bool,
) -> AutoFixAction {
    if skip_gc {
        return result;
    }
    // GC: keep at most the 3 most recent .checkpointed.*.db copies per
    // source DB. Sidecar NotFound is success; other delete errors surface.
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

/// Unique `.checkpointed.<ts>-<uuid8>.db` path under exclusive create-new.
fn unique_checkpoint_dest(src_path: &Path) -> PathBuf {
    let stamp = format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%S%.6fZ"),
        &uuid::Uuid::new_v4().as_simple().to_string()[..8]
    );
    let mut s = src_path.as_os_str().to_owned();
    s.push(format!(".checkpointed.{stamp}.db"));
    PathBuf::from(s)
}

/// Copy main DB into a newly claimed exclusive destination (retry on name clash).
fn copy_main_exclusive(src: &Path) -> io::Result<PathBuf> {
    for _ in 0..16 {
        let dest = unique_checkpoint_dest(src);
        match copy_create_new(src, &dest) {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                let _ = fs::remove_file(&dest);
                return Err(e);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "exhausted unique checkpoint destination names",
    ))
}

fn copy_create_new(src: &Path, dest: &Path) -> io::Result<()> {
    let mut out = OpenOptions::new().write(true).create_new(true).open(dest)?;
    let mut inp = File::open(src)?;
    io::copy(&mut inp, &mut out)?;
    out.flush()?;
    // Exclusive create uses process umask defaults; restore source mode bits
    // before any success advertisement (owner-only DBs must stay owner-only).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(src)?.permissions().mode();
        fs::set_permissions(dest, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Discard incomplete dest; second value is `true` when cleanup could not
/// remove or quarantine the ordinary checkpoint name — caller must skip GC.
fn checkpoint_failure_action(src: &str, dest: &Path, note: String) -> (AutoFixAction, bool) {
    let stuck = discard_incomplete_checkpoint(dest);
    let skip_gc = stuck.is_some();
    let note = match stuck {
        Some(cleanup) => format!("{note}; {cleanup}"),
        None => note,
    };
    (
        AutoFixAction {
            path: src.to_string(),
            action: "checkpoint_wal_copy".to_string(),
            outcome: "error".to_string(),
            note,
            destination: None,
        },
        skip_gc,
    )
}

fn shm_copy_failure_token(shm_dest: &Path, copy_err: &io::Error) -> String {
    match fs::remove_file(shm_dest) {
        Ok(()) => "shm=copy_failed_proceeded_without".to_string(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            "shm=copy_failed_proceeded_without".to_string()
        }
        Err(rm_err) => {
            let incomplete = {
                let mut p = shm_dest.as_os_str().to_owned();
                p.push(".incomplete");
                PathBuf::from(p)
            };
            match fs::rename(shm_dest, &incomplete) {
                Ok(()) => format!(
                    "shm=copy_failed_partial_renamed_incomplete (copy: {copy_err}; rm: {rm_err})"
                ),
                Err(rename_err) => format!(
                    "shm=copy_failed_partial_remains (copy: {copy_err}; rm: {rm_err}; rename: {rename_err})"
                ),
            }
        }
    }
}

/// Remove a partial checkpoint destination (main + sidecars). On stubborn
/// delete failure, rename to an explicit `.incomplete` suffix so the path is
/// never a normal-looking usable checkpoint. Returns a note when a usable
/// name could not be cleared.
fn discard_incomplete_checkpoint(dest: &Path) -> Option<String> {
    #[cfg(all(test, unix))]
    if INJECT_STUCK_FAILED_DEST.get() {
        // Leave the ordinary `.checkpointed.*.db` name in place so GC would
        // otherwise treat it as a usable copy (discrimination for skip-GC).
        return Some(format!(
            "incomplete remains at {} (rm: injected; rename: injected)",
            dest.display()
        ));
    }

    let mut stuck = Vec::new();
    for path in [
        dest.to_path_buf(),
        sidecar(dest, "-wal"),
        sidecar(dest, "-shm"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(rm_err) => {
                let incomplete = {
                    let mut p = path.as_os_str().to_owned();
                    p.push(".incomplete");
                    PathBuf::from(p)
                };
                match fs::rename(&path, &incomplete) {
                    Ok(()) => {}
                    Err(rename_err) => stuck.push(format!(
                        "incomplete remains at {} (rm: {rm_err}; rename: {rename_err})",
                        path.display()
                    )),
                }
            }
        }
    }
    if stuck.is_empty() {
        None
    } else {
        Some(stuck.join("; "))
    }
}

/// Tri-state result of probing whether a live daemon holds `db_path` open.
/// `Unknown` must fail closed at every call site: it is not a synonym for
/// `NotOwned`, and callers must never fall through to an unguarded copy on
/// `Unknown` — that was the fail-open bug (torn main+wal+shm copies plus a
/// `wal_checkpoint(TRUNCATE)` against a partial WAL, reported as `outcome=ok`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum DbOwnership {
    /// lsof ran cleanly and found at least one process with the file open.
    Owned,
    /// lsof ran cleanly and found no holders.
    NotOwned,
    /// Ownership could not be determined; `reason` is surfaced verbatim in
    /// the caller's receipt note (never swallowed).
    Unknown(String),
}

/// Best-effort detection: does any running tachi-tachi-server have an
/// open file handle on `db_path`? Uses `lsof` on Unix. Only a clean lsof
/// run (readable exit status + output) may resolve to `Owned`/`NotOwned`;
/// canonicalize failure, a missing/erroring `lsof`, and non-unix platforms
/// all return `Unknown` — the caller must fail closed on that, not fall
/// through to the unguarded copy path.
#[cfg(unix)]
fn probe_db_ownership(db_path: &Path) -> DbOwnership {
    use std::process::Command;
    let abs = match db_path.canonicalize() {
        Ok(p) => p,
        Err(e) => return DbOwnership::Unknown(format!("canonicalize failed: {e}")),
    };
    let abs_str = abs.to_string_lossy().to_string();
    // Using `--` to terminate options before the path argument so paths
    // beginning with `-` are treated literally.
    let output = Command::new("lsof").arg("--").arg(&abs_str).output();
    match output {
        Ok(o) if o.status.success() => {
            // lsof prints a header line + one line per holder; >1 line means held.
            let stdout = String::from_utf8_lossy(&o.stdout);
            if stdout.lines().count() > 1 {
                DbOwnership::Owned
            } else {
                DbOwnership::NotOwned
            }
        }
        Ok(o) => {
            // lsof's non-zero exit is ambiguous by design: it means either
            // "no holders found" (the common, harmless case — empty
            // stdout/stderr) or a genuine fault (permission denied, lsof
            // internal error, unexpected args). Only trust the "no holders"
            // reading when lsof stayed silent on both streams; any
            // diagnostic text means we cannot tell, so fail closed.
            let stderr = String::from_utf8_lossy(&o.stderr);
            let stdout = String::from_utf8_lossy(&o.stdout);
            if stderr.trim().is_empty() && stdout.trim().is_empty() {
                DbOwnership::NotOwned
            } else {
                let detail = stderr
                    .lines()
                    .next()
                    .filter(|l| !l.trim().is_empty())
                    .or_else(|| stdout.lines().next())
                    .unwrap_or("lsof exited non-zero")
                    .trim()
                    .to_string();
                DbOwnership::Unknown(format!("lsof error: {detail}"))
            }
        }
        Err(e) => DbOwnership::Unknown(format!("lsof unavailable: {e}")),
    }
}

#[cfg(not(unix))]
fn probe_db_ownership(_db_path: &Path) -> DbOwnership {
    DbOwnership::Unknown("unsupported platform (no ownership probe on non-unix)".to_string())
}

// Test inject: force `daemon_ownership` to return a specific result without
// depending on real lsof behavior (a genuinely erroring lsof is not
// reliably reproducible across CI environments).
#[cfg(all(test, unix))]
thread_local! {
    static INJECT_OWNERSHIP: std::cell::RefCell<Option<DbOwnership>> =
        const { std::cell::RefCell::new(None) };
}

fn daemon_ownership(db_path: &Path) -> DbOwnership {
    #[cfg(all(test, unix))]
    {
        if let Some(forced) = INJECT_OWNERSHIP.with(|c| c.borrow_mut().take()) {
            return forced;
        }
    }
    probe_db_ownership(db_path)
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
        // — those go away together with their parent below. Skip quarantined
        // `.incomplete` leftovers so GC never treats them as usable copies.
        if name.contains(".incomplete") {
            continue;
        }
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
        // Sidecar NotFound is success (idempotent). Any other delete error
        // must be reported with path+error — never silent `let _ =`.
        report_sidecar_delete(&sidecar(path, "-wal"), &mut errs);
        report_sidecar_delete(&sidecar(path, "-shm"), &mut errs);
    }
    if errs.is_empty() {
        None
    } else {
        Some(format!("gc: {}", errs.join(", ")))
    }
}

fn report_sidecar_delete(path: &Path, errs: &mut Vec<String>) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => errs.push(format!("rm {}: {e}", path.display())),
    }
}

#[cfg(all(test, unix))]
mod checkpoint_honesty_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn write_wal_mode_db(path: &Path) {
        let conn = rusqlite::Connection::open(path).expect("open fixture db");
        conn.pragma_update(None, "journal_mode", "WAL")
            .expect("journal_mode=WAL");
        conn.pragma_update(None, "wal_autocheckpoint", 0)
            .expect("disable wal autocheckpoint");
        // Keep -wal/-shm after the last connection closes so copy-path tests
        // can observe present sidecars (SQLite otherwise deletes them).
        unsafe {
            let mut enable: std::os::raw::c_int = 1;
            let rc = rusqlite::ffi::sqlite3_file_control(
                conn.handle(),
                std::ptr::null(),
                rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
                &mut enable as *mut _ as *mut std::os::raw::c_void,
            );
            assert_eq!(rc, rusqlite::ffi::SQLITE_OK, "SQLITE_FCNTL_PERSIST_WAL");
        }
        conn.execute_batch(
            "CREATE TABLE t(x INTEGER);
             INSERT INTO t VALUES (1);",
        )
        .expect("seed wal-mode fixture");
        drop(conn);
        assert!(
            sidecar(path, "-wal").exists(),
            "WAL-mode fixture must leave a -wal sidecar"
        );
        assert!(
            sidecar(path, "-shm").exists(),
            "WAL-mode fixture must leave a -shm sidecar"
        );
    }

    fn write_delete_mode_db(path: &Path) {
        let conn = rusqlite::Connection::open(path).expect("open fixture db");
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE t(x INTEGER);
             INSERT INTO t VALUES (1);",
        )
        .expect("seed delete-mode fixture");
        drop(conn);
        assert!(!sidecar(path, "-wal").exists());
        assert!(!sidecar(path, "-shm").exists());
    }

    fn chmod(path: &Path, mode: u32) {
        let mut perms = fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(mode);
        fs::set_permissions(path, perms).expect("chmod");
    }

    fn checkpointed_globs(dir: &Path, basename: &str) -> Vec<PathBuf> {
        let prefix = format!("{basename}.checkpointed.");
        let mut out = Vec::new();
        for ent in fs::read_dir(dir).expect("read_dir").flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && name.ends_with(".db") && !name.contains(".incomplete") {
                out.push(ent.path());
            }
        }
        out
    }

    #[test]
    fn wal_copy_fail_returns_error_without_usable_destination() {
        // Deterministic inject: replace -wal with a directory so copy fails
        // without depending on chmod/privilege behavior.
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_wal_mode_db(&db);
        let wal = sidecar(&db, "-wal");
        fs::remove_file(&wal).expect("remove wal file");
        fs::create_dir(&wal).expect("wal path as directory");

        let action = checkpoint_wal_copy(db.to_str().unwrap());

        let _ = fs::remove_dir(&wal); // allow tempdir cleanup
        assert_eq!(action.outcome, "error");
        assert!(
            action.note.contains("copy wal:"),
            "note must name wal copy failure, got: {}",
            action.note
        );
        assert!(
            action.destination.is_none(),
            "must not advertise a usable destination"
        );
        assert!(
            checkpointed_globs(dir.path(), "memory.db").is_empty(),
            "no usable .checkpointed.*.db may remain"
        );
    }

    #[test]
    fn wal_copy_fail_chmod_denied_still_discards() {
        // Retain privilege-path coverage alongside the directory inject.
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_wal_mode_db(&db);
        let wal = sidecar(&db, "-wal");
        chmod(&wal, 0o000);

        let action = checkpoint_wal_copy(db.to_str().unwrap());

        chmod(&wal, 0o644); // allow tempdir cleanup
        assert_eq!(action.outcome, "error");
        assert!(action.destination.is_none());
        assert!(checkpointed_globs(dir.path(), "memory.db").is_empty());
    }

    #[test]
    fn open_failure_discards_destination_and_returns_none() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        // Non-SQLite bytes copy fine but open_for_wal_checkpoint fails.
        fs::write(&db, b"this is not a sqlite database file!!!!!").unwrap();
        fs::write(sidecar(&db, "-wal"), b"wal-bytes").unwrap();

        let action = checkpoint_wal_copy(db.to_str().unwrap());

        assert_eq!(action.outcome, "error");
        // rusqlite may fail at open or at the first pragma on non-DB bytes;
        // either arm must discard and return destination: None.
        assert!(
            action.note.contains("open copy:")
                || action.note.contains("wal_checkpoint failed on copy:"),
            "note must name open/checkpoint failure, got: {}",
            action.note
        );
        assert!(
            action.note.contains("shm="),
            "failure note must retain shm token, got: {}",
            action.note
        );
        assert!(
            action.destination.is_none(),
            "must not advertise a usable destination on open/checkpoint failure"
        );
        assert!(
            checkpointed_globs(dir.path(), "memory.db").is_empty(),
            "no usable .checkpointed.*.db may remain after open/checkpoint failure"
        );
    }

    #[test]
    fn failed_checkpoint_does_not_clobber_prior_success() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_wal_mode_db(&db);

        let first = checkpoint_wal_copy(db.to_str().unwrap());
        assert_eq!(first.outcome, "ok");
        let first_dest = PathBuf::from(first.destination.expect("first success dest"));
        assert!(first_dest.exists());
        let first_bytes = fs::read(&first_dest).expect("read first checkpoint");

        // Force a later mid-copy WAL failure (same second is fine — names collide
        // no longer; discard must not delete the earlier success).
        let wal = sidecar(&db, "-wal");
        fs::remove_file(&wal).expect("remove wal");
        fs::create_dir(&wal).expect("wal as directory");

        let second = checkpoint_wal_copy(db.to_str().unwrap());
        let _ = fs::remove_dir(&wal);

        assert_eq!(second.outcome, "error");
        assert!(second.destination.is_none());
        assert!(
            first_dest.exists(),
            "later WAL-copy failure must not delete prior success at {}",
            first_dest.display()
        );
        assert_eq!(
            fs::read(&first_dest).expect("re-read first"),
            first_bytes,
            "prior success contents must be intact"
        );
        let remaining = checkpointed_globs(dir.path(), "memory.db");
        assert!(
            remaining.iter().any(|p| p == &first_dest),
            "prior success must remain a usable .checkpointed.*.db, got {remaining:?}"
        );
    }

    #[test]
    fn wal_copy_ok_checkpoint_succeeds() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_wal_mode_db(&db);

        let action = checkpoint_wal_copy(db.to_str().unwrap());

        assert_eq!(action.outcome, "ok");
        let dest = action.destination.expect("usable destination");
        assert!(Path::new(&dest).exists());
        assert!(
            dest.contains(".checkpointed.") && dest.ends_with(".db"),
            "timestamped checkpoint naming preserved: {dest}"
        );
        // Collision-resistant stamp: fractional seconds + short hex suffix.
        let name = Path::new(&dest)
            .file_name()
            .unwrap()
            .to_string_lossy();
        assert!(
            name.contains('-'),
            "dest name should include uuid suffix separator: {name}"
        );
        assert!(
            action.note.contains("shm=ok") || action.note.contains("shm=absent"),
            "success note must include shm token, got: {}",
            action.note
        );
    }

    #[test]
    fn no_sidecars_checkpoint_succeeds() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_delete_mode_db(&db);

        let action = checkpoint_wal_copy(db.to_str().unwrap());

        assert_eq!(action.outcome, "ok");
        assert!(action.destination.is_some());
        assert!(
            action.note.contains("shm=absent"),
            "expected shm=absent, got: {}",
            action.note
        );
    }

    #[test]
    fn shm_copy_fail_allows_success_with_required_note() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_wal_mode_db(&db);
        let shm = sidecar(&db, "-shm");
        assert!(shm.exists(), "WAL-mode fixture should create -shm");
        chmod(&shm, 0o000);

        let action = checkpoint_wal_copy(db.to_str().unwrap());

        chmod(&shm, 0o644);
        assert_eq!(action.outcome, "ok");
        assert!(action.destination.is_some());
        assert!(
            action.note.contains("shm=copy_failed_proceeded_without"),
            "required shm failure token missing from note: {}",
            action.note
        );
        let dest = PathBuf::from(action.destination.unwrap());
        assert!(
            !sidecar(&dest, "-shm").exists(),
            "partial shm dest must be deleted after copy failure"
        );
    }

    #[test]
    fn gc_no_sidecars_is_clean_and_idempotent() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("memory.db");
        fs::write(&src, b"main").unwrap();
        for i in 0..4 {
            let p = dir
                .path()
                .join(format!("memory.db.checkpointed.2026010{i}T000000Z.db"));
            fs::write(&p, format!("copy{i}")).unwrap();
        }

        let note = gc_old_checkpoint_copies(&src, 3);
        assert!(
            note.is_none(),
            "absent sidecars must not produce gc failure notes: {note:?}"
        );
        let remaining = checkpointed_globs(dir.path(), "memory.db");
        assert_eq!(remaining.len(), 3);

        let note2 = gc_old_checkpoint_copies(&src, 3);
        assert!(note2.is_none(), "second gc run must stay clean: {note2:?}");
        assert_eq!(checkpointed_globs(dir.path(), "memory.db").len(), 3);
    }

    #[test]
    fn gc_sidecar_delete_denied_reports_path_and_error() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("memory.db");
        fs::write(&src, b"main").unwrap();
        let cp = dir
            .path()
            .join("memory.db.checkpointed.20260101T000000Z.db");
        fs::write(&cp, b"copy").unwrap();
        // Directory at the -wal path → remove_file fails with a non-NotFound error.
        let wal_side = sidecar(&cp, "-wal");
        fs::create_dir(&wal_side).unwrap();

        let note = gc_old_checkpoint_copies(&src, 0).expect("must report sidecar delete error");
        assert!(
            note.contains(wal_side.to_string_lossy().as_ref()),
            "gc note must include sidecar path, got: {note}"
        );
        assert!(
            note.contains("rm "),
            "gc note must include rm error framing, got: {note}"
        );
    }

    #[test]
    fn gc_second_run_after_cleanup_is_clean() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("memory.db");
        fs::write(&src, b"main").unwrap();
        for i in 0..5 {
            let p = dir
                .path()
                .join(format!("memory.db.checkpointed.2026020{i}T000000Z.db"));
            fs::write(&p, b"c").unwrap();
            fs::write(sidecar(&p, "-wal"), b"w").unwrap();
            fs::write(sidecar(&p, "-shm"), b"s").unwrap();
        }

        let first = gc_old_checkpoint_copies(&src, 2);
        assert!(first.is_none(), "successful sidecar deletes: {first:?}");
        assert_eq!(checkpointed_globs(dir.path(), "memory.db").len(), 2);

        let second = gc_old_checkpoint_copies(&src, 2);
        assert!(second.is_none(), "second run must be clean: {second:?}");
    }

    #[test]
    fn stuck_failed_dest_skips_gc_preserving_prior_successes() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_wal_mode_db(&db);

        let mut priors = Vec::new();
        for _ in 0..3 {
            let action = checkpoint_wal_copy(db.to_str().unwrap());
            assert_eq!(action.outcome, "ok");
            let dest = PathBuf::from(action.destination.expect("success dest"));
            assert!(dest.exists());
            priors.push(dest);
        }
        assert_eq!(checkpointed_globs(dir.path(), "memory.db").len(), 3);

        struct ClearStuckInject;
        impl Drop for ClearStuckInject {
            fn drop(&mut self) {
                INJECT_STUCK_FAILED_DEST.set(false);
            }
        }
        INJECT_STUCK_FAILED_DEST.set(true);
        let _clear = ClearStuckInject;
        let failed = checkpoint_wal_copy(db.to_str().unwrap());
        drop(_clear);

        assert_eq!(failed.outcome, "error");
        assert!(failed.destination.is_none());
        assert!(
            failed.note.contains("incomplete remains"),
            "failure note must record stuck cleanup, got: {}",
            failed.note
        );

        for prior in &priors {
            assert!(
                prior.exists(),
                "stuck failed dest must not let GC cull prior success {}",
                prior.display()
            );
        }
        let remaining = checkpointed_globs(dir.path(), "memory.db");
        assert!(
            remaining.len() >= 3,
            "all three prior successes must survive; got {remaining:?}"
        );
        for prior in &priors {
            assert!(
                remaining.iter().any(|p| p == prior),
                "prior {} missing from remaining {remaining:?}",
                prior.display()
            );
        }
        // Allow tempdir cleanup of the stuck leftover.
        for p in remaining {
            if !priors.iter().any(|prior| prior == &p) {
                let _ = fs::remove_file(&p);
            }
        }
    }

    #[test]
    fn checkpoint_dest_preserves_source_mode_bits() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_delete_mode_db(&db);
        chmod(&db, 0o600);

        let action = checkpoint_wal_copy(db.to_str().unwrap());

        assert_eq!(action.outcome, "ok");
        let dest = PathBuf::from(action.destination.expect("success dest"));
        let mode = fs::metadata(&dest).expect("dest metadata").permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "checkpoint dest must preserve source mode bits (not world-readable)"
        );
        assert_eq!(
            mode & 0o044,
            0,
            "checkpoint dest must not be group/world-readable"
        );
    }

    /// Sanity check that the real (uninjected) probe resolves an ordinary
    /// tempdir fixture — nobody else has it open — to `NotOwned`, so the
    /// `Owned`/`Unknown` tests below are meaningfully exercising the
    /// fail-closed branches and not just always hitting the same arm.
    #[test]
    fn not_owned_probe_falls_through_to_copy() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_delete_mode_db(&db);

        assert_eq!(daemon_ownership(&db), DbOwnership::NotOwned);

        let action = checkpoint_wal_copy(db.to_str().unwrap());
        assert_eq!(action.outcome, "ok");
        assert!(action.destination.is_some());
    }

    struct ClearOwnershipInject;
    impl Drop for ClearOwnershipInject {
        fn drop(&mut self) {
            INJECT_OWNERSHIP.with(|c| *c.borrow_mut() = None);
        }
    }

    #[test]
    fn owned_daemon_skips_without_touching_disk() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_delete_mode_db(&db);

        let _clear = ClearOwnershipInject;
        INJECT_OWNERSHIP.with(|c| *c.borrow_mut() = Some(DbOwnership::Owned));

        let action = checkpoint_wal_copy(db.to_str().unwrap());

        assert_eq!(action.outcome, "skipped");
        assert!(action.destination.is_none());
        assert!(
            action.note.contains("live daemon holds this DB"),
            "note must name the live-daemon skip reason, got: {}",
            action.note
        );
        assert!(
            checkpointed_globs(dir.path(), "memory.db").is_empty(),
            "an owned DB must produce zero .checkpointed.*.db copies"
        );
    }

    #[test]
    fn unknown_ownership_fails_closed_without_touching_disk() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_delete_mode_db(&db);

        let _clear = ClearOwnershipInject;
        INJECT_OWNERSHIP.with(|c| {
            *c.borrow_mut() = Some(DbOwnership::Unknown(
                "lsof unavailable: No such file or directory (os error 2)".to_string(),
            ))
        });

        let action = checkpoint_wal_copy(db.to_str().unwrap());

        assert_eq!(
            action.outcome, "skipped",
            "undeterminable ownership must fail closed (skip), not fall through to copy"
        );
        assert!(
            action.destination.is_none(),
            "must never advertise a destination when ownership could not be determined"
        );
        assert!(
            action.note.contains("daemon ownership undetermined"),
            "note must say ownership was undetermined, got: {}",
            action.note
        );
        assert!(
            action.note.contains("lsof unavailable"),
            "note must surface the underlying reason (拒必有声), got: {}",
            action.note
        );
        assert!(
            checkpointed_globs(dir.path(), "memory.db").is_empty(),
            "unknown ownership must produce zero .checkpointed.*.db copies — \
             never a potentially torn destination"
        );
    }

    #[test]
    fn unknown_ownership_note_is_distinct_from_owned_note() {
        // Guard against the two fail-closed branches being conflated into a
        // single "not owned"-shaped note (the bug this change fixes): an
        // undeterminable daemon must be surfaced differently than a
        // confirmed-live one, even though both skip.
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        write_delete_mode_db(&db);

        let _clear = ClearOwnershipInject;
        INJECT_OWNERSHIP.with(|c| {
            *c.borrow_mut() = Some(DbOwnership::Unknown("canonicalize failed: x".into()))
        });
        let unknown_action = checkpoint_wal_copy(db.to_str().unwrap());

        INJECT_OWNERSHIP.with(|c| *c.borrow_mut() = Some(DbOwnership::Owned));
        let owned_action = checkpoint_wal_copy(db.to_str().unwrap());

        assert_eq!(unknown_action.outcome, "skipped");
        assert_eq!(owned_action.outcome, "skipped");
        assert_ne!(
            unknown_action.note, owned_action.note,
            "Unknown and Owned must not collapse into the same receipt note"
        );
    }
}

use crate::daemon_lock::{DualDaemonLock, DualLockError};
use crate::db_ownership::{daemon_ownership, DbOwnership};
use memcore::store::sticky_cutover::{
    StickyCutoverPlan, StickyCutoverReceipt, StickyCutoverReceiptPhase,
};
use memcore::MemoryStore;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::{A2aAction, StickyCutoverAction};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
std::thread_local! {
    static FAULT_DURING_PREPARED_STAGE: Cell<bool> = const { Cell::new(false) };
    static FAULT_AFTER_PREPARED_PUBLISH: Cell<bool> = const { Cell::new(false) };
    static FAULT_AFTER_CORE_COMMIT: Cell<bool> = const { Cell::new(false) };
    static FAULT_REPLACE_PUBLIC_RECEIPT: Cell<bool> = const { Cell::new(false) };
    static FAULT_REPLACE_DB_PATH: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
fn set_fault(cell: &'static std::thread::LocalKey<Cell<bool>>, enabled: bool) {
    cell.with(|value| value.set(enabled));
}

fn fault_after_prepared_publish() -> Result<(), memcore::MemoryError> {
    #[cfg(test)]
    if FAULT_AFTER_PREPARED_PUBLISH.with(|fault| fault.replace(false)) {
        return Err(memcore::MemoryError::InvalidArg(
            "injected post-publication/pre-commit failure".to_string(),
        ));
    }
    Ok(())
}

fn fault_after_core_commit() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(test)]
    if FAULT_AFTER_CORE_COMMIT.with(|fault| fault.replace(false)) {
        return Err("injected post-commit/pre-finalization failure".into());
    }
    Ok(())
}

fn fault_replace_public_receipt(_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(test)]
    if FAULT_REPLACE_PUBLIC_RECEIPT.with(|fault| fault.replace(false)) {
        let displaced = _path.with_extension("displaced-prepared");
        fs::rename(_path, &displaced)?;
        fs::write(_path, b"concurrent replacement")?;
    }
    Ok(())
}

fn fault_replace_db_path(_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(test)]
    if FAULT_REPLACE_DB_PATH.with(|fault| fault.replace(false)) {
        let displaced = _path.with_extension("displaced-db");
        fs::rename(_path, &displaced)?;
        fs::copy(&displaced, _path)?;
    }
    Ok(())
}

pub(super) fn run(
    global_db_path: &Path,
    app_home: &Path,
    action: A2aAction,
) -> Result<(), Box<dyn std::error::Error>> {
    let A2aAction::StickyCutover { action } = action;
    match action {
        StickyCutoverAction::Plan => plan(global_db_path),
        StickyCutoverAction::Apply { plan, confirm } => {
            apply(global_db_path, app_home, &plan, confirm)
        }
    }
}

fn plan(global_db_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let target = canonical_existing_db(global_db_path)?;
    let identity = target.to_string_lossy().into_owned();
    let store = MemoryStore::open_read_only(&identity)?;
    let as_of = chrono::Utc::now().to_rfc3339();
    let plan = store.plan_sticky_cutover_at(identity, &as_of, |body| {
        crate::memory_search_ops::scrub_generated_memory_text(body)
    })?;
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(())
}

fn apply(
    global_db_path: &Path,
    app_home: &Path,
    plan_path: &Path,
    confirm: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !confirm {
        return Err("sticky cutover apply requires --confirm".into());
    }
    let target = canonical_existing_db(global_db_path)?;
    let plan: StickyCutoverPlan = serde_json::from_slice(&fs::read(plan_path)?)?;
    plan.validate()?;
    if fs::canonicalize(&plan.target_db_identity)? != target {
        return Err("sticky cutover plan target DB mismatch".into());
    }
    let receipt_path = receipt_path_for_plan(plan_path);
    if receipt_path == target || fs::canonicalize(&receipt_path).is_ok_and(|path| path == target) {
        return Err("sticky cutover receipt sidecar must not be the target DB".into());
    }

    let _lock = match DualDaemonLock::acquire(app_home, &target) {
        Ok(lock) => lock,
        Err(DualLockError::ScopedRunning { pid }) => {
            return Err(
                format!("sticky cutover apply refused: daemon pid {pid} holds scoped lock").into(),
            )
        }
        Err(DualLockError::LegacyRunning { pid }) => {
            return Err(
                format!("sticky cutover apply refused: daemon pid {pid} holds legacy lock").into(),
            )
        }
        Err(DualLockError::Io(error)) => {
            return Err(format!("sticky cutover daemon ownership unknown: {error}").into())
        }
    };
    match daemon_ownership(&target) {
        DbOwnership::NotOwned => {}
        DbOwnership::Owned => {
            return Err("sticky cutover apply refused: target DB is owned by a live daemon".into())
        }
        DbOwnership::Unknown(reason) => {
            return Err(format!(
                "sticky cutover apply refused: target DB ownership unknown: {reason}"
            )
            .into())
        }
    }

    let existing_receipt = read_existing_receipt(&receipt_path)?;
    let mut store = MemoryStore::open_existing_read_write(&target.to_string_lossy())?;
    store.verify_opened_physical_db_identity(&target)?;
    let mut prepared_file = match existing_receipt.as_ref() {
        Some(receipt) if receipt.phase == StickyCutoverReceiptPhase::Prepared => {
            Some(OpenOptions::new().read(true).open(&receipt_path)?)
        }
        _ => None,
    };
    let mut published = existing_receipt.clone();
    fault_replace_db_path(&target)?;
    let result = store.apply_sticky_cutover_with_precommit_receipt(
        &plan,
        |body| crate::memory_search_ops::scrub_generated_memory_text(body),
        |result| {
            let expected = &result.receipt;
            if let Some(existing) = published.as_ref() {
                if existing != expected {
                    return Err(memcore::MemoryError::InvalidArg(
                        "sticky cutover receipt sidecar conflicts with this plan/application"
                            .to_string(),
                    ));
                }
                return Ok(());
            }
            prepared_file = Some(persist_prepared_receipt(&receipt_path, expected)?);
            published = Some(expected.clone());
            fault_after_prepared_publish()
        },
    )?;
    store.verify_opened_physical_db_identity(&target)?;

    fault_after_core_commit()?;

    match existing_receipt.as_ref() {
        Some(existing) if existing.phase == StickyCutoverReceiptPhase::Committed => {
            if *existing != result.receipt || !result.replayed {
                return Err(
                    "sticky cutover committed sidecar does not match DB replay state".into(),
                );
            }
        }
        _ => {
            let prepared_file = prepared_file.ok_or_else(|| {
                "sticky cutover committed without a durable prepared receipt handle".to_string()
            })?;
            fault_replace_public_receipt(&receipt_path)?;
            finalize_prepared_receipt(&receipt_path, &prepared_file, &result.receipt)?;
        }
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn canonical_existing_db(path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "sticky cutover target is not a regular file: {}",
            path.display()
        )
        .into());
    }
    Ok(fs::canonicalize(path)?)
}

fn receipt_path_for_plan(plan: &Path) -> PathBuf {
    let mut receipt = plan.as_os_str().to_owned();
    receipt.push(".receipt");
    PathBuf::from(receipt)
}

fn read_existing_receipt(
    path: &Path,
) -> Result<Option<StickyCutoverReceipt>, Box<dyn std::error::Error>> {
    match fs::read(path) {
        Ok(bytes) => {
            let receipt: StickyCutoverReceipt = serde_json::from_slice(&bytes)?;
            receipt.validate()?;
            Ok(Some(receipt))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn sync_parent(path: &Path) -> std::io::Result<()> {
    File::open(path.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()
}

fn write_staged_receipt(
    public_path: &Path,
    phase: &str,
    receipt: &StickyCutoverReceipt,
) -> Result<(PathBuf, File), std::io::Error> {
    let parent = public_path.parent().unwrap_or_else(|| Path::new("."));
    let name = public_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "receipt".into());
    let bytes = serde_json::to_vec_pretty(receipt).map_err(std::io::Error::other)?;
    for attempt in 0..32 {
        let staged = parent.join(format!(
            ".{name}.sticky-cutover-{phase}-{}-{attempt}",
            uuid::Uuid::new_v4()
        ));
        let mut file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&staged)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = (|| {
            #[cfg(test)]
            if phase == "prepared" && FAULT_DURING_PREPARED_STAGE.with(|fault| fault.replace(false))
            {
                file.write_all(&bytes[..bytes.len() / 2])?;
                return Err(std::io::Error::other(
                    "injected partial prepared receipt write failure",
                ));
            }
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()
        })() {
            drop(file);
            let _ = fs::remove_file(&staged);
            return Err(error);
        }
        return Ok((staged, file));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate sticky cutover receipt staging path",
    ))
}

fn persist_prepared_receipt(
    receipt_path: &Path,
    receipt: &StickyCutoverReceipt,
) -> Result<File, memcore::MemoryError> {
    let (staged, file) = write_staged_receipt(receipt_path, "prepared", receipt)
        .map_err(|error| memcore::MemoryError::InvalidArg(error.to_string()))?;
    if let Err(error) = fs::hard_link(&staged, receipt_path) {
        let _ = fs::remove_file(&staged);
        return Err(memcore::MemoryError::InvalidArg(format!(
            "sticky cutover prepared receipt publication failed: {error}"
        )));
    }
    sync_parent(receipt_path).map_err(|error| {
        memcore::MemoryError::InvalidArg(format!(
            "sticky cutover prepared receipt parent sync failed: {error}; prepared artifact retained"
        ))
    })?;
    if fs::remove_file(&staged).is_ok() {
        let _ = sync_parent(receipt_path);
    }
    Ok(file)
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn atomic_exchange_paths(left: &Path, right: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let left = CString::new(left.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "receipt path contains NUL",
        )
    })?;
    let right = CString::new(right.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "receipt path contains NUL",
        )
    })?;
    let rc = unsafe {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP)
        }
        #[cfg(target_os = "linux")]
        {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                left.as_ptr(),
                libc::AT_FDCWD,
                right.as_ptr(),
                libc::RENAME_EXCHANGE,
            ) as i32
        }
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
fn atomic_exchange_paths(_left: &Path, _right: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic receipt exchange is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn path_names_open_file(path: &Path, file: &File) -> std::io::Result<bool> {
    let path_metadata = fs::symlink_metadata(path)?;
    let file_metadata = file.metadata()?;
    Ok(path_metadata.file_type().is_file()
        && path_metadata.dev() == file_metadata.dev()
        && path_metadata.ino() == file_metadata.ino())
}

#[cfg(not(unix))]
fn path_names_open_file(_path: &Path, _file: &File) -> std::io::Result<bool> {
    Ok(false)
}

fn finalize_prepared_receipt(
    receipt_path: &Path,
    prepared_file: &File,
    committed: &StickyCutoverReceipt,
) -> Result<(), Box<dyn std::error::Error>> {
    let (staged, committed_file) = write_staged_receipt(receipt_path, "committed", committed)?;
    if let Err(error) = atomic_exchange_paths(&staged, receipt_path) {
        let _ = fs::remove_file(&staged);
        return Err(format!("sticky cutover committed receipt exchange failed: {error}").into());
    }
    if !path_names_open_file(&staged, prepared_file)? {
        let displaced = OpenOptions::new().read(true).open(&staged)?;
        atomic_exchange_paths(&staged, receipt_path)?;
        let restored = path_names_open_file(receipt_path, &displaced)?
            && path_names_open_file(&staged, &committed_file)?;
        return Err(if restored {
            format!(
                "sticky cutover public receipt was replaced; replacement restored and committed staging retained at {}",
                staged.display()
            )
            .into()
        } else {
            format!(
                "sticky cutover receipt changed during finalization; artifacts retained at {} and {}",
                receipt_path.display(),
                staged.display()
            )
            .into()
        });
    }
    sync_parent(receipt_path)?;
    eprintln!(
        "sticky cutover retained immutable prepared receipt backup at {}",
        staged.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_plan_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let app_home = dir.path().join("app");
        std::fs::create_dir_all(&app_home).unwrap();
        let identity = db.to_string_lossy().into_owned();
        let store = MemoryStore::open(&identity).unwrap();
        let plan = store
            .plan_sticky_cutover_at(identity, "2026-08-13T00:00:00Z", str::to_string)
            .unwrap();
        drop(store);
        let plan_path = dir.path().join("plan.json");
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();
        (dir, db, app_home, plan_path)
    }

    #[test]
    fn sticky_cutover_receipt_uses_a_deterministic_sidecar() {
        let plan = Path::new("/tmp/sticky-cutover-plan.json");
        assert_eq!(
            receipt_path_for_plan(plan),
            Path::new("/tmp/sticky-cutover-plan.json.receipt")
        );
    }

    #[test]
    fn sticky_cutover_receipt_artifact_is_body_free() {
        let source = include_str!("../../../memcore/src/store/sticky_cutover.rs");
        assert!(!source.contains("pub body:"));
        assert!(source.contains("source_body_digest"));
        assert!(source.contains("envelope_body_digest"));
    }

    #[test]
    fn sticky_cutover_apply_publishes_committed_sidecar_and_replays() {
        let (_dir, db, app_home, plan_path) = empty_plan_fixture();

        apply(&db, &app_home, &plan_path, true).unwrap();
        let receipt_path = receipt_path_for_plan(&plan_path);
        let first = read_existing_receipt(&receipt_path).unwrap().unwrap();
        assert_eq!(first.phase, StickyCutoverReceiptPhase::Committed);
        assert!(first.rows.is_empty());

        apply(&db, &app_home, &plan_path, true).unwrap();
        let replay = read_existing_receipt(&receipt_path).unwrap().unwrap();
        assert_eq!(replay, first);
    }

    #[test]
    fn sticky_cutover_prepared_stage_failure_rolls_back_and_can_retry() {
        let (_dir, db, app_home, plan_path) = empty_plan_fixture();
        set_fault(&FAULT_DURING_PREPARED_STAGE, true);
        let error = apply(&db, &app_home, &plan_path, true).unwrap_err();
        assert!(error.to_string().contains("partial prepared"));
        assert!(!receipt_path_for_plan(&plan_path).exists());

        apply(&db, &app_home, &plan_path, true).unwrap();
        let receipt = read_existing_receipt(&receipt_path_for_plan(&plan_path))
            .unwrap()
            .unwrap();
        assert_eq!(receipt.phase, StickyCutoverReceiptPhase::Committed);
    }

    #[test]
    fn sticky_cutover_post_publication_failure_rolls_back_with_prepared_recovery() {
        let (_dir, db, app_home, plan_path) = empty_plan_fixture();
        set_fault(&FAULT_AFTER_PREPARED_PUBLISH, true);
        let error = apply(&db, &app_home, &plan_path, true).unwrap_err();
        assert!(error.to_string().contains("post-publication/pre-commit"));
        let receipt_path = receipt_path_for_plan(&plan_path);
        let prepared = read_existing_receipt(&receipt_path).unwrap().unwrap();
        assert_eq!(prepared.phase, StickyCutoverReceiptPhase::Prepared);

        let store = MemoryStore::open_read_only(db.to_str().unwrap()).unwrap();
        let replay_plan: StickyCutoverPlan =
            serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
        let second = store
            .plan_sticky_cutover_at(
                replay_plan.target_db_identity.clone(),
                &replay_plan.as_of,
                str::to_string,
            )
            .unwrap();
        assert_eq!(second.plan_digest, replay_plan.plan_digest);
        drop(store);

        apply(&db, &app_home, &plan_path, true).unwrap();
        let committed = read_existing_receipt(&receipt_path).unwrap().unwrap();
        assert_eq!(committed.phase, StickyCutoverReceiptPhase::Committed);
        assert_eq!(committed.plan_digest, prepared.plan_digest);
    }

    #[test]
    fn sticky_cutover_post_commit_failure_recovers_prepared_sidecar() {
        let (_dir, db, app_home, plan_path) = empty_plan_fixture();
        set_fault(&FAULT_AFTER_CORE_COMMIT, true);
        let error = apply(&db, &app_home, &plan_path, true).unwrap_err();
        assert!(error.to_string().contains("post-commit/pre-finalization"));
        let receipt_path = receipt_path_for_plan(&plan_path);
        let prepared = read_existing_receipt(&receipt_path).unwrap().unwrap();
        assert_eq!(prepared.phase, StickyCutoverReceiptPhase::Prepared);

        apply(&db, &app_home, &plan_path, true).unwrap();
        let committed = read_existing_receipt(&receipt_path).unwrap().unwrap();
        assert_eq!(committed.phase, StickyCutoverReceiptPhase::Committed);
        assert_eq!(committed.plan_digest, prepared.plan_digest);
        assert_eq!(committed.decision_digest, prepared.decision_digest);
    }

    #[test]
    fn sticky_cutover_replacement_is_restored_without_false_success() {
        let (dir, db, app_home, plan_path) = empty_plan_fixture();
        set_fault(&FAULT_REPLACE_PUBLIC_RECEIPT, true);
        let error = apply(&db, &app_home, &plan_path, true).unwrap_err();
        assert!(error.to_string().contains("replacement restored"));
        let receipt_path = receipt_path_for_plan(&plan_path);
        assert_eq!(
            std::fs::read(&receipt_path).unwrap(),
            b"concurrent replacement"
        );
        assert!(dir
            .path()
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("sticky-cutover-committed")));
    }

    #[test]
    fn sticky_cutover_valid_committed_mismatch_refuses_replay() {
        let (_dir, db, app_home, plan_path) = empty_plan_fixture();
        apply(&db, &app_home, &plan_path, true).unwrap();
        let receipt_path = receipt_path_for_plan(&plan_path);
        let original = read_existing_receipt(&receipt_path).unwrap().unwrap();
        let mut different = original.clone();
        different.decision_digest = "0".repeat(64);
        different.receipt_digest.clear();
        different.receipt_digest = different.compute_digest().unwrap();
        different.validate().unwrap();
        std::fs::write(
            &receipt_path,
            serde_json::to_vec_pretty(&different).unwrap(),
        )
        .unwrap();

        let error = apply(&db, &app_home, &plan_path, true).unwrap_err();
        assert!(error.to_string().contains("does not match DB replay state"));
        assert_eq!(
            read_existing_receipt(&receipt_path).unwrap(),
            Some(different)
        );
    }

    #[test]
    fn sticky_cutover_db_path_replacement_fails_before_receipt_publication() {
        let (dir, db, app_home, plan_path) = empty_plan_fixture();
        set_fault(&FAULT_REPLACE_DB_PATH, true);
        let error = apply(&db, &app_home, &plan_path, true).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("path identity changed after open"),
            "unexpected replacement refusal: {error}"
        );
        assert!(!receipt_path_for_plan(&plan_path).exists());
        assert!(dir.path().join("memory.displaced-db").exists());
    }

    #[test]
    fn sticky_cutover_apply_requires_confirmation_before_opening_the_plan() {
        let error = apply(
            Path::new("/definitely/missing.db"),
            Path::new("/definitely/missing-home"),
            Path::new("/definitely/missing-plan.json"),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires --confirm"));
    }
}

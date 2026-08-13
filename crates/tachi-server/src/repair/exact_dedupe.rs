use crate::daemon_lock::{DualDaemonLock, DualLockError};
use crate::db_ownership::{daemon_ownership, DbOwnership};
use crate::manifest::{DbRole, Manifest, MANIFEST_SCHEMA_VERSION};
use memcore::store::exact_dedupe::{
    ExactDedupePlan, ExactDedupeReceipt, ExactDedupeReceiptDbState, ExactDedupeRestoreReceipt,
};
use memcore::MemoryStore;
#[cfg(test)]
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
std::thread_local! {
    static FAULT_AFTER_DB_COMMIT_BEFORE_FINALIZATION: Cell<bool> = const { Cell::new(false) };
    static FAULT_AFTER_PREPARED_PUBLISH_BEFORE_COMMIT: Cell<bool> = const { Cell::new(false) };
    static FAULT_REPLACE_PUBLIC_RECEIPT_BEFORE_FINALIZATION: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
fn inject_fault_after_db_commit_before_finalization_for_test(enabled: bool) {
    FAULT_AFTER_DB_COMMIT_BEFORE_FINALIZATION.with(|fault| fault.set(enabled));
}

#[cfg(test)]
fn inject_fault_during_prepared_stage_write_for_test(enabled: bool) {
    super::receipt::inject_fault_during_prepared_stage_write_for_test(enabled);
}

#[cfg(test)]
fn inject_fault_before_prepared_parent_sync_for_test(enabled: bool) {
    super::receipt::inject_fault_before_prepared_parent_sync_for_test(enabled);
}

#[cfg(test)]
fn inject_fault_after_prepared_publish_before_commit_for_test(enabled: bool) {
    FAULT_AFTER_PREPARED_PUBLISH_BEFORE_COMMIT.with(|fault| fault.set(enabled));
}

#[cfg(test)]
fn inject_fault_replace_public_receipt_before_finalization_for_test(enabled: bool) {
    FAULT_REPLACE_PUBLIC_RECEIPT_BEFORE_FINALIZATION.with(|fault| fault.set(enabled));
}

fn fault_after_db_commit_before_finalization() -> Result<(), &'static str> {
    #[cfg(test)]
    if FAULT_AFTER_DB_COMMIT_BEFORE_FINALIZATION.with(|fault| fault.replace(false)) {
        return Err("injected post-commit/pre-finalization fault");
    }
    Ok(())
}

fn fault_after_prepared_publish_before_commit() -> Result<(), memcore::MemoryError> {
    #[cfg(test)]
    if FAULT_AFTER_PREPARED_PUBLISH_BEFORE_COMMIT.with(|fault| fault.replace(false)) {
        return Err(memcore::MemoryError::InvalidArg(
            "injected post-publication/pre-commit failure".to_string(),
        ));
    }
    Ok(())
}

fn fault_replace_public_receipt_before_finalization(_receipt_out: &Path) -> Result<(), String> {
    #[cfg(test)]
    if FAULT_REPLACE_PUBLIC_RECEIPT_BEFORE_FINALIZATION.with(|fault| fault.replace(false)) {
        let displaced = _receipt_out.with_extension("displaced-prepared");
        fs::rename(_receipt_out, &displaced)
            .map_err(|error| format!("displace prepared receipt fixture: {error}"))?;
        fs::write(_receipt_out, b"concurrent replacement")
            .map_err(|error| format!("install receipt replacement fixture: {error}"))?;
    }
    Ok(())
}

fn persist_prepared_receipt_before_commit(
    receipt_out: &Path,
    result: &memcore::store::exact_dedupe::ExactDedupeApplyResult,
) -> Result<File, memcore::MemoryError> {
    let receipt_json = serde_json::to_vec_pretty(&result.receipt)?;
    super::receipt::persist_prepared_artifact_bytes(
        receipt_out,
        super::receipt::ReceiptKind::ExactDedupe,
        &receipt_json,
    )
    .map_err(|error| {
        let detail = match error {
            super::receipt::PreparedArtifactError::Stage(error) => format!(
                "exact-dedupe prepared receipt could not be staged durably at {}: {error}",
                receipt_out.display()
            ),
            super::receipt::PreparedArtifactError::AlreadyExists => format!(
                "exact-dedupe receipt output already exists: {}",
                receipt_out.display()
            ),
            super::receipt::PreparedArtifactError::Publish(error) => format!(
                "exact-dedupe prepared receipt could not be atomically published before commit at {}: {error}",
                receipt_out.display()
            ),
            super::receipt::PreparedArtifactError::RecoveryAlreadyExists => format!(
                "exact-dedupe receipt protocol unexpectedly requested a recovery link at {}",
                receipt_out.display()
            ),
            super::receipt::PreparedArtifactError::RecoveryPublish(error) => format!(
                "exact-dedupe receipt protocol unexpectedly failed a recovery link at {}: {error}",
                receipt_out.display()
            ),
            super::receipt::PreparedArtifactError::ParentSync(error) => format!(
                "exact-dedupe prepared receipt parent could not be synced durably before commit at {}: {error}; the public prepared artifact was retained conservatively",
                receipt_out.display()
            ),
        };
        memcore::MemoryError::InvalidArg(detail)
    })
}

fn reconcile_failed_prepared_receipt(
    store: &MemoryStore,
    receipt_out: &Path,
    receipt: &ExactDedupeReceipt,
    apply_error: Box<dyn std::error::Error>,
) -> Box<dyn std::error::Error> {
    let original = apply_error.to_string();
    let db_state = match store.classify_exact_dedupe_receipt_db_state(receipt) {
        Ok(state) => state,
        Err(error) => {
            return format!(
                "exact-dedupe apply failed after prepared receipt publication: {original}; DB outcome could not be reconciled ({error}), so the prepared receipt was retained at {}",
                receipt_out.display()
            )
            .into()
        }
    };
    match db_state {
        ExactDedupeReceiptDbState::Applied => format!(
            "exact-dedupe apply returned an error after prepared receipt publication: {original}; the committed DB post-state was proven, so the recovery receipt was retained at {}",
            receipt_out.display()
        )
        .into(),
        ExactDedupeReceiptDbState::Indeterminate => format!(
            "exact-dedupe apply failed after prepared receipt publication: {original}; the DB outcome is indeterminate, so the prepared receipt was retained at {}",
            receipt_out.display()
        )
        .into(),
        ExactDedupeReceiptDbState::NotApplied => {
            format!(
                "exact-dedupe apply failed after prepared receipt publication: {original}; DB rollback was proven and the prepared receipt was retained at {} because path-based cleanup cannot safely delete a concurrently replaced object",
                receipt_out.display()
            )
            .into()
        }
    }
}

fn run_physical_identity_checked_exact_dedupe<T>(
    store: &mut MemoryStore,
    target: &Path,
    operation: &str,
    action: impl FnOnce(&mut MemoryStore) -> Result<T, memcore::MemoryError>,
) -> Result<(T, Option<String>), Box<dyn std::error::Error>> {
    store
        .verify_opened_physical_db_identity(target)
        .map_err(|error| {
            format!(
                "exact-dedupe {operation} physical identity check failed before operation: {error}"
            )
        })?;
    let result = action(store);
    let post = store
        .verify_opened_physical_db_identity(target)
        .map_err(|error| {
            format!(
                "exact-dedupe {operation} physical identity check failed after operation: {error}"
            )
        });
    match (result, post) {
        (Ok(value), Ok(())) => Ok((value, None)),
        (Ok(value), Err(identity_error)) => Ok((value, Some(identity_error))),
        (Err(error), Ok(())) => Err(error.into()),
        (Err(error), Err(identity_error)) => Err(format!(
            "{error}; additionally, the physical identity invariant failed after exact-dedupe {operation}: {identity_error}"
        )
        .into()),
    }
}

pub(super) fn target_and_daemon_scope(
    operation: &str,
    db: &str,
    app_home: &Path,
    require_write: bool,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    target_and_daemon_scope_with_inventory_policy(operation, db, app_home, require_write, false)
}

pub(super) fn manifest_target_and_daemon_scope(
    operation: &str,
    db: &str,
    app_home: &Path,
    require_write: bool,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    target_and_daemon_scope_with_inventory_policy(operation, db, app_home, require_write, true)
}

fn target_and_daemon_scope_with_inventory_policy(
    operation: &str,
    db: &str,
    app_home: &Path,
    require_write: bool,
    manifest_only: bool,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let manifest_path = app_home.join("manifest.json");
    let manifest = Manifest::load(&manifest_path)?;
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(format!(
            "unsupported manifest schema version {} at {}",
            manifest.schema_version,
            manifest_path.display()
        )
        .into());
    }
    let entry = if manifest_only {
        super::inventory::resolve_manifest_one(&manifest, db)?
    } else {
        super::inventory::resolve_one(&manifest, db)?
    }
    .ok_or_else(|| format!("--db '{db}' did not resolve to exactly one manifest DB"))?;
    crate::path_utils::manifest_db_leaf_exists(&entry)?;
    let target = std::fs::canonicalize(entry.path)?;
    let authorized = manifest
        .dbs
        .iter()
        .find(|entry| std::fs::canonicalize(&entry.path).is_ok_and(|path| path == target))
        .ok_or_else(|| {
            format!(
                "{operation} target {} is not authorized by manifest",
                target.display()
            )
        })?;
    if authorized.schema_kind != "tachi" {
        return Err(format!(
            "{operation} target {} is not a Tachi-schema manifest DB",
            target.display()
        )
        .into());
    }
    if require_write && !authorized.allow_write {
        return Err(format!(
            "{operation} target {} is not writable by manifest authority",
            target.display()
        )
        .into());
    }
    let daemon_scope = manifest
        .dbs
        .iter()
        .find(|entry| entry.role == DbRole::Global)
        .map(|entry| std::fs::canonicalize(&entry.path))
        .transpose()?
        .unwrap_or_else(|| target.clone());
    Ok((target, daemon_scope))
}

pub fn plan(
    db: &str,
    output: &Path,
    limit: Option<usize>,
    prefix: Option<&str>,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (target, _) = target_and_daemon_scope("exact-dedupe", db, app_home, false)?;
    if output == target || std::fs::canonicalize(output).is_ok_and(|path| path == target) {
        return Err("exact-dedupe output must not be the target DB".into());
    }
    let identity = target.to_string_lossy().into_owned();
    let store = MemoryStore::open_read_only(&identity)?;
    let report = store.plan_exact_dedupe(identity, limit, prefix)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(serde_json::to_string_pretty(&report)?.as_bytes())?;
    file.write_all(b"\n")?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub fn apply(
    db: &str,
    plan_path: &Path,
    yes: bool,
    receipt_out: &Path,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !yes {
        return Err("exact-dedupe apply requires --yes".into());
    }
    let (target, daemon_scope) = target_and_daemon_scope("exact-dedupe", db, app_home, true)?;
    if receipt_out == target || std::fs::canonicalize(receipt_out).is_ok_and(|path| path == target)
    {
        return Err("exact-dedupe receipt output must not be the target DB".into());
    }
    let plan: ExactDedupePlan = serde_json::from_slice(&std::fs::read(plan_path)?)?;
    plan.validate()?;
    if std::fs::canonicalize(&plan.target_db_identity)? != target {
        return Err("exact-dedupe plan target DB mismatch".into());
    }
    let _lock = match DualDaemonLock::acquire(app_home, &daemon_scope) {
        Ok(lock) => lock,
        Err(DualLockError::ScopedRunning { pid }) => {
            return Err(
                format!("exact-dedupe apply refused: daemon pid {pid} holds scoped lock").into(),
            )
        }
        Err(DualLockError::LegacyRunning { pid }) => {
            return Err(
                format!("exact-dedupe apply refused: daemon pid {pid} holds legacy lock").into(),
            )
        }
        Err(DualLockError::Io(error)) => {
            return Err(format!("exact-dedupe daemon ownership unknown: {error}").into())
        }
    };
    match daemon_ownership(&target) {
        DbOwnership::NotOwned => {}
        DbOwnership::Owned => {
            return Err("exact-dedupe apply refused: target DB is owned by a live daemon".into())
        }
        DbOwnership::Unknown(reason) => {
            return Err(format!(
                "exact-dedupe apply refused: target DB ownership unknown: {reason}"
            )
            .into())
        }
    }
    if std::fs::symlink_metadata(receipt_out).is_ok() {
        return Err(format!(
            "exact-dedupe receipt output already exists: {}",
            receipt_out.display()
        )
        .into());
    }
    let mut store = MemoryStore::open_existing_read_write(&target.to_string_lossy())?;
    let mut published_prepared_receipt = None;
    let mut prepared_receipt_file = None;
    let apply_attempt =
        run_physical_identity_checked_exact_dedupe(&mut store, &target, "apply", |store| {
            store.apply_exact_dedupe_with_precommit_receipt(&plan, |result| {
                prepared_receipt_file =
                    Some(persist_prepared_receipt_before_commit(receipt_out, result)?);
                published_prepared_receipt = Some(result.receipt.clone());
                fault_after_prepared_publish_before_commit()
            })
        });
    let (result, post_identity_error) = match apply_attempt {
        Ok(result) => result,
        Err(error) => {
            if let Some(receipt) = published_prepared_receipt.as_ref() {
                return Err(reconcile_failed_prepared_receipt(
                    &store,
                    receipt_out,
                    receipt,
                    error,
                ));
            }
            return Err(error);
        }
    };
    if let Some(identity_error) = post_identity_error {
        eprintln!(
            "CRITICAL: exact-dedupe apply wrote and synced the receipt at {} before commit, then committed {} archived loser(s) to a database handle whose target path identity changed: {identity_error}",
            receipt_out.display(),
            result.applied_losers,
        );
        return Err(identity_error.into());
    }
    if let Err(error) = fault_after_db_commit_before_finalization() {
        return Err(format!(
            "exact-dedupe apply committed {} archived loser(s), then {error}; durable prepared recovery receipt retained at {}",
            result.applied_losers,
            receipt_out.display()
        )
        .into());
    }
    fault_replace_public_receipt_before_finalization(receipt_out)?;
    let prepared_receipt_file = prepared_receipt_file.ok_or_else(|| {
        "exact-dedupe apply committed without retaining its prepared receipt object handle"
            .to_string()
    })?;
    let committed_receipt_bytes = serde_json::to_vec_pretty(&result.receipt)?;
    let prepared_backup = match super::receipt::finalize_prepared_receipt(
        receipt_out,
        super::receipt::ReceiptKind::ExactDedupe,
        &prepared_receipt_file,
        &committed_receipt_bytes,
    ) {
        Ok(path) => path,
        Err(error) => {
            return Err(format!(
                "exact-dedupe apply committed {} archived loser(s), but receipt finalization failed: {error}; commit-boundary artifacts were retained for recovery",
                result.applied_losers,
            )
            .into())
        }
    };
    eprintln!(
        "exact-dedupe retained the immutable prepared receipt backup at {}",
        prepared_backup.display()
    );
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

pub fn restore(
    db: &str,
    receipt_path: &Path,
    yes: bool,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !yes {
        return Err("exact-dedupe restore requires --yes".into());
    }
    let (target, daemon_scope) = target_and_daemon_scope("exact-dedupe", db, app_home, true)?;
    let receipt = ExactDedupeRestoreReceipt::from_slice(&std::fs::read(receipt_path)?)?;
    receipt.validate()?;
    if std::fs::canonicalize(receipt.target_db_identity())? != target {
        return Err("exact-dedupe receipt target DB mismatch".into());
    }
    let _lock = match DualDaemonLock::acquire(app_home, &daemon_scope) {
        Ok(lock) => lock,
        Err(DualLockError::ScopedRunning { pid }) => {
            return Err(
                format!("exact-dedupe restore refused: daemon pid {pid} holds scoped lock").into(),
            )
        }
        Err(DualLockError::LegacyRunning { pid }) => {
            return Err(
                format!("exact-dedupe restore refused: daemon pid {pid} holds legacy lock").into(),
            )
        }
        Err(DualLockError::Io(error)) => {
            return Err(format!("exact-dedupe daemon ownership unknown: {error}").into())
        }
    };
    match daemon_ownership(&target) {
        DbOwnership::NotOwned => {}
        DbOwnership::Owned => {
            return Err("exact-dedupe restore refused: target DB is owned by a live daemon".into())
        }
        DbOwnership::Unknown(reason) => {
            return Err(format!(
                "exact-dedupe restore refused: target DB ownership unknown: {reason}"
            )
            .into())
        }
    }
    let mut store = MemoryStore::open_existing_read_write(&target.to_string_lossy())?;
    let (result, post_identity_error) =
        run_physical_identity_checked_exact_dedupe(&mut store, &target, "restore", |store| {
            store.restore_exact_dedupe_versioned(&receipt)
        })?;
    if let Some(identity_error) = post_identity_error {
        eprintln!(
            "CRITICAL: exact-dedupe restore committed against a database handle whose target path identity changed: {identity_error}"
        );
        return Err(identity_error.into());
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_lock::{legacy_daemon_lock_path, scoped_daemon_lock_path, DaemonLock};
    #[cfg(unix)]
    use crate::db_ownership::set_ownership_inject_for_test;
    use crate::manifest::DbEntry;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let app_home = dir.path().join(".tachi");
        std::fs::create_dir_all(&app_home).unwrap();
        let db_path = dir.path().join(memcore::MEMORY_DB_FILENAME);
        let mut store = MemoryStore::open(&db_path.to_string_lossy()).unwrap();
        for id in ["winner", "loser"] {
            store
                .insert_if_absent(&memcore::MemoryEntry {
                    id: id.into(),
                    path: "/same".into(),
                    summary: String::new(),
                    text: "duplicate".into(),
                    importance: 0.7,
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "fact".into(),
                    topic: String::new(),
                    keywords: Vec::new(),
                    persons: Vec::new(),
                    entities: Vec::new(),
                    location: String::new(),
                    source: "test".into(),
                    scope: "general".into(),
                    archived: false,
                    access_count: 0,
                    scored_count: 0,
                    last_access: None,
                    last_use_at: None,
                    revision: 1,
                    vector: None,
                    retention_policy: (id == "winner").then_some("permanent".into()),
                    domain: None,
                    metadata: serde_json::json!({}),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".into(),
                })
                .unwrap();
        }
        let identity = std::fs::canonicalize(&db_path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let dedupe_plan = store
            .plan_exact_dedupe(identity.clone(), None, None)
            .unwrap();
        assert_eq!(
            dedupe_plan.planned_losers, 1,
            "fixture must seed one intentional exact duplicate"
        );
        let plan_path = dir.path().join("plan.json");
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&dedupe_plan).unwrap()).unwrap();
        let mut manifest = Manifest::empty();
        manifest.dbs.push(DbEntry {
            path: identity,
            role: DbRole::Project,
            owner: "tachi".into(),
            schema_kind: "tachi".into(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".into(),
            scope_hint: "project:test".into(),
            notes: String::new(),
        });
        manifest.save(&app_home.join("manifest.json")).unwrap();
        let receipt_path = dir.path().join("receipt.json");
        (dir, app_home, db_path, plan_path, receipt_path)
    }

    #[cfg(unix)]
    #[test]
    fn exact_dedupe_identity_guard_rejects_replacement_before_action() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.db");
        let replacement = dir.path().join("replacement.db");
        let mut store = MemoryStore::open(target.to_str().unwrap()).unwrap();
        drop(MemoryStore::open(replacement.to_str().unwrap()).unwrap());
        std::fs::rename(&replacement, &target).unwrap();
        let mut called = false;

        let error =
            run_physical_identity_checked_exact_dedupe(&mut store, &target, "apply", |_| {
                called = true;
                Ok(())
            })
            .expect_err("detached handle must fail before mutation");

        assert!(!called);
        assert!(error.to_string().contains("before operation"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn exact_dedupe_identity_guard_reports_replacement_during_action() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.db");
        let replacement = dir.path().join("replacement.db");
        let mut store = MemoryStore::open(target.to_str().unwrap()).unwrap();
        drop(MemoryStore::open(replacement.to_str().unwrap()).unwrap());

        let ((), post_error) =
            run_physical_identity_checked_exact_dedupe(&mut store, &target, "restore", |_| {
                std::fs::rename(&replacement, &target)
                    .map_err(|error| memcore::MemoryError::InvalidArg(error.to_string()))?;
                Ok(())
            })
            .expect("post-operation identity drift must preserve the action result for recovery");

        assert!(post_error
            .expect("replacement must be reported")
            .contains("after operation"));
    }

    #[test]
    fn plan_refuses_existing_output_and_preserves_first_plan() {
        let (_dir, app_home, db_path, _plan_path, _receipt_path) = fixture();
        let output = app_home.join("cli-plan.json");
        plan(&db_path.to_string_lossy(), &output, None, None, &app_home).unwrap();
        let first = std::fs::read(&output).unwrap();
        let error = plan(&db_path.to_string_lossy(), &output, None, None, &app_home).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(output).unwrap(), first);
    }

    #[test]
    fn plan_refuses_target_database_as_output_without_changing_it() {
        let (_dir, app_home, db_path, _plan_path, _receipt_path) = fixture();
        let before = std::fs::read(&db_path).unwrap();
        let error = plan(&db_path.to_string_lossy(), &db_path, None, None, &app_home).unwrap_err();
        assert!(
            error.to_string().contains("must not be the target DB"),
            "unexpected refusal: {error}"
        );
        assert_eq!(std::fs::read(db_path).unwrap(), before);
    }

    #[test]
    #[cfg(unix)]
    fn plan_refuses_manifest_project_symlink_without_following_foreign_db() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, app_home, foreign_db, _plan_path, _receipt_path) = fixture();
        let project_link = app_home.join("project-link.db");
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
        let output = app_home.join("plan-output.json");

        let error = plan("project:linked", &output, None, None, &app_home)
            .expect_err("manifest project symlink must refuse exact-dedupe");

        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "unexpected refusal: {error}"
        );
        assert!(!output.exists());
        let link_after = std::fs::symlink_metadata(&project_link).unwrap();
        assert_eq!((link_after.dev(), link_after.ino()), link_identity);
        assert_eq!(std::fs::read_link(&project_link).unwrap(), foreign_db);
        let foreign_after = std::fs::symlink_metadata(&foreign_db).unwrap();
        assert_eq!((foreign_after.dev(), foreign_after.ino()), foreign_identity);
        assert_eq!(std::fs::read(&foreign_db).unwrap(), foreign_before);
    }

    #[test]
    #[cfg(unix)]
    fn plan_refuses_dangling_manifest_project_symlink() {
        use std::os::unix::fs::MetadataExt;

        let (dir, app_home, _db_path, _plan_path, _receipt_path) = fixture();
        let missing_target = dir.path().join("missing-external.db");
        let project_link = app_home.join("dangling-project.db");
        std::os::unix::fs::symlink(&missing_target, &project_link)
            .expect("dangling project symlink");
        let mut manifest = Manifest::load(&app_home.join("manifest.json")).unwrap();
        manifest.dbs[0].path = project_link.to_string_lossy().into_owned();
        manifest.dbs[0].scope_hint = "project:dangling".into();
        manifest.save(&app_home.join("manifest.json")).unwrap();
        let link_metadata = std::fs::symlink_metadata(&project_link).unwrap();
        let link_identity = (link_metadata.dev(), link_metadata.ino());
        let output = app_home.join("plan-output.json");

        let error = plan("project:dangling", &output, None, None, &app_home)
            .expect_err("dangling manifest project symlink must refuse exact-dedupe");

        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "unexpected refusal: {error}"
        );
        assert!(!output.exists());
        let link_after = std::fs::symlink_metadata(&project_link).unwrap();
        assert_eq!((link_after.dev(), link_after.ino()), link_identity);
        assert_eq!(std::fs::read_link(&project_link).unwrap(), missing_target);
        assert!(std::fs::symlink_metadata(&missing_target).is_err());
    }

    #[test]
    fn apply_requires_yes_before_target_resolution() {
        let dir = tempfile::tempdir().unwrap();
        assert!(apply(
            "missing",
            Path::new("missing"),
            false,
            Path::new("missing-receipt"),
            dir.path()
        )
        .unwrap_err()
        .to_string()
        .contains("requires --yes"));
    }

    #[test]
    fn apply_refuses_malformed_manifest_without_mutation() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        std::fs::write(app_home.join("manifest.json"), b"not json").unwrap();

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(error.to_string().contains("manifest"));
        let archived: i64 = rusqlite::Connection::open(db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[test]
    fn apply_refuses_unregistered_target_without_mutation() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        Manifest::empty()
            .save(&app_home.join("manifest.json"))
            .unwrap();

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not authorized by manifest"));
        let archived: i64 = rusqlite::Connection::open(db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[test]
    fn read_only_plan_allows_manifest_read_only_target_but_apply_refuses_it() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        let manifest_path = app_home.join("manifest.json");
        let mut manifest = Manifest::load(&manifest_path).unwrap();
        manifest.dbs[0].allow_write = false;
        manifest.save(&manifest_path).unwrap();

        let output = app_home.join("read-only-plan.json");
        plan(&db_path.to_string_lossy(), &output, None, None, &app_home).unwrap();
        assert!(output.exists());

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("not writable by manifest authority"));
        let archived: i64 = rusqlite::Connection::open(db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[test]
    fn apply_refuses_non_tachi_manifest_target_without_mutation() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        let manifest_path = app_home.join("manifest.json");
        let mut manifest = Manifest::load(&manifest_path).unwrap();
        manifest.dbs[0].schema_kind = "openclaw_legacy".into();
        manifest.save(&manifest_path).unwrap();

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not a Tachi-schema manifest DB"));
        let archived: i64 = rusqlite::Connection::open(db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[test]
    fn apply_refuses_scoped_daemon_lock() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        let _holder = DaemonLock::acquire(scoped_daemon_lock_path(&app_home, &db_path))
            .expect("hold scoped daemon lock");
        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(error.to_string().contains("holds scoped lock"));
    }

    #[test]
    fn apply_refuses_legacy_daemon_lock() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        let _holder = DaemonLock::acquire(legacy_daemon_lock_path(&app_home))
            .expect("hold legacy daemon lock");
        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(error.to_string().contains("holds legacy lock"));
    }

    #[test]
    fn project_apply_refuses_manifest_global_scoped_lock_without_mutation() {
        let (_dir, app_home, project_path, plan_path, receipt_path) = fixture();
        let global_path = app_home.join("global.db");
        MemoryStore::open(&global_path.to_string_lossy()).unwrap();
        let mut manifest = Manifest::empty();
        let entry = |path: &Path, role, scope: &str| DbEntry {
            path: std::fs::canonicalize(path)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            role,
            owner: "tachi".into(),
            schema_kind: "tachi".into(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".into(),
            scope_hint: scope.into(),
            notes: String::new(),
        };
        manifest.dbs = vec![
            entry(&global_path, DbRole::Global, "global"),
            entry(&project_path, DbRole::Project, "project:test"),
        ];
        manifest.save(&app_home.join("manifest.json")).unwrap();
        let before_bytes = std::fs::read(&project_path).unwrap();
        let before_archived: i64 = rusqlite::Connection::open(&project_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        let _holder = DaemonLock::acquire(scoped_daemon_lock_path(&app_home, &global_path))
            .expect("hold manifest-global scoped daemon lock");

        let error = apply("project:test", &plan_path, true, &receipt_path, &app_home).unwrap_err();
        assert!(error.to_string().contains("holds scoped lock"));
        assert_eq!(std::fs::read(&project_path).unwrap(), before_bytes);
        let after_archived: i64 = rusqlite::Connection::open(&project_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after_archived, before_archived);
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_injected_owned_before_rw_open() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::Owned));
        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(error.to_string().contains("owned by a live daemon"));
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_injected_unknown_before_rw_open() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::Unknown("injected".into())));
        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(error.to_string().contains("ownership unknown: injected"));
    }

    #[cfg(unix)]
    #[test]
    fn apply_succeeds_without_recreating_missing_memories_vec() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("DROP TABLE memories_vec", []).unwrap();
        drop(conn);
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));

        apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let (archived, superseded_by): (i64, Option<String>) = conn
            .query_row(
                "SELECT archived,superseded_by FROM memories WHERE id='loser'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(archived, 1);
        assert_eq!(superseded_by.as_deref(), Some("winner"));
        let table_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='memories_vec'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn apply_writes_a_durable_receipt_that_restore_consumes() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));

        apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .expect("apply plan");

        let receipt: memcore::store::exact_dedupe::ExactDedupeReceipt =
            serde_json::from_slice(&std::fs::read(&receipt_path).expect("receipt file exists"))
                .expect("receipt file is valid JSON");
        receipt.validate().expect("receipt is self-consistent");
        assert_eq!(
            receipt.phase,
            memcore::store::exact_dedupe::ExactDedupeReceiptPhase::Committed
        );
        assert_eq!(receipt.rows.len(), 1);
        assert_eq!(receipt.rows[0].loser_id, "loser");
        assert!(
            receipt.target_db_physical_identity.starts_with("unix:"),
            "receipt must bind the physical DB identity, got {}",
            receipt.target_db_physical_identity
        );

        let archived_before: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT archived FROM memories WHERE id='loser'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(archived_before, 1);

        restore(&db_path.to_string_lossy(), &receipt_path, true, &app_home).expect("restore");

        let archived_after: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT archived FROM memories WHERE id='loser'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(archived_after, 0);
    }

    #[cfg(unix)]
    #[test]
    fn post_commit_fault_retains_an_honest_prepared_receipt_that_can_restore() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        inject_fault_after_db_commit_before_finalization_for_test(true);

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .expect_err("inject crash window after DB commit");
        assert!(
            error
                .to_string()
                .contains("post-commit/pre-finalization fault"),
            "unexpected failure: {error}"
        );
        let receipt: ExactDedupeReceipt = serde_json::from_slice(
            &std::fs::read(&receipt_path).expect("prepared receipt remains durable"),
        )
        .expect("prepared receipt is valid JSON");
        assert_eq!(
            receipt.phase,
            memcore::store::exact_dedupe::ExactDedupeReceiptPhase::Prepared
        );
        receipt.validate().expect("prepared receipt remains valid");
        let archived: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT archived FROM memories WHERE id='loser'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archived, 1, "DB transaction committed before the fault");

        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        restore(&db_path.to_string_lossy(), &receipt_path, true, &app_home)
            .expect("prepared receipt restores only after proving committed post-state");
        let archived: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT archived FROM memories WHERE id='loser'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_when_receipt_output_already_exists_without_mutation() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        std::fs::write(&receipt_path, b"pre-existing").unwrap();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("receipt output already exists"),
            "unexpected refusal: {error}"
        );
        let archived: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
        assert_eq!(std::fs::read(&receipt_path).unwrap(), b"pre-existing");
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_missing_receipt_parent_without_committing_mutation() {
        let (dir, app_home, db_path, plan_path, _receipt_path) = fixture();
        let receipt_path = dir.path().join("missing-parent").join("receipt.json");
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("could not be staged durably"),
            "unexpected refusal: {error}"
        );
        let archived: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
        assert!(!receipt_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn partial_prepared_stage_write_never_publishes_or_leaks_a_receipt() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        inject_fault_during_prepared_stage_write_for_test(true);

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .expect_err("partial staged write must abort before DB commit");
        assert!(
            error
                .to_string()
                .contains("injected partial prepared-receipt write failure"),
            "unexpected refusal: {error}"
        );
        assert!(
            !receipt_path.exists(),
            "partial bytes must never appear at the public receipt path"
        );
        let leaked_stage = std::fs::read_dir(receipt_path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("exact-dedupe-prepared")
            });
        assert!(!leaked_stage, "partial staging file must be removed");
        let archived: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[cfg(unix)]
    #[test]
    fn prepared_parent_sync_failure_retains_public_evidence_without_committing() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        inject_fault_before_prepared_parent_sync_for_test(true);

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .expect_err("unsynced public receipt must abort before DB commit");
        assert!(
            error
                .to_string()
                .contains("injected prepared-receipt parent sync failure"),
            "unexpected refusal: {error}"
        );
        let retained: ExactDedupeReceipt = serde_json::from_slice(
            &std::fs::read(&receipt_path).expect("prepared receipt remains inspectable"),
        )
        .expect("retained receipt JSON");
        assert_eq!(
            retained.phase,
            memcore::store::exact_dedupe::ExactDedupeReceiptPhase::Prepared
        );
        let leaked_stage = std::fs::read_dir(receipt_path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("exact-dedupe-prepared")
            });
        assert!(!leaked_stage, "prepared staging name must be removed");
        let archived: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0, "DB mutation must roll back with publication");
    }

    #[cfg(unix)]
    #[test]
    fn post_publication_precommit_failure_proves_rollback_and_retains_prepared_evidence() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        inject_fault_after_prepared_publish_before_commit_for_test(true);

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .expect_err("failure after publication must reconcile the DB outcome");
        let message = error.to_string();
        assert!(
            message.contains("injected post-publication/pre-commit failure")
                && message.contains("DB rollback was proven")
                && message.contains("prepared receipt was retained"),
            "unexpected reconciliation error: {message}"
        );
        let retained: ExactDedupeReceipt = serde_json::from_slice(
            &std::fs::read(&receipt_path).expect("prepared receipt remains as evidence"),
        )
        .expect("retained receipt JSON");
        assert_eq!(
            retained.phase,
            memcore::store::exact_dedupe::ExactDedupeReceiptPhase::Prepared
        );
        let archived: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0, "the pre-commit DB transaction must roll back");
        let lineage: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM exact_dedupe_apply_lineage",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lineage, 0, "rollback must leave no durable apply lineage");
    }

    #[cfg(unix)]
    #[test]
    fn finalization_never_overwrites_a_concurrent_public_path_replacement() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        inject_fault_replace_public_receipt_before_finalization_for_test(true);

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .expect_err("replacement must make finalization fail closed");
        assert!(
            error
                .to_string()
                .contains("replacement was restored without overwrite"),
            "unexpected finalization error: {error}"
        );
        assert_eq!(
            std::fs::read(&receipt_path).expect("replacement remains public"),
            b"concurrent replacement"
        );
        let archived: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            archived, 1,
            "DB commit remains truthful despite finalization refusal"
        );
        let displaced = receipt_path.with_extension("displaced-prepared");
        let prepared: ExactDedupeReceipt = serde_json::from_slice(
            &std::fs::read(displaced).expect("original prepared receipt remains available"),
        )
        .expect("displaced prepared receipt JSON");
        assert_eq!(
            prepared.phase,
            memcore::store::exact_dedupe::ExactDedupeReceiptPhase::Prepared
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_leaves_no_receipt_on_precommit_core_failure() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        let mut store = MemoryStore::open_existing_read_write(&db_path.to_string_lossy()).unwrap();
        store
            .insert_if_absent(&memcore::MemoryEntry {
                id: "drifter".into(),
                path: "/same".into(),
                summary: String::new(),
                text: "duplicate".into(),
                importance: 0.7,
                timestamp: "2026-01-01T00:00:00Z".into(),
                valid_from: String::new(),
                valid_until: None,
                category: "fact".into(),
                topic: String::new(),
                keywords: Vec::new(),
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                source: "test".into(),
                scope: "general".into(),
                archived: false,
                access_count: 0,
                scored_count: 0,
                last_access: None,
                last_use_at: None,
                revision: 1,
                vector: None,
                retention_policy: None,
                domain: None,
                metadata: serde_json::json!({}),
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".into(),
            })
            .unwrap();
        drop(store);
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));

        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();

        assert!(
            error.to_string().contains("group membership drifted"),
            "unexpected refusal: {error}"
        );
        assert!(
            !receipt_path.exists(),
            "precommit core failure must not create the final receipt"
        );
        let archived: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[test]
    fn restore_requires_yes_before_target_resolution() {
        let dir = tempfile::tempdir().unwrap();
        assert!(restore("missing", Path::new("missing"), false, dir.path())
            .unwrap_err()
            .to_string()
            .contains("requires --yes"));
    }

    #[cfg(unix)]
    #[test]
    fn restore_refuses_receipt_for_a_different_target_database() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .expect("apply plan");

        let (_other_dir, other_app_home, other_db_path, _other_plan_path, _other_receipt_path) =
            fixture();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        let error = restore(
            &other_db_path.to_string_lossy(),
            &receipt_path,
            true,
            &other_app_home,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("target DB mismatch"),
            "unexpected refusal: {error}"
        );
        let archived: i64 = rusqlite::Connection::open(&other_db_path)
            .unwrap()
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }
}

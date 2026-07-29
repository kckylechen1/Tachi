use crate::daemon_lock::{DualDaemonLock, DualLockError};
use crate::db_ownership::{daemon_ownership, DbOwnership};
use memcore::store::lifecycle_consistency::{
    LifecycleConsistencyPlan, LifecycleConsistencyReceipt,
};
use memcore::MemoryStore;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

#[cfg(test)]
use std::cell::Cell;

const OPERATION: &str = "lifecycle-consistency";

#[cfg(test)]
std::thread_local! {
    static FAULT_AFTER_DB_COMMIT_BEFORE_FINALIZATION: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
fn inject_fault_after_db_commit_before_finalization_for_test(enabled: bool) {
    FAULT_AFTER_DB_COMMIT_BEFORE_FINALIZATION.with(|fault| fault.set(enabled));
}

fn fault_after_db_commit_before_finalization() -> Result<(), &'static str> {
    #[cfg(test)]
    if FAULT_AFTER_DB_COMMIT_BEFORE_FINALIZATION.with(|fault| fault.replace(false)) {
        return Err("injected post-commit/pre-finalization fault");
    }
    Ok(())
}

fn mutation_guard(
    target: &Path,
    daemon_scope: &Path,
    app_home: &Path,
) -> Result<DualDaemonLock, Box<dyn std::error::Error>> {
    let lock = match DualDaemonLock::acquire(app_home, daemon_scope) {
        Ok(lock) => lock,
        Err(DualLockError::ScopedRunning { pid }) => {
            return Err(
                format!("{OPERATION} mutation refused: daemon pid {pid} holds scoped lock").into(),
            )
        }
        Err(DualLockError::LegacyRunning { pid }) => {
            return Err(
                format!("{OPERATION} mutation refused: daemon pid {pid} holds legacy lock").into(),
            )
        }
        Err(DualLockError::Io(error)) => {
            return Err(format!("{OPERATION} daemon ownership unknown: {error}").into())
        }
    };
    match daemon_ownership(target) {
        DbOwnership::NotOwned => Ok(lock),
        DbOwnership::Owned => {
            Err(format!("{OPERATION} mutation refused: target DB is owned by a live daemon").into())
        }
        DbOwnership::Unknown(reason) => Err(format!(
            "{OPERATION} mutation refused: target DB ownership unknown: {reason}"
        )
        .into()),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn write_new_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    sync_parent_directory(path)
}

fn finalize_prepared_receipt(
    receipt_out: &Path,
    committed_receipt: &LifecycleConsistencyReceipt,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = serde_json::to_vec_pretty(committed_receipt)?;
    let parent = receipt_out.parent().unwrap_or_else(|| Path::new("."));
    let name = receipt_out
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("lifecycle-consistency receipt output requires a UTF-8 filename")?;
    let mut staged = None;
    for attempt in 0..32 {
        let candidate = parent.join(format!(
            ".{name}.lifecycle-committed-{}-{attempt}",
            std::process::id()
        ));
        match write_new_durable(&candidate, &bytes) {
            Ok(()) => {
                staged = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let staged = staged.ok_or("could not allocate lifecycle committed receipt staging path")?;
    fs::rename(&staged, receipt_out)?;
    sync_parent_directory(receipt_out)?;
    Ok(())
}

pub fn render_human_plan(plan: &LifecycleConsistencyPlan) -> String {
    let mut lines = vec![format!(
        "lifecycle-consistency: planned_mutations={} adjudication_required={}",
        plan.planned_mutations, plan.adjudication_count
    )];
    for mutation in &plan.mutations {
        lines.push(format!(
            "mutation kind={:?} row={} related={}",
            mutation.kind, mutation.row_id, mutation.related_row_id
        ));
    }
    for item in &plan.adjudication_required {
        let missing = item
            .missing_target_id
            .as_deref()
            .map(|target| format!(" missing_target={target}"))
            .unwrap_or_default();
        lines.push(format!(
            "adjudication_required kind={:?} rows={}{}",
            item.kind,
            item.row_ids.join(","),
            missing
        ));
    }
    lines.join("\n")
}

pub fn plan(
    db: &str,
    output: &Path,
    json: bool,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (target, _) = super::exact_dedupe::target_and_daemon_scope(OPERATION, db, app_home, false)?;
    if output == target || std::fs::canonicalize(output).is_ok_and(|path| path == target) {
        return Err("lifecycle-consistency output must not be the target DB".into());
    }
    let identity = target.to_string_lossy().into_owned();
    let store = MemoryStore::open_read_only(&identity)?;
    let report = store.plan_lifecycle_consistency(identity)?;
    let serialized = serde_json::to_vec_pretty(&report)?;
    write_new(output, &serialized)?;
    if json {
        println!("{}", String::from_utf8(serialized)?);
    } else {
        println!("{}", render_human_plan(&report));
    }
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
        return Err("lifecycle-consistency apply requires --yes".into());
    }
    if receipt_out.exists() {
        return Err(format!(
            "lifecycle-consistency receipt output already exists: {}",
            receipt_out.display()
        )
        .into());
    }
    let (target, daemon_scope) =
        super::exact_dedupe::target_and_daemon_scope(OPERATION, db, app_home, true)?;
    if receipt_out == target || std::fs::canonicalize(receipt_out).is_ok_and(|path| path == target)
    {
        return Err("lifecycle-consistency receipt output must not be the target DB".into());
    }
    let plan: LifecycleConsistencyPlan = serde_json::from_slice(&std::fs::read(plan_path)?)?;
    plan.validate()?;
    if std::fs::canonicalize(&plan.target_db_identity)? != target {
        return Err("lifecycle-consistency plan target DB mismatch".into());
    }
    let _lock = mutation_guard(&target, &daemon_scope, app_home)?;
    let mut store = MemoryStore::open_existing_read_write(&target.to_string_lossy())?;
    // The prepared receipt is the recoverable record. Its file data and
    // directory entry are fsynced before the transaction can commit; a crash
    // thereafter leaves a restorable `prepared` receipt whose restore path
    // first proves the exact DB post-state with full CAS.
    let prepared_receipt = store.prepare_lifecycle_consistency_receipt(&plan)?;
    let prepared_json = serde_json::to_vec_pretty(&prepared_receipt)?;
    write_new_durable(receipt_out, &prepared_json)?;
    let mut result = match store.apply_prepared_lifecycle_consistency(&plan, &prepared_receipt) {
        Ok(result) => result,
        Err(error) => {
            return Err(format!(
                "lifecycle-consistency apply failed; prepared recovery receipt retained at {} and restore will verify actual DB state before writing: {error}",
                receipt_out.display()
            )
            .into())
        }
    };
    if let Err(error) = fault_after_db_commit_before_finalization() {
        return Err(format!(
            "lifecycle-consistency apply committed {} mutation(s), then {error}; durable prepared recovery receipt retained at {}",
            result.applied_mutations,
            receipt_out.display()
        )
        .into());
    }
    let committed_receipt = result.receipt.clone().into_committed()?;
    if let Err(error) = finalize_prepared_receipt(receipt_out, &committed_receipt) {
        return Err(format!(
            "lifecycle-consistency apply committed {} mutation(s), but finalization failed: {error}; durable prepared recovery receipt remains at {}",
            result.applied_mutations,
            receipt_out.display()
        )
        .into());
    }
    result.receipt = committed_receipt;
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
        return Err("lifecycle-consistency restore requires --yes".into());
    }
    let (target, daemon_scope) =
        super::exact_dedupe::target_and_daemon_scope(OPERATION, db, app_home, true)?;
    let receipt: LifecycleConsistencyReceipt =
        serde_json::from_slice(&std::fs::read(receipt_path)?)?;
    receipt.validate()?;
    if std::fs::canonicalize(&receipt.target_db_identity)? != target {
        return Err("lifecycle-consistency receipt target DB mismatch".into());
    }
    let _lock = mutation_guard(&target, &daemon_scope, app_home)?;
    let mut store = MemoryStore::open_existing_read_write(&target.to_string_lossy())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&store.restore_lifecycle_consistency(&receipt)?)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_lock::{scoped_daemon_lock_path, DaemonLock};
    #[cfg(unix)]
    use crate::db_ownership::set_ownership_inject_for_test;
    use crate::manifest::{DbEntry, DbRole, Manifest};
    use memcore::store::lifecycle_consistency::LifecycleReceiptPhase;
    use memcore::MemoryEntry;
    use std::path::PathBuf;

    fn entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            path: format!("/lifecycle/{id}"),
            summary: String::new(),
            text: format!("private-{id}"),
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
        }
    }

    fn fixture(ambiguous: bool) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let app_home = dir.path().join(".tachi");
        std::fs::create_dir_all(&app_home).unwrap();
        let db_path = dir.path().join(memcore::MEMORY_DB_FILENAME);
        let mut store = MemoryStore::open(&db_path.to_string_lossy()).unwrap();
        for id in ["winner", "loser"] {
            store.insert_if_absent(&entry(id)).unwrap();
        }
        store.supersede_memory("winner", "loser").unwrap();
        store.supersede_memory("loser", "winner").unwrap();
        if !ambiguous {
            store.archive_memory("loser").unwrap();
        }
        let identity = std::fs::canonicalize(&db_path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let lifecycle_plan = store.plan_lifecycle_consistency(identity.clone()).unwrap();
        let plan_path = dir.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::to_vec_pretty(&lifecycle_plan).unwrap(),
        )
        .unwrap();
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

    #[test]
    fn human_and_json_plan_name_ambiguous_cycle_without_content() {
        let (_dir, app_home, db_path, _plan_path, _receipt_path) = fixture(true);
        let output = app_home.join("planned.json");
        plan(&db_path.to_string_lossy(), &output, false, &app_home).unwrap();
        let parsed: LifecycleConsistencyPlan =
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        let human = render_human_plan(&parsed);
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(human.contains("adjudication_required"));
        assert!(human.contains("winner") && human.contains("loser"));
        assert!(json.contains("ambiguous_two_node_cycle"));
        assert!(!human.contains("private-") && !json.contains("private-"));
    }

    #[test]
    fn apply_and_restore_require_yes_before_target_resolution() {
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
        assert!(restore("missing", Path::new("missing"), false, dir.path())
            .unwrap_err()
            .to_string()
            .contains("requires --yes"));
    }

    #[test]
    fn manifest_read_authority_allows_plan_but_write_authority_blocks_apply() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture(false);
        let manifest_path = app_home.join("manifest.json");
        let mut manifest = Manifest::load(&manifest_path).unwrap();
        manifest.dbs[0].allow_write = false;
        manifest.save(&manifest_path).unwrap();
        let output = app_home.join("read-only-plan.json");
        plan(&db_path.to_string_lossy(), &output, true, &app_home).unwrap();
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
        assert!(!receipt_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn apply_writes_durable_receipt_and_restore_consumes_it() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture(false);
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap();
        let receipt: LifecycleConsistencyReceipt =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        receipt.validate().unwrap();
        assert_eq!(receipt.phase, LifecycleReceiptPhase::Committed);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let winner_target: Option<String> = conn
            .query_row(
                "SELECT superseded_by FROM memories WHERE id='winner'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(winner_target.is_none());
        drop(conn);
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        restore(&db_path.to_string_lossy(), &receipt_path, true, &app_home).unwrap();
        let restored: Option<String> = rusqlite::Connection::open(db_path)
            .unwrap()
            .query_row(
                "SELECT superseded_by FROM memories WHERE id='winner'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(restored.as_deref(), Some("loser"));
    }

    #[cfg(unix)]
    #[test]
    fn post_commit_fault_keeps_prepared_receipt_for_deterministic_restore() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture(false);
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        inject_fault_after_db_commit_before_finalization_for_test(true);
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
            .contains("injected post-commit/pre-finalization fault"));

        let prepared: LifecycleConsistencyReceipt =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        prepared.validate().unwrap();
        assert_eq!(prepared.phase, LifecycleReceiptPhase::Prepared);
        let committed_target: Option<String> = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT superseded_by FROM memories WHERE id='winner'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(committed_target.is_none());

        // A prepared record alone is not authority to write: restore first
        // validates this exact committed state, then restores under CAS.
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        restore(&db_path.to_string_lossy(), &receipt_path, true, &app_home).unwrap();
        let restored: Option<String> = rusqlite::Connection::open(db_path)
            .unwrap()
            .query_row(
                "SELECT superseded_by FROM memories WHERE id='winner'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(restored.as_deref(), Some("loser"));
    }

    #[test]
    fn existing_receipt_destination_blocks_apply_before_mutation() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture(false);
        std::fs::write(&receipt_path, b"reserved").unwrap();
        let before: (i64, Option<String>) = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT revision,superseded_by FROM memories WHERE id='winner'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let error = apply(
            &db_path.to_string_lossy(),
            &plan_path,
            true,
            &receipt_path,
            &app_home,
        )
        .unwrap_err();
        assert!(error.to_string().contains("receipt output already exists"));
        let after: (i64, Option<String>) = rusqlite::Connection::open(db_path)
            .unwrap()
            .query_row(
                "SELECT revision,superseded_by FROM memories WHERE id='winner'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(after, before);
    }

    #[test]
    fn apply_refuses_scoped_daemon_lock_before_receipt_or_mutation() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture(false);
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
        assert!(!receipt_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_owned_database_before_receipt_or_mutation() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture(false);
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
        assert!(!receipt_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn apply_fails_closed_when_database_ownership_is_unknown() {
        let (_dir, app_home, db_path, plan_path, receipt_path) = fixture(false);
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
        assert!(!receipt_path.exists());
    }
}

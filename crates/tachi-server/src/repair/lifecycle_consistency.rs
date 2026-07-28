use crate::daemon_lock::{DualDaemonLock, DualLockError};
use crate::db_ownership::{daemon_ownership, DbOwnership};
use memcore::store::lifecycle_consistency::{
    LifecycleConsistencyPlan, LifecycleConsistencyReceipt,
};
use memcore::MemoryStore;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

const OPERATION: &str = "lifecycle-consistency";

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
    // Reserve and prove the receipt destination before the DB transaction.
    // Holding the create-new handle closes the path race that a separate
    // existence check would leave between apply and receipt persistence.
    let mut receipt_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(receipt_out)?;
    let result = match store.apply_lifecycle_consistency(&plan) {
        Ok(result) => result,
        Err(error) => {
            drop(receipt_file);
            let _ = std::fs::remove_file(receipt_out);
            return Err(error.into());
        }
    };
    let receipt_json = serde_json::to_vec_pretty(&result.receipt)?;
    if let Err(error) = receipt_file
        .write_all(&receipt_json)
        .and_then(|()| receipt_file.write_all(b"\n"))
        .and_then(|()| receipt_file.sync_all())
    {
        let printable = String::from_utf8_lossy(&receipt_json);
        eprintln!(
            "CRITICAL: lifecycle-consistency apply committed {} mutation(s) but the receipt could not be durably written to {}: {error}\nReceipt JSON (save for manual `repair lifecycle restore`):\n{printable}",
            result.applied_mutations,
            receipt_out.display(),
        );
        return Err(
            format!("lifecycle-consistency receipt write failed after commit: {error}").into(),
        );
    }
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

use crate::daemon_lock::{DualDaemonLock, DualLockError};
use crate::db_ownership::{daemon_ownership, DbOwnership};
use crate::manifest::{DbRole, Manifest};
use memcore::store::exact_dedupe::ExactDedupePlan;
use memcore::MemoryStore;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

fn target_and_daemon_scope(
    db: &str,
    app_home: &Path,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let manifest_path = if app_home.join("manifest.json").exists() {
        app_home.join("manifest.json")
    } else {
        Manifest::default_path(&dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
    };
    let manifest = Manifest::load_or_empty(&manifest_path);
    let entry = super::inventory::resolve_one(&manifest, db)
        .ok_or_else(|| format!("--db '{db}' did not resolve to exactly one manifest DB"))?;
    let target = std::fs::canonicalize(entry.path)?;
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
    let (target, _) = target_and_daemon_scope(db, app_home)?;
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
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !yes {
        return Err("exact-dedupe apply requires --yes".into());
    }
    let (target, daemon_scope) = target_and_daemon_scope(db, app_home)?;
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
    let mut store = MemoryStore::open_existing_read_write(&target.to_string_lossy())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&store.apply_exact_dedupe(&plan)?)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_lock::{legacy_daemon_lock_path, scoped_daemon_lock_path, DaemonLock};
    #[cfg(unix)]
    use crate::db_ownership::set_ownership_inject_for_test;
    use crate::manifest::DbEntry;
    use rusqlite::params;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let app_home = dir.path().join(".tachi");
        std::fs::create_dir_all(&app_home).unwrap();
        Manifest::empty()
            .save(&app_home.join("manifest.json"))
            .unwrap();
        let db_path = dir.path().join(memcore::MEMORY_DB_FILENAME);
        let store = MemoryStore::open(&db_path.to_string_lossy()).unwrap();
        for id in ["winner", "loser"] {
            store
                .connection()
                .execute(
                    "INSERT INTO memories(id,path,text,timestamp,created_at,updated_at,retention_policy) VALUES(?1,'/same','duplicate','2026-01-01','2026-01-01','2026-01-01',?2)",
                    params![id, (id == "winner").then_some("permanent")],
                )
                .unwrap();
        }
        let identity = std::fs::canonicalize(&db_path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let dedupe_plan = store.plan_exact_dedupe(identity, None, None).unwrap();
        let plan_path = dir.path().join("plan.json");
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&dedupe_plan).unwrap()).unwrap();
        (dir, app_home, db_path, plan_path)
    }

    #[test]
    fn plan_refuses_existing_output_and_preserves_first_plan() {
        let (_dir, app_home, db_path, _plan_path) = fixture();
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
        let (_dir, app_home, db_path, _plan_path) = fixture();
        let before = std::fs::read(&db_path).unwrap();
        let error = plan(&db_path.to_string_lossy(), &db_path, None, None, &app_home).unwrap_err();
        assert!(
            error.to_string().contains("must not be the target DB"),
            "unexpected refusal: {error}"
        );
        assert_eq!(std::fs::read(db_path).unwrap(), before);
    }

    #[test]
    fn apply_requires_yes_before_target_resolution() {
        let dir = tempfile::tempdir().unwrap();
        assert!(apply("missing", Path::new("missing"), false, dir.path())
            .unwrap_err()
            .to_string()
            .contains("requires --yes"));
    }

    #[test]
    fn apply_refuses_scoped_daemon_lock() {
        let (_dir, app_home, db_path, plan_path) = fixture();
        let _holder = DaemonLock::acquire(scoped_daemon_lock_path(&app_home, &db_path))
            .expect("hold scoped daemon lock");
        let error = apply(&db_path.to_string_lossy(), &plan_path, true, &app_home).unwrap_err();
        assert!(error.to_string().contains("holds scoped lock"));
    }

    #[test]
    fn apply_refuses_legacy_daemon_lock() {
        let (_dir, app_home, db_path, plan_path) = fixture();
        let _holder = DaemonLock::acquire(legacy_daemon_lock_path(&app_home))
            .expect("hold legacy daemon lock");
        let error = apply(&db_path.to_string_lossy(), &plan_path, true, &app_home).unwrap_err();
        assert!(error.to_string().contains("holds legacy lock"));
    }

    #[test]
    fn project_apply_refuses_manifest_global_scoped_lock_without_mutation() {
        let (_dir, app_home, project_path, plan_path) = fixture();
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

        let error = apply("project:test", &plan_path, true, &app_home).unwrap_err();
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
        let (_dir, app_home, db_path, plan_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::Owned));
        let error = apply(&db_path.to_string_lossy(), &plan_path, true, &app_home).unwrap_err();
        assert!(error.to_string().contains("owned by a live daemon"));
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_injected_unknown_before_rw_open() {
        let (_dir, app_home, db_path, plan_path) = fixture();
        set_ownership_inject_for_test(Some(DbOwnership::Unknown("injected".into())));
        let error = apply(&db_path.to_string_lossy(), &plan_path, true, &app_home).unwrap_err();
        assert!(error.to_string().contains("ownership unknown: injected"));
    }

    #[cfg(unix)]
    #[test]
    fn apply_succeeds_without_recreating_missing_memories_vec() {
        let (_dir, app_home, db_path, plan_path) = fixture();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("DROP TABLE memories_vec", []).unwrap();
        drop(conn);
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));

        apply(&db_path.to_string_lossy(), &plan_path, true, &app_home).unwrap();

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
}

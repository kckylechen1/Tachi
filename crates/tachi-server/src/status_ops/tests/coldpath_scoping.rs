//! Coldpath perf-pack item 5: `tachi status`'s default probe scope is
//! global DB + current-project DB only; `--all-dbs` restores the full
//! manifest fleet. See `status_ops::snapshot::collect_snapshot_inner`'s doc
//! comment for the mechanism.

use crate::manifest::{DbEntry, DbRole, Manifest};
use crate::status_ops::collect_snapshot_scoped;
use memcore::MemoryStore;

fn entry(path: &std::path::Path, role: DbRole) -> DbEntry {
    DbEntry {
        path: path.to_string_lossy().to_string(),
        role,
        owner: "tachi".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: false,
        allow_write: true,
        last_doctor_at: "1970-01-01T00:00:00Z".to_string(),
        last_classification: "healthy".to_string(),
        scope_hint: "unknown".to_string(),
        notes: String::new(),
    }
}

/// Sets up an app_home with a manifest listing THREE real, openable DBs
/// (global, project, and one unrelated "extra" DB standing in for the rest
/// of a multi-DB fleet), returns (app_home, global_db_path, project_db_path).
fn three_db_fixture(
    dir: &std::path::Path,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let app_home = dir.join("home");
    let global_db = app_home.join("global/memory.db");
    let project_db = dir.join("project/memory.db");
    let extra_db = app_home.join("agents/extra/memory.db");

    for db in [&global_db, &project_db, &extra_db] {
        std::fs::create_dir_all(db.parent().unwrap()).expect("create db parent");
        MemoryStore::open(db.to_str().unwrap()).expect("open db");
    }

    let manifest = Manifest {
        schema_version: crate::manifest::MANIFEST_SCHEMA_VERSION,
        generated_at: chrono::Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![
            entry(&global_db, DbRole::Global),
            entry(&project_db, DbRole::Project),
            entry(&extra_db, DbRole::Agent),
        ],
    };
    let manifest_path = app_home.join("manifest.json");
    manifest.save(&manifest_path).expect("save manifest");

    (app_home, global_db, project_db)
}

#[test]
fn default_scope_probes_only_global_and_project() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (app_home, global_db, project_db) = three_db_fixture(dir.path());

    let snapshot = collect_snapshot_scoped(&app_home, &global_db, Some(&project_db), false);

    assert_eq!(
        snapshot.dbs.len(),
        2,
        "default scope must probe exactly global + project, not the full 3-db fleet: {:?}",
        snapshot.dbs.iter().map(|d| &d.path).collect::<Vec<_>>()
    );
    let probed_paths: Vec<&str> = snapshot.dbs.iter().map(|d| d.path.as_str()).collect();
    assert!(probed_paths.contains(&global_db.to_str().unwrap()));
    assert!(probed_paths.contains(&project_db.to_str().unwrap()));
}

#[test]
fn all_dbs_flag_restores_full_fleet() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (app_home, global_db, project_db) = three_db_fixture(dir.path());

    let snapshot = collect_snapshot_scoped(&app_home, &global_db, Some(&project_db), true);

    assert_eq!(
        snapshot.dbs.len(),
        3,
        "--all-dbs must restore every manifest entry: {:?}",
        snapshot.dbs.iter().map(|d| &d.path).collect::<Vec<_>>()
    );
}

#[test]
fn default_scope_with_no_project_db_probes_only_global() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (app_home, global_db, _project_db) = three_db_fixture(dir.path());

    let snapshot = collect_snapshot_scoped(&app_home, &global_db, None, false);

    assert_eq!(
        snapshot.dbs.len(),
        1,
        "with no project db path, default scope must probe only global: {:?}",
        snapshot.dbs.iter().map(|d| &d.path).collect::<Vec<_>>()
    );
    assert_eq!(snapshot.dbs[0].path, global_db.to_str().unwrap());
}

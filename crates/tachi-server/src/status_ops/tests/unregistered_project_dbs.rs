use super::*;
use crate::manifest::{Manifest, MANIFEST_SCHEMA_VERSION};

fn mk_project_db(projects_dir: &std::path::Path, name: &str, write_db: bool) -> std::path::PathBuf {
    let project_dir = projects_dir.join(name);
    std::fs::create_dir_all(&project_dir).expect("create project dir");
    let db_path = project_dir.join("memory.db");
    if write_db {
        std::fs::write(&db_path, b"not a real sqlite file, path presence is enough")
            .expect("write fixture memory.db");
    }
    db_path
}

fn empty_manifest() -> Manifest {
    Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generated_at: "2026-07-12T00:00:00+00:00".to_string(),
        comment: String::new(),
        dbs: Vec::new(),
    }
}

fn manifest_with_entry(scope_hint: &str, path: &std::path::Path) -> Manifest {
    let mut e = entry(DbRole::Project, scope_hint);
    e.path = path.to_string_lossy().to_string();
    let mut manifest = empty_manifest();
    manifest.dbs.push(e);
    manifest
}

fn manifest_with_entries(pairs: &[(&str, &std::path::Path)]) -> Manifest {
    let mut manifest = empty_manifest();
    for (scope_hint, path) in pairs {
        let mut e = entry(DbRole::Project, scope_hint);
        e.path = path.to_string_lossy().to_string();
        manifest.dbs.push(e);
    }
    manifest
}

// #1040: a `<projects_dir>/<name>/memory.db` that exists on disk but has no
// matching manifest entry must be counted as unregistered.
#[test]
fn counts_project_dirs_with_memory_db_not_in_manifest() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let projects_dir = app_home.path().join("projects");
    let registered_db = mk_project_db(&projects_dir, "registered-project", true);
    let _unregistered_db = mk_project_db(&projects_dir, "rogue-project", true);

    let manifest = manifest_with_entry("project:registered-project", &registered_db);

    let count = count_unregistered_project_dbs(&projects_dir, &manifest);
    assert_eq!(
        count, 1,
        "exactly the rogue project dir (not in manifest) must be counted"
    );
}

// #1040: every project dir with a memory.db has a matching manifest entry ->
// zero unregistered, no warning line should be emitted.
//
// Isolation note: `push_unregistered_project_db_warning` does NOT take the
// in-memory `manifest` this test builds — it re-derives its own manifest by
// reading `<app_home>/manifest.json` off disk (see `warnings.rs`). Without
// persisting `manifest` to that path first, the on-disk load falls through
// `Manifest::load_or_empty`'s `Self::empty()` branch (the file doesn't
// exist yet in a fresh tempdir), so `db_a`/`db_b` would count as
// unregistered against an *empty* manifest regardless of what this test
// asserts about the in-memory one — a fixture/disk mismatch that made the
// original assertion fail deterministically (not, as first suspected, a
// leak onto the real `~/.tachi` home: `app_home` here is always the
// tempdir, never `resolve_app_home()`). Write the fixture manifest to disk
// so both call sites — the direct `count_unregistered_project_dbs` call
// below and `push_unregistered_project_db_warning`'s internal reload — see
// the identical, fixture-scoped state.
#[test]
fn zero_when_every_project_db_is_registered() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let projects_dir = app_home.path().join("projects");
    let db_a = mk_project_db(&projects_dir, "project-a", true);
    let db_b = mk_project_db(&projects_dir, "project-b", true);

    let manifest =
        manifest_with_entries(&[("project:project-a", &db_a), ("project:project-b", &db_b)]);
    manifest
        .save(&app_home.path().join("manifest.json"))
        .expect(
            "persist fixture manifest.json so push_unregistered_project_db_warning's \
                 own on-disk reload sees the same registrations, not an empty fallback",
        );

    let count = count_unregistered_project_dbs(&projects_dir, &manifest);
    assert_eq!(count, 0, "all project DBs registered -> zero unregistered");

    let mut warnings = Vec::new();
    push_unregistered_project_db_warning(&mut warnings, app_home.path());
    assert!(
        warnings.is_empty(),
        "no unregistered project DBs -> no warning line; got {warnings:?}"
    );
}

// A freshly created project directory with no memory.db yet (nothing written)
// must not be counted — there is nothing to register.
#[test]
fn empty_project_dir_without_memory_db_is_not_counted() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let projects_dir = app_home.path().join("projects");
    let _empty_dir = mk_project_db(&projects_dir, "not-yet-initialized", false);

    let count = count_unregistered_project_dbs(&projects_dir, &empty_manifest());
    assert_eq!(count, 0, "no memory.db yet -> nothing to register");
}

// Missing `projects/` directory entirely (e.g. a fresh install with no
// projects created yet) must not error or panic — zero, not a crash.
#[test]
fn missing_projects_dir_returns_zero() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let projects_dir = app_home.path().join("projects");

    let count = count_unregistered_project_dbs(&projects_dir, &empty_manifest());
    assert_eq!(count, 0);
}

// The warning line format includes the count and the issue reference, and
// is appended (not replacing) whatever build_status_warnings already
// produced.
//
// Explicit empty manifest.json persisted below so this doesn't silently
// depend on `push_unregistered_project_db_warning`'s no-file-on-disk ->
// empty-manifest fallback (see the isolation note on
// `zero_when_every_project_db_is_registered` above) — the fixture is
// intentionally "nothing registered", not "manifest.json happens to be
// absent".
#[test]
fn warning_line_reports_count_and_issue_reference() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let projects_dir = app_home.path().join("projects");
    let _rogue = mk_project_db(&projects_dir, "rogue", true);
    empty_manifest()
        .save(&app_home.path().join("manifest.json"))
        .expect("persist fixture empty manifest.json");

    let mut warnings = vec!["some pre-existing warning".to_string()];
    push_unregistered_project_db_warning(&mut warnings, app_home.path());

    assert_eq!(warnings.len(), 2, "must append, not replace: {warnings:?}");
    assert!(
        warnings[1].contains("unregistered project DBs: 1") && warnings[1].contains("#1040"),
        "unexpected warning line: {}",
        warnings[1]
    );
}

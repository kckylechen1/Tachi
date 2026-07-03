use crate::manifest::{DbEntry, DbRole};
use crate::status_ops::snapshot::db_status_label_for_tests;

fn entry(role: DbRole, scope_hint: &str, path: &str) -> DbEntry {
    DbEntry {
        path: path.to_string(),
        role,
        owner: "tachi".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: false,
        allow_write: true,
        last_doctor_at: "1970-01-01T00:00:00Z".to_string(),
        last_classification: "healthy".to_string(),
        scope_hint: scope_hint.to_string(),
        notes: String::new(),
    }
}

#[test]
fn db_status_label_skips_unknown_scope_hint_and_uses_project_name() {
    let path = std::path::PathBuf::from("/tmp/home/projects/sigil/memory.db");
    let label = db_status_label_for_tests(
        &entry(DbRole::Project, "unknown", path.to_str().unwrap()),
        &path,
    );
    assert_eq!(label, "project:sigil");
}

#[test]
fn db_status_label_skips_tachi_other_and_names_run_project_db() {
    let path = std::path::PathBuf::from(
        "/tmp/home/.tachi/runs/poke_20260628T103218Z_d0c2cbbd/sandbox/.tachi/project/memory.db",
    );
    let label = db_status_label_for_tests(
        &entry(DbRole::Unknown, "tachi-other", path.to_str().unwrap()),
        &path,
    );
    assert_eq!(label, "run:poke_20260628T103218Z_d0c2cbbd:project");
}

#[test]
fn db_status_label_keeps_global_role() {
    let path = std::path::PathBuf::from("/tmp/home/global/memory.db");
    let label = db_status_label_for_tests(
        &entry(DbRole::Global, "unknown", path.to_str().unwrap()),
        &path,
    );
    assert_eq!(label, "global");
}

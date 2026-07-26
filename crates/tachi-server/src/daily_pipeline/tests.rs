use super::*;
use crate::server_state::DbScope;
use std::path::PathBuf;

fn manifest_target(role: &str, path: PathBuf) -> ManifestDbTarget {
    ManifestDbTarget {
        name: "target".to_string(),
        label: "target".to_string(),
        path,
        role: role.to_string(),
        owner: "tachi".to_string(),
        schema_kind: "tachi".to_string(),
        allow_write: true,
        last_classification: "healthy".to_string(),
    }
}

#[tokio::test]
async fn collect_database_stats_for_targets_preserves_manifest_order() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut first = manifest_target("global", tmp.path().join("first.db"));
    first.name = "first".to_string();
    let mut second = manifest_target("project", tmp.path().join("second.db"));
    second.name = "second".to_string();

    let stats = collect_database_stats_for_targets(vec![first, second])
        .await
        .expect("stats");

    assert_eq!(
        stats
            .iter()
            .map(|stat| stat.name.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert!(stats.iter().all(|stat| stat.error.is_some()));
}

#[test]
fn truth_maintenance_routes_external_project_target_by_path() {
    let tmp = crate::test_support::non_skipped_fixture_tempdir("daily-pipeline-");
    let global = tmp.path().join("global").join("memory.db");
    let current_project = tmp
        .path()
        .join("workspace")
        .join(".tachi")
        .join("memory.db");
    crate::test_support::assert_repo_local_db_fixture_not_skipped(&current_project);
    let external = tmp.path().join("agent").join("memory.db");
    let target = manifest_target("agent", external.clone());

    let route = resolve_truth_maintenance_route_for_paths_in_home(
        tmp.path(),
        &global,
        Some(&current_project),
        &target,
    );

    assert_eq!(route.target_db, DbScope::Project);
    assert_eq!(route.named_project, None);
    assert_eq!(route.db_path.as_deref(), Some(external.as_path()));
}

#[test]
fn truth_maintenance_routes_external_global_target_as_global_path() {
    let tmp = crate::test_support::non_skipped_fixture_tempdir("daily-pipeline-");
    let global = tmp.path().join("global").join("memory.db");
    let current_project = tmp
        .path()
        .join("workspace")
        .join(".tachi")
        .join("memory.db");
    crate::test_support::assert_repo_local_db_fixture_not_skipped(&current_project);
    let external_global = tmp.path().join("archive").join("global-memory.db");
    let target = manifest_target("global", external_global.clone());

    let route = resolve_truth_maintenance_route_for_paths_in_home(
        tmp.path(),
        &global,
        Some(&current_project),
        &target,
    );

    assert_eq!(route.target_db, DbScope::Global);
    assert_eq!(route.named_project, None);
    assert_eq!(route.db_path.as_deref(), Some(external_global.as_path()));
}

#[test]
fn truth_maintenance_routes_plan_c_project_by_name() {
    let tmp = crate::test_support::non_skipped_fixture_tempdir("daily-pipeline-");

    let global = tmp.path().join("global").join("memory.db");
    let current_project = tmp
        .path()
        .join("workspace")
        .join(".tachi")
        .join("memory.db");
    crate::test_support::assert_repo_local_db_fixture_not_skipped(&current_project);
    let named = tmp.path().join("projects").join("sigil").join("memory.db");
    let target = manifest_target("project", named);

    let route = resolve_truth_maintenance_route_for_paths_in_home(
        tmp.path(),
        &global,
        Some(&current_project),
        &target,
    );

    assert_eq!(route.target_db, DbScope::Project);
    assert_eq!(route.named_project.as_deref(), Some("sigil"));
    assert_eq!(route.db_path, None);
}

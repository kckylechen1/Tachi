use super::*;
use memcore::{MemoryStore, TachiEventQuery};

fn create_named_project_db(project: &str) {
    let db_path = crate::path_utils::plan_c_global_db_path(project);
    std::fs::create_dir_all(db_path.parent().expect("named project DB parent"))
        .expect("create named project DB parent");
    let db_str = db_path.to_str().expect("named project DB path");
    drop(MemoryStore::open_with_label(db_str, project).expect("create named project DB"));
}

async fn emit_outcome(
    server: &crate::MemoryServer,
    id: &str,
    project: Option<&str>,
    outcome: &str,
) {
    let mut emit = tachi_event_params("emit");
    emit.id = Some(id.to_string());
    emit.source_repo = Some("sigil".to_string());
    emit.adapter = Some("route-test".to_string());
    emit.project = project.map(str::to_string);
    emit.domain = Some("routing".to_string());
    emit.session_id = Some(id.to_string());
    emit.actor = Some("tester".to_string());
    emit.event_type = Some("session.outcome".to_string());
    emit.authority = Some("review_signal_only".to_string());
    emit.payload = Some(json!({
        "outcome": outcome,
        "evidence_basis": "external_evidence",
    }));
    crate::event_ops::handle_tachi_event(server, emit)
        .await
        .expect("emit route outcome");
}

async fn query_event_ids(server: &crate::MemoryServer, project: Option<&str>) -> Vec<String> {
    let mut query = tachi_event_params("query");
    query.project = project.map(str::to_string);
    query.limit = 20;
    let body = crate::event_ops::handle_tachi_event(server, query)
        .await
        .expect("query route events");
    let parsed: Value = serde_json::from_str(&body).expect("query JSON");
    parsed["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter_map(|event| event["id"].as_str().map(str::to_string))
        .collect()
}

async fn outcome_event_count(server: &crate::MemoryServer, project: Option<&str>) -> u64 {
    let mut metrics = tachi_event_params("metrics");
    metrics.project = project.map(str::to_string);
    let body = crate::event_ops::handle_tachi_event(server, metrics)
        .await
        .expect("metrics route events");
    let parsed: Value = serde_json::from_str(&body).expect("metrics JSON");
    parsed["metrics"]["session_outcomes"]["outcome_events"]
        .as_u64()
        .expect("outcome event count")
}

fn store_event_count(store: &mut MemoryStore) -> Result<usize, String> {
    store
        .list_tachi_events(&TachiEventQuery {
            limit: 20,
            ..TachiEventQuery::default()
        })
        .map(|events| events.len())
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn tachi_event_route_keeps_explicit_named_project_and_global_fallback_isolated() {
    let (server, _temp_home) = make_server_with_temp_home();
    let named_project = "event_route_named";
    create_named_project_db(named_project);

    emit_outcome(
        &server,
        "named-route-outcome",
        Some(named_project),
        "ai_corrected",
    )
    .await;
    emit_outcome(&server, "global-route-outcome", None, "user_correct").await;
    emit_outcome(
        &server,
        "blank-global-route-outcome",
        Some("   "),
        "unresolved",
    )
    .await;

    let named_ids = query_event_ids(&server, Some(named_project)).await;
    assert_eq!(named_ids, vec!["named-route-outcome"]);
    assert_eq!(outcome_event_count(&server, Some(named_project)).await, 1);

    let global_ids = query_event_ids(&server, None).await;
    assert!(
        global_ids.contains(&"global-route-outcome".to_string()),
        "global query should include no-project events: {global_ids:?}"
    );
    assert!(
        global_ids.contains(&"blank-global-route-outcome".to_string()),
        "blank project should fall back to the global route when no project DB is bound: {global_ids:?}"
    );
    assert!(
        !global_ids.contains(&"named-route-outcome".to_string()),
        "global query must not read explicit named-project events: {global_ids:?}"
    );
    assert_eq!(outcome_event_count(&server, None).await, 2);

    let named_store_count = server
        .with_named_project_store_read(named_project, store_event_count)
        .expect("read named project events");
    let global_store_count = server
        .with_global_store_read(store_event_count)
        .expect("read global events");
    assert_eq!(named_store_count, 1);
    assert_eq!(global_store_count, 2);
}

#[tokio::test]
async fn tachi_event_route_uses_bound_project_db_without_explicit_project() {
    let (server, temp_home) = make_server_with_temp_home();
    emit_outcome(&server, "pre-bind-global-outcome", None, "ai_corrected").await;

    let root = temp_home
        .temp_home
        .join("Event Route Repo")
        .canonicalize()
        .unwrap_or_else(|_| temp_home.temp_home.join("Event Route Repo"));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");
    server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("project DB init should succeed");

    let named_project = "event_route_named_bound_server";
    create_named_project_db(named_project);
    emit_outcome(
        &server,
        "named-bound-route-outcome",
        Some(named_project),
        "ai_corrected",
    )
    .await;
    emit_outcome(&server, "bound-route-outcome", None, "user_correct").await;
    emit_outcome(
        &server,
        "blank-bound-route-outcome",
        Some("   "),
        "unresolved",
    )
    .await;

    let project_ids = query_event_ids(&server, None).await;
    assert!(
        project_ids.contains(&"bound-route-outcome".to_string()),
        "no-project query should use the bound project DB: {project_ids:?}"
    );
    assert!(
        project_ids.contains(&"blank-bound-route-outcome".to_string()),
        "blank project should fall back to the bound project DB: {project_ids:?}"
    );
    assert!(
        !project_ids.contains(&"pre-bind-global-outcome".to_string()),
        "bound project query must not fall through to pre-existing global events: {project_ids:?}"
    );
    assert!(
        !project_ids.contains(&"named-bound-route-outcome".to_string()),
        "bound project query must not read explicit named-project events: {project_ids:?}"
    );
    assert_eq!(outcome_event_count(&server, None).await, 2);

    let named_ids = query_event_ids(&server, Some(named_project)).await;
    assert_eq!(named_ids, vec!["named-bound-route-outcome"]);
    assert_eq!(outcome_event_count(&server, Some(named_project)).await, 1);

    let project_store_count = server
        .with_project_store_read(store_event_count)
        .expect("read bound project events");
    let global_store_count = server
        .with_global_store_read(store_event_count)
        .expect("read global events");
    assert_eq!(project_store_count, 2);
    assert_eq!(global_store_count, 1);
}

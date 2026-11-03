use super::*;
use crate::MemoryServer;

fn entry(id: &str, query: &str, path: &str) -> memcore::MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = path.to_string();
    entry.summary = format!("{query} stored evidence");
    entry.text = format!("{query} applies to this library");
    entry.keywords = vec![query.to_string()];
    entry
}

fn named_library(server: &MemoryServer, name: &str, entries: &[memcore::MemoryEntry]) {
    let path = server
        .tachi_home_dir()
        .join("projects")
        .join(name)
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut store = memcore::MemoryStore::open_with_label(path.to_str().unwrap(), name).unwrap();
    for entry in entries {
        store.upsert(entry).unwrap();
    }
}

fn fallback_config(server: &MemoryServer, projects: &[&str]) {
    std::fs::write(
        server.tachi_home_dir().join("routing.json"),
        serde_json::to_vec(&json!({"fallback_projects": projects})).unwrap(),
    )
    .unwrap();
}

async fn recall(server: &MemoryServer, query: &str, scope: &str, project: Option<&str>) -> Value {
    let params = serde_json::from_value(json!({
        "action": "search", "query": query, "content_scope": scope,
        "project": project, "format": "json", "top_k": 10,
    }))
    .unwrap();
    let response = crate::facade_memory_ops::handle_tachi_memory(server, params)
        .await
        .unwrap();
    serde_json::from_str(&response).unwrap()
}

fn rows(response: &Value) -> Vec<&Value> {
    response["sections"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|section| section["rows"].as_array().unwrap())
        .collect()
}

fn ids(response: &Value) -> Vec<String> {
    rows(response)
        .into_iter()
        .filter_map(|row| row["id"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn workspace_recall_all_stops_across_content_legs_before_global_and_shared() {
    let (server, _project) = make_server_with_project_fixture("recall-primary");
    let query = "WorkspaceFirstAllNeedle";
    server
        .with_project_store(|store| {
            store
                .upsert(&entry("workspace-memory", query, "/notes/local"))
                .map_err(|e| e.to_string())
        })
        .unwrap();
    server
        .with_global_store(|store| {
            store
                .upsert(&entry("global-memory", query, "/notes/global"))
                .map_err(|e| e.to_string())
        })
        .unwrap();
    let mut wiki = entry("shared-wiki", query, "/wiki/shared");
    wiki.metadata = json!({"lifecycle": "active"});
    named_library(&server, "wiki", &[wiki]);
    fallback_config(&server, &["must-not-be-resolved"]);

    let response = recall(&server, query, "all", None).await;
    assert_eq!(response["status"], "completed", "{response}");
    assert_eq!(ids(&response), vec!["workspace-memory"], "{response}");
    assert_eq!(
        rows(&response)[0]["store"],
        json!({"kind": "bound_project"})
    );
}

#[tokio::test]
async fn workspace_recall_filters_local_results_before_falling_back_without_global_access_writes() {
    let (server, _project) = make_server_with_project_fixture("recall-filtered");
    let query = "WorkspaceFilteredNeedle";
    // A matching Wiki row is not a result for content_scope=memory.
    let mut local_wiki = entry("local-wiki", query, "/wiki/local");
    local_wiki.metadata = json!({"lifecycle": "active"});
    server
        .with_project_store(|store| store.upsert(&local_wiki).map_err(|e| e.to_string()))
        .unwrap();
    server
        .with_global_store(|store| {
            store
                .upsert(&entry("global-result", query, "/notes/global"))
                .map_err(|e| e.to_string())
        })
        .unwrap();
    fallback_config(&server, &["must-not-be-resolved"]);
    let before = server
        .with_global_store_read(|store| store.get("global-result").map_err(|e| e.to_string()))
        .unwrap()
        .unwrap();

    let response = recall(&server, query, "memory", None).await;
    assert_eq!(ids(&response), vec!["global-result"], "{response}");
    assert_eq!(
        rows(&response)[0]["store"],
        json!({"kind": "legacy_global"})
    );
    let after = server
        .with_global_store_read(|store| store.get("global-result").map_err(|e| e.to_string()))
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.access_count, after.last_access),
        (before.access_count, before.last_access)
    );
}

#[tokio::test]
async fn workspace_recall_configured_projects_are_ordered_lazy_and_read_only() {
    let server = make_server();
    let query = "OrderedFallbackNeedle";
    named_library(&server, "empty-first", &[]);
    named_library(
        &server,
        "winner-second",
        &[entry("named-result", query, "/notes/winner")],
    );
    fallback_config(
        &server,
        &["empty-first", "winner-second", "must-not-be-resolved"],
    );

    let response = recall(&server, query, "memory", None).await;
    assert_eq!(response["status"], "completed", "{response}");
    assert_eq!(ids(&response), vec!["named-result"], "{response}");
    assert_eq!(
        rows(&response)[0]["store"],
        json!({"kind": "named_project", "project": "winner-second"})
    );
    let stored = server
        .with_named_project_store_read("winner-second", |store| {
            store.get("named-result").map_err(|e| e.to_string())
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.access_count, 0,
        "fallback must not reinforce another library"
    );
}

#[tokio::test]
async fn workspace_recall_explicit_project_does_not_expand_even_on_empty() {
    let server = make_server();
    let query = "ExplicitOnlyNeedle";
    named_library(&server, "explicit-empty", &[]);
    server
        .with_global_store(|store| {
            store
                .upsert(&entry("global-decoy", query, "/notes/global"))
                .map_err(|e| e.to_string())
        })
        .unwrap();
    fallback_config(&server, &["must-not-be-resolved"]);
    let response = recall(&server, query, "memory", Some("explicit-empty")).await;
    assert_eq!(response["status"], "completed", "{response}");
    assert!(rows(&response).is_empty(), "{response}");

    let missing = recall(&server, query, "memory", Some("unknown-explicit")).await;
    assert_eq!(missing["status"], "partial", "{missing}");
    assert!(rows(&missing).is_empty());
    assert!(missing.to_string().contains("unknown-explicit"));
}

#[tokio::test]
async fn workspace_recall_corrupt_current_library_is_an_error_not_a_global_miss() {
    let server = make_server();
    let query = "BrokenWorkspaceNeedle";
    let path = server
        .tachi_home_dir()
        .join("projects/broken-current")
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"not a SQLite database").unwrap();
    server.set_session_identity(None, Some("broken-current".to_string()), None);
    server
        .with_global_store(|store| {
            store
                .upsert(&entry("global-decoy", query, "/notes/global"))
                .map_err(|e| e.to_string())
        })
        .unwrap();

    let response = recall(&server, query, "memory", None).await;
    assert_eq!(response["status"], "partial", "{response}");
    assert!(rows(&response).is_empty(), "{response}");
    assert!(response["sections"][0].get("error").is_some(), "{response}");
}

#[tokio::test]
async fn workspace_recall_wiki_falls_back_only_after_local_lifecycle_filtering() {
    let (server, _project) = make_server_with_project_fixture("wiki-recall-primary");
    let query = "WikiLifecycleFallbackNeedle";
    let mut pending = entry("local-pending", query, "/wiki/pending");
    pending.metadata = json!({"lifecycle": "pending_review"});
    server
        .with_project_store(|store| store.upsert(&pending).map_err(|e| e.to_string()))
        .unwrap();
    let mut active = entry("shared-active", query, "/wiki/active");
    active.metadata = json!({"lifecycle": "active"});
    named_library(&server, "wiki", &[active]);
    fallback_config(&server, &["must-not-be-resolved"]);
    let response = recall(&server, query, "wiki", None).await;
    assert_eq!(ids(&response), vec!["shared-active"], "{response}");
    assert_eq!(
        rows(&response)[0]["store"],
        json!({"kind": "named_project", "project": "wiki"})
    );
    let standalone = crate::wiki_ops::collect_wiki_search_value(
        &server,
        serde_json::from_value(json!({"query": query})).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        standalone["results"][0]["id"], "shared-active",
        "{standalone}"
    );
    assert_eq!(standalone["stores"].as_array().unwrap().len(), 2,
        "standalone Wiki must check current then shared, and stop before the invalid configured target");
}

#[tokio::test]
async fn workspace_recall_explicit_directory_reads_an_existing_library_without_registration() {
    let server = make_server();
    let fixture = crate::test_support::non_skipped_fixture_tempdir("recall-workspace-");
    let root = fixture.path().join("library-folder");
    let db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let name = crate::path_utils::plan_c_dir_name_from_root(&root).unwrap();
    let query = "DirectoryFallbackNeedle";
    {
        let mut store = memcore::MemoryStore::open_with_label(db.to_str().unwrap(), &name).unwrap();
        store
            .upsert(&entry("directory-result", query, "/notes/directory"))
            .unwrap();
    }
    crate::project_db_ops::register_repo_local_manifest_entry_in_home(
        &db,
        &name,
        &server.tachi_home_dir(),
    )
    .unwrap();
    std::fs::write(
        server.tachi_home_dir().join("routing.json"),
        serde_json::to_vec(&json!({
            "fallback_workspaces": [root, fixture.path().join("must-not-be-resolved")]
        }))
        .unwrap(),
    )
    .unwrap();
    let manifest_path = server.tachi_home_dir().join("manifest.json");
    let before = std::fs::read(&manifest_path).unwrap();

    let response = recall(&server, query, "memory", None).await;
    assert_eq!(ids(&response), vec!["directory-result"], "{response}");
    assert_eq!(rows(&response)[0]["store"]["project"], name);
    assert_eq!(
        std::fs::read(&manifest_path).unwrap(),
        before,
        "reading a configured directory must not register or rewrite it"
    );
}

#[tokio::test]
async fn workspace_recall_missing_directory_library_is_error_and_never_created() {
    let server = make_server();
    let fixture = crate::test_support::non_skipped_fixture_tempdir("missing-recall-workspace-");
    let root = fixture.path().join("empty-folder");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        server.tachi_home_dir().join("routing.json"),
        serde_json::to_vec(&json!({
            "fallback_workspaces": [root]
        }))
        .unwrap(),
    )
    .unwrap();

    let response = recall(&server, "MissingDirectoryLibraryNeedle", "memory", None).await;
    assert_eq!(response["status"], "partial", "{response}");
    assert!(rows(&response).is_empty());
    assert!(
        !root.join(".tachi").exists(),
        "fallback lookup must not create a project DB"
    );
}

#[test]
fn workspace_recall_rejects_incompatible_hardlinked_alias_before_deduplication() {
    use crate::memory_search_ops::routing_config::WorkspaceRecallPlan;
    use crate::tool_params::StoreRef;
    let server = make_server();
    named_library(&server, "original", &[]);
    let projects = server.tachi_home_dir().join("projects");
    std::fs::create_dir_all(projects.join("hardlink-alias")).unwrap();
    std::fs::hard_link(
        projects.join("original").join(memcore::MEMORY_DB_FILENAME),
        projects
            .join("hardlink-alias")
            .join(memcore::MEMORY_DB_FILENAME),
    )
    .unwrap();
    let admission_error = server
        .with_named_project_store_read("hardlink-alias", |_| Ok(()))
        .expect_err("sharing an inode does not authorize another persistent role");
    fallback_config(&server, &["original", "hardlink-alias"]);
    let mut plan = WorkspaceRecallPlan::new(&server, None, false, true, false);
    assert!(matches!(
        plan.next(&server).unwrap().unwrap().store,
        StoreRef::LegacyGlobal
    ));
    assert_eq!(
        plan.next(&server).unwrap().unwrap().store,
        StoreRef::named("original")
    );
    assert_eq!(
        plan.next(&server)
            .err()
            .expect("an incompatible alias must not become a silent duplicate miss"),
        admission_error
    );
}

#[test]
fn workspace_recall_deduplicates_admitted_repository_compatibility_aliases() {
    use crate::memory_search_ops::routing_config::WorkspaceRecallPlan;
    use crate::tool_params::StoreRef;
    let server = make_server();
    let fixture = crate::test_support::non_skipped_fixture_tempdir("recall-alias-");
    let root = fixture.path().join("Repo");
    let db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let canonical = crate::path_utils::plan_c_dir_name_from_root(&root).unwrap();
    let compatibility = crate::path_utils::plan_c_previous_dir_name_from_root(&root).unwrap();
    assert_ne!(canonical, compatibility);
    drop(memcore::MemoryStore::open_with_label(db.to_str().unwrap(), &canonical).unwrap());
    crate::project_db_ops::register_repo_local_manifest_entry_in_home(
        &db,
        &canonical,
        &server.tachi_home_dir(),
    )
    .unwrap();
    for name in [&canonical, &compatibility] {
        server
            .with_named_project_store_read(name, |_| Ok(()))
            .expect("both repository-derived names must pass normal role admission");
    }
    let before_db = std::fs::read(&db).unwrap();
    let manifest_path = server.tachi_home_dir().join("manifest.json");
    let before_manifest = std::fs::read(&manifest_path).unwrap();
    fallback_config(&server, &[&canonical, &compatibility]);
    let mut plan = WorkspaceRecallPlan::new(&server, None, false, true, false);
    assert!(matches!(
        plan.next(&server).unwrap().unwrap().store,
        StoreRef::LegacyGlobal
    ));
    assert_eq!(
        plan.next(&server).unwrap().unwrap().store,
        StoreRef::named(canonical)
    );
    assert!(
        plan.next(&server).unwrap().is_none(),
        "an admitted compatibility alias must not repeat the same physical library"
    );
    assert_eq!(std::fs::read(&db).unwrap(), before_db);
    assert_eq!(std::fs::read(&manifest_path).unwrap(), before_manifest);
}

#[tokio::test]
async fn workspace_recall_does_not_discover_unconfigured_projects_from_query_text() {
    let server = make_server();
    let project = "unconfigured-library";
    let query = "NoImplicitLibraryScanNeedle";
    named_library(
        &server,
        project,
        &[entry("unconfigured-result", query, "/notes/unconfigured")],
    );
    let response = recall(&server, &format!("{query} {project}"), "memory", None).await;
    assert_eq!(response["status"], "completed", "{response}");
    assert!(
        rows(&response).is_empty(),
        "query text must not grant a project a fallback slot: {response}"
    );
}

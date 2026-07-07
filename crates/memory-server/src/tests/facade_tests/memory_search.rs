use super::*;
use memory_core::MemoryStore;

#[derive(Debug, PartialEq, Eq)]
struct AccessSnapshot {
    access_count: i64,
    recall_count: i64,
    query_diversity: i64,
    tier: String,
    access_history_count: i64,
}

fn create_named_project_db(project: &str, entries: Vec<memory_core::MemoryEntry>) {
    let db_path = crate::path_utils::plan_c_global_db_path(project);
    std::fs::create_dir_all(db_path.parent().expect("named project DB parent"))
        .expect("create named project DB parent");
    let mut store =
        MemoryStore::open_with_label(db_path.to_str().expect("named project DB path"), project)
            .expect("create named project DB");
    for entry in entries {
        store.upsert(&entry).expect("seed named project entry");
    }
}

fn access_snapshot(server: &crate::MemoryServer, project: &str, id: &str) -> AccessSnapshot {
    server
        .with_named_project_store_read(project, |store| {
            store
                .connection()
                .query_row(
                    "SELECT access_count, recall_count, query_diversity, tier,
                            (SELECT COUNT(*) FROM access_history WHERE memory_id = ?1)
                     FROM memories WHERE id = ?1",
                    rusqlite::params![id],
                    |row| {
                        Ok(AccessSnapshot {
                            access_count: row.get(0)?,
                            recall_count: row.get(1)?,
                            query_diversity: row.get(2)?,
                            tier: row.get(3)?,
                            access_history_count: row.get(4)?,
                        })
                    },
                )
                .map_err(|e| format!("snapshot {id}: {e}"))
        })
        .expect("read named project access snapshot")
}

fn search_params(query: &str, project: Option<&str>) -> crate::tool_params::SearchMemoryParams {
    crate::tool_params::SearchMemoryParams {
        query: query.to_string(),
        query_vec: None,
        top_k: 5,
        path_prefix: None,
        include_training: false,
        include_archived: false,
        candidates_per_channel: 20,
        mmr_threshold: Some(0.85),
        graph_expand_hops: 0,
        graph_relation_filter: None,
        weights: None,
        context_symbols: Vec::new(),
        agent_role: None,
        project: project.map(str::to_string),
        domain: None,
        file_context: None,
        error_context: None,
        enable_rerank: false,
        as_of: None,
        include_metadata: false,
    }
}

async fn bind_test_project(server: &crate::MemoryServer, temp_home: &std::path::Path, name: &str) {
    let root = temp_home.join(name);
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");
    server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("project DB init should succeed");
}

#[tokio::test]
async fn tachi_memory_search_defaults_to_json_and_keeps_markdown_escape_hatch() {
    let server = make_server();

    let mut json_params = tachi_memory_params("search");
    json_params.format = None;
    json_params.query = Some("facade default json no matches".to_string());
    let json_body = crate::facade_memory_ops::handle_tachi_memory(&server, json_params)
        .await
        .expect("default search should succeed");
    let parsed: Value = serde_json::from_str(&json_body).expect("default search JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["query"], json!("facade default json no matches"));

    let mut markdown_params = tachi_memory_params("search");
    markdown_params.query = Some("facade markdown output".to_string());
    let markdown = crate::facade_memory_ops::handle_tachi_memory(&server, markdown_params)
        .await
        .expect("markdown search should succeed");
    assert!(markdown.starts_with("## Tachi search:"), "{markdown}");
}

#[tokio::test]
async fn tachi_memory_search_records_user_access_history() {
    let server = make_server();
    let entry_id = format!("explicit-recall-access-{}", uuid::Uuid::new_v4());
    let mut entry = make_entry(&entry_id);
    entry.summary = "explicit recall access sentinel".to_string();
    entry.text = "explicit recall access sentinel should record access history".to_string();
    entry.keywords = vec!["explicit".to_string(), "recall".to_string()];
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("seed: {e}")))
        .expect("seed recall entry");

    let mut params = tachi_memory_params("search");
    params.format = None;
    params.scope = Some("memory".to_string());
    params.query = Some("explicit recall access sentinel".to_string());
    params.top_k = 3;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("search should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("search JSON");
    let memory_rows = parsed["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["name"] == json!("Memory"))
        })
        .and_then(|section| section["rows"].as_array())
        .expect("memory rows");
    assert!(
        memory_rows.iter().any(|row| row["id"] == json!(entry_id)),
        "seeded row should be recalled: {memory_rows:?}"
    );

    let post = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&entry_id, false)
                .map_err(|e| format!("get: {e}"))
        })
        .expect("post-read")
        .expect("seeded entry exists");
    assert_eq!(
        post.access_count, 1,
        "user-facing search should bump access_count once"
    );
    assert!(
        post.last_access.is_some(),
        "user-facing search should set last_access"
    );
}

#[tokio::test]
async fn explicit_cross_project_search_returns_hits_without_recording_access() {
    let (server, temp_home) = make_server_with_temp_home();
    bind_test_project(&server, &temp_home.temp_home, "Explicit Source Repo").await;

    let foreign_project = "explicit_cross_project_target";
    let mut entry = make_entry("explicit-cross-project-needle");
    entry.summary = "Explicit cross project access sentinel".to_string();
    entry.text =
        "ExplicitCrossProjectNeedle should be returned without recording access.".to_string();
    entry.keywords = vec!["ExplicitCrossProjectNeedle".to_string()];
    create_named_project_db(foreign_project, vec![entry]);

    let before = access_snapshot(&server, foreign_project, "explicit-cross-project-needle");
    let response = server
        .search_memory(Parameters(search_params(
            "ExplicitCrossProjectNeedle",
            Some(foreign_project),
        )))
        .await
        .expect("explicit cross-project search should succeed");
    let rows: Vec<Value> = serde_json::from_str(&response).expect("search response JSON");
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("explicit-cross-project-needle")),
        "expected explicit project search to return the foreign hit: {rows:?}"
    );

    let after = access_snapshot(&server, foreign_project, "explicit-cross-project-needle");
    assert_eq!(
        after, before,
        "explicit foreign project search must not mutate target access bookkeeping"
    );
}

#[tokio::test]
async fn inferred_cross_project_search_returns_hits_without_recording_access() {
    let (server, temp_home) = make_server_with_temp_home();
    bind_test_project(&server, &temp_home.temp_home, "Inferred Source Repo").await;

    let foreign_project = "inferred_cross_project_target";
    let mut entry = make_entry("inferred-cross-project-needle");
    entry.summary = "Inferred cross project access sentinel".to_string();
    entry.text = "InferredCrossProjectNeedle should be found when query mentions inferred_cross_project_target.".to_string();
    entry.keywords = vec![
        "InferredCrossProjectNeedle".to_string(),
        foreign_project.to_string(),
    ];
    create_named_project_db(foreign_project, vec![entry]);

    let before = access_snapshot(&server, foreign_project, "inferred-cross-project-needle");
    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: format!("Find InferredCrossProjectNeedle in {foreign_project}"),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            agent_role: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("inferred cross-project search should succeed");
    assert!(
        response.contains("inferred-cross-project-needle")
            || response.contains("InferredCrossProjectNeedle"),
        "expected inferred project search to return the foreign hit: {response}"
    );

    let after = access_snapshot(&server, foreign_project, "inferred-cross-project-needle");
    assert_eq!(
        after, before,
        "inferred foreign project search must not mutate target access bookkeeping"
    );
}

#[tokio::test]
async fn global_only_named_project_search_returns_hits_without_recording_access() {
    let (server, _temp_home) = make_server_with_temp_home();
    assert_eq!(
        server.project_db_path_buf(),
        None,
        "fixture should exercise the no-bound-project fail-closed path"
    );

    let foreign_project = "global_only_cross_project_target";
    let mut entry = make_entry("global-only-cross-project-needle");
    entry.summary = "Global-only cross project access sentinel".to_string();
    entry.text =
        "GlobalOnlyCrossProjectNeedle should be returned without recording access.".to_string();
    entry.keywords = vec!["GlobalOnlyCrossProjectNeedle".to_string()];
    create_named_project_db(foreign_project, vec![entry]);

    let before = access_snapshot(&server, foreign_project, "global-only-cross-project-needle");
    let response = server
        .search_memory(Parameters(search_params(
            "GlobalOnlyCrossProjectNeedle",
            Some(foreign_project),
        )))
        .await
        .expect("global-only named project search should succeed");
    let rows: Vec<Value> = serde_json::from_str(&response).expect("search response JSON");
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("global-only-cross-project-needle")),
        "expected global-only named project search to return the foreign hit: {rows:?}"
    );

    let after = access_snapshot(&server, foreign_project, "global-only-cross-project-needle");
    assert_eq!(
        after, before,
        "no-bound-project named search must not mutate target access bookkeeping"
    );
}

#[tokio::test]
async fn own_project_named_search_still_records_access() {
    let (server, temp_home) = make_server_with_temp_home();
    bind_test_project(&server, &temp_home.temp_home, "Own Project Repo").await;
    let project_db = server
        .project_db_path_buf()
        .expect("bound project DB should be active");
    let project_name = crate::path_utils::plan_c_dir_name_from_root(
        project_db
            .parent()
            .and_then(std::path::Path::parent)
            .expect("repo root from .tachi/memory.db"),
    )
    .expect("derive plan C project name");

    let mut entry = make_entry("own-project-named-needle");
    entry.summary = "Own project named access sentinel".to_string();
    entry.text =
        "OwnProjectNamedNeedle should still record access in the bound project.".to_string();
    entry.keywords = vec!["OwnProjectNamedNeedle".to_string()];
    server
        .with_project_store(|store| store.upsert(&entry).map_err(|e| format!("seed: {e}")))
        .expect("seed own project entry");

    let before = server
        .with_project_store_read(|store| {
            store
                .get("own-project-named-needle")
                .map_err(|e| format!("get before: {e}"))
        })
        .expect("read own project before")
        .expect("own project entry exists before");
    assert_eq!(before.access_count, 0);

    let response = server
        .search_memory(Parameters(search_params(
            "OwnProjectNamedNeedle",
            Some(&project_name),
        )))
        .await
        .expect("own named project search should succeed");
    let rows: Vec<Value> = serde_json::from_str(&response).expect("search response JSON");
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("own-project-named-needle")),
        "expected own named project search to return the bound hit: {rows:?}"
    );

    let after = server
        .with_project_store_read(|store| {
            store
                .get("own-project-named-needle")
                .map_err(|e| format!("get after: {e}"))
        })
        .expect("read own project after")
        .expect("own project entry exists after");
    assert_eq!(
        after.access_count, 1,
        "own project named search must still record access"
    );
}

#[tokio::test]
async fn tachi_memory_search_caps_large_top_k() {
    let server = make_server();
    server
        .with_global_store(|store| {
            for idx in 0..(crate::MAX_FACADE_TOP_K + 25) {
                let mut entry = make_entry(&format!("facade-clamp-{idx}"));
                entry.path = format!("/facade/clamp/{idx}");
                entry.summary = format!("facade clamp sentinel {idx}");
                entry.text = format!("facade clamp sentinel searchable row {idx}");
                entry.keywords = vec!["facade".to_string(), "clamp".to_string()];
                store
                    .upsert(&entry)
                    .map_err(|e| format!("seed clamp row: {e}"))?;
            }
            Ok(())
        })
        .expect("seed clamp memories");

    let mut params = tachi_memory_params("search");
    params.format = None;
    params.query = Some("facade clamp sentinel".to_string());
    params.top_k = 10_000;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("search should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("search JSON");
    let memory_rows = parsed["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["name"] == json!("Memory"))
        })
        .and_then(|section| section["rows"].as_array())
        .expect("memory rows");

    assert_eq!(memory_rows.len(), crate::MAX_FACADE_TOP_K);
}

#[tokio::test]
async fn direct_tachi_search_does_not_surface_fts_errors_for_unbalanced_parentheses() {
    let server = make_server();
    let mut entry = make_entry("direct-search-paren-noise");
    entry.summary = "direct search parenthesis regression".to_string();
    entry.text =
        "ParenNoiseNeedle should survive unbalanced parentheses in search input.".to_string();
    entry.keywords = vec!["ParenNoiseNeedle".to_string()];
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("seed: {e}")))
        .expect("seed paren regression entry");

    let params = TachiSearchParams {
        query: "))) ParenNoiseNeedle (((".to_string(),
        scope: "memory".to_string(),
        top_k: 5,
        path_prefix: None,
        project: None,
        domain: None,
        file_context: None,
        error_context: None,
        context_symbols: Vec::new(),
        agent_role: None,
        category: None,
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        as_of: None,
    };

    let (sections, _, _) =
        crate::facade_search_ops::collect_tachi_search_sections(&server, &params).await;
    let memory_rows = sections
        .iter()
        .find(|(name, _)| name == "Memory")
        .map(|(_, rows)| rows)
        .expect("memory section");

    assert!(
        !memory_rows
            .as_str()
            .is_some_and(|rows| rows.contains("fts5: syntax error")),
        "facade must not render FTS syntax errors as user-facing rows: {memory_rows}"
    );
    assert!(
        memory_rows
            .to_string()
            .contains("direct-search-paren-noise"),
        "sanitized query should still find the lexical token, got: {memory_rows}"
    );
}

#[tokio::test]
async fn direct_tachi_search_caps_large_top_k() {
    let server = make_server();
    server
        .with_global_store(|store| {
            for idx in 0..(crate::MAX_FACADE_TOP_K + 25) {
                let mut entry = make_entry(&format!("direct-search-clamp-{idx}"));
                entry.path = format!("/facade/direct-clamp/{idx}");
                entry.summary = format!("direct facade clamp sentinel {idx}");
                entry.text = format!("direct facade clamp sentinel searchable row {idx}");
                entry.keywords = vec!["direct".to_string(), "facade".to_string()];
                store
                    .upsert(&entry)
                    .map_err(|e| format!("seed direct clamp row: {e}"))?;
            }
            Ok(())
        })
        .expect("seed direct clamp memories");

    let params = TachiSearchParams {
        query: "direct facade clamp sentinel".to_string(),
        scope: "memory".to_string(),
        top_k: 10_000,
        path_prefix: None,
        project: None,
        domain: None,
        file_context: None,
        error_context: None,
        context_symbols: Vec::new(),
        agent_role: None,
        category: None,
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        as_of: None,
    };

    let (sections, _, _) =
        crate::facade_search_ops::collect_tachi_search_sections(&server, &params).await;
    let memory_rows = sections
        .iter()
        .find(|(name, _)| name == "Memory")
        .and_then(|(_, rows)| rows.as_array())
        .expect("memory rows");

    assert_eq!(memory_rows.len(), crate::MAX_FACADE_TOP_K);
}

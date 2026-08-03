use super::*;
use memcore::MemoryStore;

#[derive(Debug, PartialEq, Eq)]
struct AccessSnapshot {
    access_count: i64,
    recall_count: i64,
    query_diversity: i64,
    tier: String,
    access_history_count: i64,
}

fn create_named_project_db(project: &str, entries: Vec<memcore::MemoryEntry>) {
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
        // tachi#1201 k3: search_memory now defaults to markdown; every
        // caller of this helper parses the response as JSON, so opt in
        // explicitly (this is the "existing JSON assertion migrated to
        // explicit format=json" sweep the ticket asks for).
        format: Some("json".to_string()),
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
    for section in parsed["sections"].as_array().expect("search sections") {
        assert!(
            section["rows"].is_array(),
            "section must keep array rows: {section:#}"
        );
        assert!(
            section.get("error").is_none(),
            "successful sections retain their existing shape: {section:#}"
        );
    }

    let mut markdown_params = tachi_memory_params("search");
    markdown_params.query = Some("facade markdown output".to_string());
    let markdown = crate::facade_memory_ops::handle_tachi_memory(&server, markdown_params)
        .await
        .expect("markdown search should succeed");
    assert!(markdown.starts_with("## Tachi search:"), "{markdown}");
}

/// A binding receipt is a diagnostic, not a result. `serde_json::Map` is a
/// `BTreeMap` (this workspace never enables `preserve_order`), so `"binding"`
/// sorts ahead of `"sections"` on key name alone and an unconditional receipt
/// buried the answer to every successful search under ~15 lines of paths.
///
/// Discrimination on ONE variable at a time, same server, same seeded row:
/// a routine hit carries only the one-line `binding_summary`; an un-honored
/// `scope`, and an empty result set (the exact "there is no memory" moment
/// the #898 receipts exist for), each put the full receipt back.
#[tokio::test]
async fn tachi_memory_search_json_attaches_full_binding_only_when_it_has_news() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("binding-news");
    let sentinel = "BindingNewsSearchSentinel";
    let mut entry = make_entry("binding-news-search-sentinel");
    entry.path = "/facade/binding-news".to_string();
    entry.summary = format!("{sentinel} summary");
    entry.text = format!("{sentinel} row so a routine search returns results");
    entry.keywords = vec![sentinel.to_string()];
    server
        .with_project_store(|store| {
            store
                .upsert(&entry)
                .map_err(|e| format!("seed project row: {e}"))
        })
        .expect("seed project row");
    server
        .with_global_store(|store| {
            store
                .upsert(&entry)
                .map_err(|e| format!("seed global row: {e}"))
        })
        .expect("seed global row");

    // 1. Routine hit: project-bound process, honored scope, rows returned.
    let mut hit = tachi_memory_params("search");
    hit.format = Some("json".to_string());
    hit.scope = Some("memory".to_string());
    hit.query = Some(sentinel.to_string());
    let hit_body = crate::facade_memory_ops::handle_tachi_memory(&server, hit)
        .await
        .expect("routine search should succeed");
    let hit_json: Value = serde_json::from_str(&hit_body).expect("routine search JSON");
    let hit_rows = hit_json["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["name"] == json!("Memory"))
        })
        .and_then(|section| section["rows"].as_array())
        .expect("memory rows");
    assert!(
        !hit_rows.is_empty(),
        "fixture precondition: the routine arm must actually return rows, \
         otherwise it is testing the empty-result arm; got {hit_json:#}"
    );
    assert!(
        hit_json.get("binding").is_none(),
        "a routine successful search must not bury its results under a \
         diagnostic receipt: {hit_json:#}"
    );
    assert!(
        hit_json["binding_summary"]
            .as_str()
            .is_some_and(|line| line.starts_with("Library binding:")),
        "provenance is suppressed to one line, never dropped: {hit_json:#}"
    );

    // 2. Same query, un-honored `scope`: the caller did not get what it asked
    //    for, so the full receipt comes back.
    let mut remapped = tachi_memory_params("search");
    remapped.format = Some("json".to_string());
    remapped.scope = Some("not-a-scope".to_string());
    remapped.query = Some(sentinel.to_string());
    let remapped_body = crate::facade_memory_ops::handle_tachi_memory(&server, remapped)
        .await
        .expect("scope-remapped search should succeed");
    let remapped_json: Value =
        serde_json::from_str(&remapped_body).expect("scope-remapped search JSON");
    assert_eq!(remapped_json["scope_remapped"], json!(true));
    assert!(
        remapped_json["binding"]["global_path"].as_str().is_some(),
        "an un-honored scope must carry the full binding receipt: \
         {remapped_json:#}"
    );

    // 3. Same server, honored scope, nothing found: the "there is no memory"
    //    moment must say which libraries were actually addressed.
    let mut empty = tachi_memory_params("search");
    empty.format = Some("json".to_string());
    empty.scope = Some("memory".to_string());
    empty.query = Some("ZqxvBindingNewsNoSuchTokenAnywhere".to_string());
    let empty_body = crate::facade_memory_ops::handle_tachi_memory(&server, empty)
        .await
        .expect("empty search should succeed");
    let empty_json: Value = serde_json::from_str(&empty_body).expect("empty search JSON");
    assert!(
        empty_json["sections"]
            .as_array()
            .expect("sections")
            .iter()
            .all(|section| section["rows"]
                .as_array()
                .is_some_and(|rows| rows.is_empty())),
        "fixture precondition: this arm must return no rows; got {empty_json:#}"
    );
    assert!(
        empty_json["binding"]["global_path"].as_str().is_some(),
        "an empty result set must carry the full binding receipt: {empty_json:#}"
    );
}

/// Companion to the receipt-volume test above, and the reason no
/// `explicit_project != effective_named_project` trigger was added to
/// `binding_receipt_is_notable`: on this surface a caller who names a project
/// that does not exist is NOT silently downgraded to the bound project DB.
/// `search_memory/rows.rs`'s named-project branch returns
/// `Project '<name>' not found` whenever `project_only` is false — which every
/// receipt-carrying caller passes (`facade_search_ops.rs`,
/// `tools/memory_facade.rs`, `bootstrap/cli_tool.rs`) — and
/// `json_search_section` turns that into a typed `search_failure` section.
///
/// The discrimination that matters is the sentinel: it exists ONLY in the
/// bound project DB, so if the named-project miss ever starts falling through
/// to the bound store, this arm returns rows and goes red instead of quietly
/// answering a different question than the one asked.
#[tokio::test]
async fn tachi_memory_search_names_a_missing_project_instead_of_answering_from_the_bound_db() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("missing-project");
    let sentinel = "MissingProjectDowngradeSentinel";
    let mut entry = make_entry("missing-project-downgrade-sentinel");
    entry.path = "/facade/missing-project".to_string();
    entry.summary = format!("{sentinel} summary");
    entry.text = format!("{sentinel} row that lives only in the bound project DB");
    entry.keywords = vec![sentinel.to_string()];
    server
        .with_project_store(|store| {
            store
                .upsert(&entry)
                .map_err(|e| format!("seed project row: {e}"))
        })
        .expect("seed project row");

    let mut params = tachi_memory_params("search");
    params.format = Some("json".to_string());
    params.scope = Some("memory".to_string());
    params.query = Some(sentinel.to_string());
    params.project = Some("ZqxvNoSuchNamedProjectAnywhere".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("a missing named project is reported in-band, not as a transport error");
    let parsed: Value = serde_json::from_str(&body).expect("search JSON");
    let memory = parsed["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["name"] == json!("Memory"))
        })
        .expect("memory section");

    assert_eq!(
        memory["rows"],
        json!([]),
        "a named project that does not exist must not be served from the \
         bound project DB: {parsed:#}"
    );
    assert_eq!(memory["error"]["kind"], json!("search_failure"));
    assert!(
        memory["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("ZqxvNoSuchNamedProjectAnywhere")
                && message.contains("not found")),
        "the failure must name the project the caller asked for: {memory:#}"
    );
}

#[tokio::test]
async fn tachi_memory_search_json_failure_keeps_rows_an_array_and_exposes_typed_error() {
    let server = make_server();
    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| connection.execute_batch("DROP TABLE memories"),
    )
    .map_err(|err| format!("force deterministic search failure: {err}"))
    .expect("drop only this test server's memory table");

    let mut params = tachi_memory_params("search");
    params.format = None;
    params.scope = Some("memory".to_string());
    params.query = Some("forced search failure".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("JSON wrapper must represent a failed section");
    let parsed: Value = serde_json::from_str(&body).expect("search JSON");
    let memory = parsed["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["name"] == json!("Memory"))
        })
        .expect("memory section");

    assert_eq!(memory["rows"], json!([]), "failure is not row data");
    assert_eq!(memory["error"]["kind"], json!("search_failure"));
    assert!(
        memory["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Search failed")),
        "failure must remain observable: {memory:#}"
    );
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
async fn tachi_search_patterns_scope_can_recall_continuity_projection_rows() {
    let server = make_server();
    let mut pattern = make_entry("patterns-scope-projection");
    pattern.path = "/user/patterns/projection-boundary".to_string();
    pattern.summary = "PatternScopeProjectionNeedle durable preference".to_string();
    pattern.text = "PatternScopeProjectionNeedle must remain explicitly searchable".to_string();
    pattern.keywords = vec!["PatternScopeProjectionNeedle".to_string()];
    pattern.metadata = json!({"projection_kind": "pattern"});
    server
        .with_global_store(|store| store.upsert(&pattern).map_err(|e| format!("seed: {e}")))
        .expect("seed pattern projection");

    let params = TachiSearchParams {
        query: "PatternScopeProjectionNeedle".to_string(),
        scope: "patterns".to_string(),
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
    let pattern_rows = sections
        .iter()
        .find(|(name, _)| name == "Patterns")
        .and_then(|(_, rows)| rows.as_array())
        .expect("patterns section");
    assert!(pattern_rows
        .iter()
        .any(|row| row["id"] == json!("patterns-scope-projection")));
}

#[tokio::test]
async fn tachi_search_memory_scope_honors_explicit_continuity_path_scope() {
    let server = make_server();
    let mut timeline = make_entry("memory-scope-timeline-projection");
    timeline.path = "/timeline/session/projection-boundary".to_string();
    timeline.summary = "TimelineScopeProjectionNeedle durable event".to_string();
    timeline.text = "TimelineScopeProjectionNeedle must remain explicitly searchable".to_string();
    timeline.keywords = vec!["TimelineScopeProjectionNeedle".to_string()];
    timeline.metadata = json!({"projection_kind": "timeline"});
    server
        .with_global_store(|store| store.upsert(&timeline).map_err(|e| format!("seed: {e}")))
        .expect("seed timeline projection");

    let params = TachiSearchParams {
        query: "TimelineScopeProjectionNeedle".to_string(),
        scope: "memory".to_string(),
        top_k: 5,
        path_prefix: Some("/timeline".to_string()),
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
        .expect("memory section");
    assert!(memory_rows
        .iter()
        .any(|row| row["id"] == json!("memory-scope-timeline-projection")));
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

// tachi#1201 k3: the raw `search_memory` MCP tool defaults `format` to
// markdown when omitted; explicit format="json" must stay byte-for-byte the
// same JSON row-array shape this tool always returned before k3.

#[tokio::test]
async fn search_memory_defaults_to_markdown_when_format_omitted() {
    let server = make_server();
    let entry_id = format!("search-default-format-{}", uuid::Uuid::new_v4());
    let mut entry = make_entry(&entry_id);
    entry.summary = "search default format sentinel".to_string();
    entry.text = "SearchDefaultFormatNeedle should show up in the markdown digest.".to_string();
    entry.keywords = vec!["SearchDefaultFormatNeedle".to_string()];
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("seed: {e}")))
        .expect("seed default-format entry");

    let response = server
        .search_memory(Parameters(crate::tool_params::SearchMemoryParams {
            query: "SearchDefaultFormatNeedle".to_string(),
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
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            format: None,
        }))
        .await
        .expect("default-format search should succeed");

    assert!(
        response.starts_with("## Tachi memory search:"),
        "omitted format should render markdown, got: {response}"
    );
    assert!(
        serde_json::from_str::<Vec<Value>>(&response).is_err(),
        "markdown digest must not happen to parse as the JSON row array: {response}"
    );
    assert!(response.contains(&entry_id), "{response}");
}

#[tokio::test]
async fn search_memory_explicit_json_keeps_pre_k3_row_array_shape() {
    let server = make_server();
    let entry_id = format!("search-explicit-json-{}", uuid::Uuid::new_v4());
    let mut entry = make_entry(&entry_id);
    entry.summary = "search explicit json sentinel".to_string();
    entry.text = "SearchExplicitJsonNeedle should show up in the JSON row array.".to_string();
    entry.keywords = vec!["SearchExplicitJsonNeedle".to_string()];
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("seed: {e}")))
        .expect("seed explicit-json entry");

    let response = server
        .search_memory(Parameters(crate::tool_params::SearchMemoryParams {
            query: "SearchExplicitJsonNeedle".to_string(),
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
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            format: Some("json".to_string()),
        }))
        .await
        .expect("explicit json search should succeed");

    let rows: Vec<Value> = serde_json::from_str(&response)
        .expect("explicit format=\"json\" must parse as the row array, byte-identical shape");
    assert!(
        rows.iter().any(|row| row["id"] == json!(entry_id)),
        "seeded row should be present: {rows:?}"
    );
}

#[tokio::test]
async fn search_memory_format_is_case_insensitive_and_trims_whitespace() {
    let server = make_server();
    for candidate in ["JSON", " json ", "Json"] {
        let response = server
            .search_memory(Parameters(crate::tool_params::SearchMemoryParams {
                query: "format polarity probe".to_string(),
                query_vec: None,
                top_k: 3,
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
                project: None,
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
                as_of: None,
                include_metadata: false,
                format: Some(candidate.to_string()),
            }))
            .await
            .unwrap_or_else(|e| panic!("format={candidate:?} should succeed: {e}"));
        serde_json::from_str::<Vec<Value>>(&response)
            .unwrap_or_else(|e| panic!("format={candidate:?} should parse as JSON: {e}"));
    }

    let markdown = server
        .search_memory(Parameters(crate::tool_params::SearchMemoryParams {
            query: "format polarity probe".to_string(),
            query_vec: None,
            top_k: 3,
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
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            format: Some("yaml".to_string()),
        }))
        .await
        .expect("unrecognized format should still succeed");
    assert!(markdown.starts_with("## Tachi memory search:"));
}

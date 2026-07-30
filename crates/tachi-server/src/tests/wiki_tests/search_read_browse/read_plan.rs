use super::*;

use crate::wiki_ops::{collect_wiki_read_value, collect_wiki_search_value};

fn planned_wiki_search(query: &str, project: Option<&str>, top_k: usize) -> WikiSearchParams {
    WikiSearchParams {
        query: query.to_string(),
        path_prefix: Some("/wiki".to_string()),
        category: None,
        top_k,
        include_archived: false,
        agent_role: None,
        project: project.map(str::to_string),
        domain: None,
        file_context: None,
        error_context: None,
        weights: None,
        lifecycle: None,
    }
}

fn wiki_entry(id: &str, path: &str, text: &str) -> MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = path.to_string();
    entry.summary = text.to_string();
    entry.text = text.to_string();
    entry.metadata = json!({"lifecycle": "active"});
    entry
}

fn register_named_project(server: &MemoryServer, name: &str) {
    let db_path = server
        .tachi_home_dir()
        .join("projects")
        .join(name)
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(db_path.parent().expect("named project parent"))
        .expect("create named project parent");
    drop(
        MemoryStore::open(db_path.to_str().expect("utf8 named project DB"))
            .expect("create named project DB"),
    );

    let manifest_path = server.tachi_home_dir().join("manifest.json");
    let mut manifest = crate::manifest::Manifest::load_or_empty(&manifest_path);
    manifest.dbs.push(crate::manifest::DbEntry {
        path: db_path.display().to_string(),
        role: crate::manifest::DbRole::Project,
        owner: "test".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: Utc::now().to_rfc3339(),
        last_classification: "healthy".to_string(),
        scope_hint: format!("project:{name}"),
        notes: String::new(),
    });
    manifest
        .save(&manifest_path)
        .expect("register named project");
}

#[tokio::test]
async fn explicit_named_wiki_search_never_falls_back_to_bound_or_legacy_global() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("bound-project");
    let query = "WikiReadPlanNamedOnlyNeedle";
    let named = wiki_entry(
        "wiki-read-plan-named",
        "/wiki/read-plan/named",
        &format!("{query} named-store sentinel"),
    );
    let bound = wiki_entry(
        "wiki-read-plan-bound",
        "/wiki/read-plan/bound",
        &format!("{query} bound-store sentinel"),
    );
    let legacy_global = wiki_entry(
        "wiki-read-plan-legacy-global",
        "/wiki/read-plan/legacy-global",
        &format!("{query} legacy-global sentinel"),
    );

    register_named_project(&server, "named-only");
    server
        .with_named_project_store("named-only", |store| {
            store.upsert(&named).map_err(|error| error.to_string())
        })
        .expect("seed named wiki store");
    server
        .with_project_store(|store| store.upsert(&bound).map_err(|error| error.to_string()))
        .expect("seed bound project wiki store");
    server
        .with_global_store(|store| {
            store
                .upsert(&legacy_global)
                .map_err(|error| error.to_string())
        })
        .expect("seed legacy global wiki store");

    let result =
        collect_wiki_search_value(&server, planned_wiki_search(query, Some("named-only"), 10))
            .await
            .expect("named wiki search");
    let ids = result["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>();

    assert!(
        ids.contains(&"wiki-read-plan-named"),
        "named hit missing: {ids:?}"
    );
    assert!(
        !ids.contains(&"wiki-read-plan-bound"),
        "RED: explicit project=named-only must not fall back to the bound project: {ids:?}"
    );
    assert!(
        !ids.contains(&"wiki-read-plan-legacy-global"),
        "RED: explicit project=named-only must not fall back to legacy global /wiki: {ids:?}"
    );
}

#[tokio::test]
async fn omitted_project_federates_bound_and_shared_but_not_legacy_global() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("bound-project");
    let query = "WikiReadPlanFederatedNeedle";
    let bound = wiki_entry(
        "wiki-read-plan-federated-bound",
        "/wiki/read-plan/federated-bound",
        &format!("{query} bound-store sentinel"),
    );
    let shared = wiki_entry(
        "wiki-read-plan-federated-shared",
        "/wiki/read-plan/federated-shared",
        &format!("{query} shared-store sentinel"),
    );
    let legacy_global = wiki_entry(
        "wiki-read-plan-federated-legacy-global",
        "/wiki/read-plan/federated-legacy-global",
        &format!("{query} legacy-global sentinel"),
    );

    register_named_project(&server, "wiki");
    server
        .with_project_store(|store| store.upsert(&bound).map_err(|error| error.to_string()))
        .expect("seed bound Wiki");
    server
        .with_named_project_store("wiki", |store| {
            store.upsert(&shared).map_err(|error| error.to_string())
        })
        .expect("seed shared Wiki");
    server
        .with_global_store(|store| {
            store
                .upsert(&legacy_global)
                .map_err(|error| error.to_string())
        })
        .expect("seed legacy global Wiki");

    let result = collect_wiki_search_value(&server, planned_wiki_search(query, None, 50))
        .await
        .expect("federated Wiki search");
    let rows = result["results"].as_array().expect("results array");
    let ids = rows
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>();
    assert!(
        ids.contains(&"wiki-read-plan-federated-bound"),
        "bound Wiki hit missing: {rows:?}"
    );
    assert!(
        ids.contains(&"wiki-read-plan-federated-shared"),
        "shared Wiki hit missing: {rows:?}"
    );
    assert!(
        !ids.contains(&"wiki-read-plan-federated-legacy-global"),
        "RED: legacy global is a migration input, not a default Wiki authority: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| !row["store"].is_null()),
        "every federated row must carry physical store identity: {rows:?}"
    );

    let candidate_counts = result["candidate_counts"]
        .as_array()
        .expect("candidate counts array");
    assert_eq!(
        candidate_counts.len(),
        2,
        "default federation must plan exactly bound + shared stores: {candidate_counts:?}"
    );
    assert!(
        candidate_counts
            .iter()
            .all(|candidate| candidate["count"].as_u64().unwrap_or(0) > 0),
        "each store must receive an independent candidate budget: {candidate_counts:?}"
    );
}

#[tokio::test]
async fn wiki_search_does_not_apply_generic_noise_skip_to_explicit_lookup() {
    let entry = wiki_entry(
        "wiki-short-explicit-query",
        "/wiki/read-plan/short-query",
        "kdb is an intentional Wiki lookup token",
    );
    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let result = collect_wiki_search_value(&server, planned_wiki_search("kdb", Some("wiki"), 10))
        .await
        .expect("short explicit Wiki lookup");
    assert!(
        result["results"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == "wiki-short-explicit-query")),
        "RED: dedicated Wiki lookup must not inherit generic conversational-noise skipping: {result:?}"
    );
}

#[tokio::test]
async fn recall_context_omitted_wiki_project_uses_bound_and_shared_plan() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("bound-project");
    crate::tests::register_logical_shared_wiki(&server);
    let query = "RecallContextFederatedWikiNeedle";
    let bound = wiki_entry(
        "recall-context-bound-wiki",
        "/wiki/read-plan/recall-context",
        &format!("{query} bound project sentinel"),
    );
    server
        .with_project_store(|store| store.upsert(&bound).map_err(|error| error.to_string()))
        .expect("seed bound recall-context wiki");

    let params: crate::tool_params::RecallContextParams = serde_json::from_value(json!({
        "query": query,
        "top_k": 1,
        "wiki_top_k": 3
    }))
    .expect("deserialize omitted wiki_project");
    assert!(params.wiki_project.is_none());
    let response = crate::foundry_runtime_ops::handle_recall_context(&server, params)
        .await
        .expect("federated recall context");
    let value: Value = serde_json::from_str(&response).expect("recall context JSON");
    assert!(
        value["wiki_results"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row["id"] == "recall-context-bound-wiki"
                    && row["store"] == json!({"kind": "bound_project"})
            })),
        "RED: omitted wiki_project failed to search the bound Wiki store: {value:#}"
    );
}

#[test]
fn federated_browse_round_robins_store_budgets() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("bound-project");
    register_named_project(&server, "wiki");
    for index in 0..3 {
        let entry = wiki_entry(
            &format!("wiki-browse-bound-{index}"),
            &format!("/wiki/read-plan/browse/bound-{index}"),
            "bound browse row",
        );
        server
            .with_project_store(|store| store.upsert(&entry).map_err(|error| error.to_string()))
            .expect("seed bound browse row");
    }
    let shared = wiki_entry(
        "wiki-browse-shared",
        "/wiki/read-plan/browse/shared",
        "shared browse row",
    );
    server
        .with_named_project_store("wiki", |store| {
            store.upsert(&shared).map_err(|error| error.to_string())
        })
        .expect("seed shared browse row");

    let value = crate::wiki_ops::collect_wiki_browse_value(
        &server,
        WikiBrowseParams {
            category: Some("/wiki/read-plan/browse".to_string()),
            limit: 2,
            project: None,
            lifecycle: None,
        },
    )
    .expect("federated browse");
    let entries = value["entries"].as_array().expect("browse entries");
    assert!(
        entries
            .iter()
            .any(|entry| entry["id"] == "wiki-browse-shared"),
        "RED: bound rows consumed the whole browse limit before shared was considered: {entries:?}"
    );
}

#[test]
fn federated_same_path_read_returns_store_qualified_candidates() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("bound-project");
    let path = "/wiki/read-plan/collision";
    let shared = wiki_entry(
        "wiki-read-plan-shared-collision",
        path,
        "shared collision sentinel",
    );
    let bound = wiki_entry(
        "wiki-read-plan-bound-collision",
        path,
        "bound collision sentinel",
    );

    register_named_project(&server, "wiki");
    server
        .with_named_project_store("wiki", |store| {
            store.upsert(&shared).map_err(|error| error.to_string())
        })
        .expect("seed logical shared wiki");
    server
        .with_project_store(|store| store.upsert(&bound).map_err(|error| error.to_string()))
        .expect("seed bound project wiki");

    let read = collect_wiki_read_value(&server, path, "wiki").expect("federated read");
    assert_eq!(read["status"], json!("ambiguous"));
    let candidates = read["candidates"].as_array().expect("candidate array");
    assert_eq!(candidates.len(), 2, "expected one candidate per store");
    assert!(
        candidates
            .iter()
            .all(|candidate| !candidate["store"].is_null()),
        "RED: cross-store ambiguity must carry store-qualified candidates: {candidates:?}"
    );
}

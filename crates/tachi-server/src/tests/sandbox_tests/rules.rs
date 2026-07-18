use super::*;

#[tokio::test]
async fn sandbox_check_respects_access_rules() {
    let server = make_server();

    // Set up a sandbox rule allowing read access for a specific role
    server
        .sandbox_set_rule(Parameters(SandboxSetRuleParams {
            agent_role: "test-role".to_string(),
            path_pattern: "/test/*".to_string(),
            access_level: "read".to_string(),
        }))
        .await
        .expect("sandbox_set_rule should succeed");

    // Check read access - should be allowed
    let check_read = server
        .sandbox_check(Parameters(SandboxCheckParams {
            agent_role: "test-role".to_string(),
            path: "/test/something".to_string(),
            operation: "read".to_string(),
        }))
        .await
        .expect("sandbox_check should succeed");

    let read_json: Value = serde_json::from_str(&check_read).unwrap();
    assert!(read_json["allowed"].as_bool().unwrap());

    // Check write access on read-only path - should be denied
    let check_write = server
        .sandbox_check(Parameters(SandboxCheckParams {
            agent_role: "test-role".to_string(),
            path: "/test/something".to_string(),
            operation: "write".to_string(),
        }))
        .await
        .expect("sandbox_check should succeed");

    let write_json: Value = serde_json::from_str(&check_write).unwrap();
    assert!(!write_json["allowed"].as_bool().unwrap());
}

#[tokio::test]
async fn sandbox_set_rule_updates_existing_rule() {
    let server = make_server();

    // Set initial rule with read access
    server
        .sandbox_set_rule(Parameters(SandboxSetRuleParams {
            agent_role: "update-role".to_string(),
            path_pattern: "/sensitive/*".to_string(),
            access_level: "read".to_string(),
        }))
        .await
        .expect("sandbox_set_rule should succeed");

    // Update to write access
    server
        .sandbox_set_rule(Parameters(SandboxSetRuleParams {
            agent_role: "update-role".to_string(),
            path_pattern: "/sensitive/*".to_string(),
            access_level: "write".to_string(),
        }))
        .await
        .expect("sandbox_set_rule update should succeed");

    // Verify write access is now allowed
    let check_write = server
        .sandbox_check(Parameters(SandboxCheckParams {
            agent_role: "update-role".to_string(),
            path: "/sensitive/data".to_string(),
            operation: "write".to_string(),
        }))
        .await
        .expect("sandbox_check should succeed");

    let write_json: Value = serde_json::from_str(&check_write).unwrap();
    assert!(write_json["allowed"].as_bool().unwrap());
}

#[tokio::test]
async fn sandbox_search_filters_project_rows_with_global_rules() {
    let tmp = tempfile::tempdir().expect("temp sandbox db root");
    let global_db = tmp.path().join("global/memory.db");
    let project_db = tmp.path().join("projects/sigil/memory.db");
    let server = crate::MemoryServer::new(global_db, Some(project_db)).expect("test server");

    server
        .with_global_store(|store| {
            let mut secret = make_entry("sandbox-global-secret");
            secret.path = "/secret/global".to_string();
            secret.text = "SandboxNeedle shared secret global memory".to_string();
            secret.summary = "global secret".to_string();
            store.upsert(&secret).map_err(|e| e.to_string())?;

            let mut public = make_entry("sandbox-global-public");
            public.path = "/public/global".to_string();
            public.text = "SandboxNeedle public global memory".to_string();
            public.summary = "global public".to_string();
            store.upsert(&public).map_err(|e| e.to_string())
        })
        .expect("seed global entries");

    server
        .with_project_store(|store| {
            let mut secret = make_entry("sandbox-project-secret");
            secret.path = "/secret/project".to_string();
            secret.text = "SandboxNeedle shared secret project memory".to_string();
            secret.summary = "project secret".to_string();
            store.upsert(&secret).map_err(|e| e.to_string())?;

            let mut public = make_entry("sandbox-project-public");
            public.path = "/public/project".to_string();
            public.text = "SandboxNeedle public project memory".to_string();
            public.summary = "project public".to_string();
            store.upsert(&public).map_err(|e| e.to_string())
        })
        .expect("seed project entries");

    server
        .sandbox_set_rule(Parameters(SandboxSetRuleParams {
            agent_role: "locked-reader".to_string(),
            path_pattern: "/secret/*".to_string(),
            access_level: "deny".to_string(),
        }))
        .await
        .expect("sandbox_set_rule should succeed");

    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "SandboxNeedle".to_string(),
            query_vec: None,
            top_k: 10,
            path_prefix: None,
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.85),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: Some("locked-reader".to_string()),
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            // tachi#1201 k3: search_memory now defaults to markdown; this
            // test parses the response as JSON, so opt in explicitly.
            format: Some("json".to_string()),
        }))
        .await
        .expect("sandboxed search should succeed");

    let rows: Vec<Value> = serde_json::from_str(&response).expect("search response JSON");
    let ids = rows
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>();

    assert!(
        ids.contains(&"sandbox-global-public"),
        "expected global public row in {ids:?}"
    );
    assert!(
        ids.contains(&"sandbox-project-public"),
        "expected project public row in {ids:?}"
    );
    assert!(
        !ids.contains(&"sandbox-global-secret"),
        "global deny rule should hide global secret row: {ids:?}"
    );
    assert!(
        !ids.contains(&"sandbox-project-secret"),
        "global deny rule should hide project secret row: {ids:?}"
    );

    let markdown = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "SandboxNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 10,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            agent_role: Some("locked-reader".to_string()),
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("sandboxed tachi_search should succeed");

    assert!(
        markdown.contains("sandbox-global-public"),
        "expected global public row in markdown: {markdown}"
    );
    assert!(
        markdown.contains("sandbox-project-public"),
        "expected project public row in markdown: {markdown}"
    );
    assert!(
        !markdown.contains("sandbox-global-secret"),
        "global deny rule should hide global secret markdown row: {markdown}"
    );
    assert!(
        !markdown.contains("sandbox-project-secret"),
        "global deny rule should hide project secret markdown row: {markdown}"
    );
}

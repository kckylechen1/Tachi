use super::*;

#[tokio::test]
async fn memory_graph_returns_seed_nodes_and_edges() {
    let server = make_server();
    let mut a = make_entry("m_a");
    a.topic = "alpha".to_string();
    a.text = "Alpha memory".to_string();
    let mut b = make_entry("m_b");
    b.topic = "beta".to_string();
    b.text = "Beta memory".to_string();
    let mut c = make_entry("m_c");
    c.topic = "gamma".to_string();
    c.text = "Gamma memory".to_string();

    server
        .with_global_store(|store| {
            store.upsert(&a).map_err(|e| e.to_string())?;
            store.upsert(&b).map_err(|e| e.to_string())?;
            store.upsert(&c).map_err(|e| e.to_string())?;
            store
                .add_edge(&memory_core::MemoryEdge {
                    source_id: "m_a".to_string(),
                    target_id: "m_b".to_string(),
                    relation: "related_to".to_string(),
                    weight: 1.0,
                    metadata: json!({}),
                    created_at: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_to: None,
                })
                .map_err(|e| e.to_string())?;
            store
                .add_edge(&memory_core::MemoryEdge {
                    source_id: "m_b".to_string(),
                    target_id: "m_c".to_string(),
                    relation: "supports".to_string(),
                    weight: 0.8,
                    metadata: json!({}),
                    created_at: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_to: None,
                })
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed graph");

    let result = server
        .memory_graph(Parameters(MemoryGraphParams {
            memory_id: Some("m_a".to_string()),
            query: None,
            path_prefix: None,
            project: None,
            top_k: 3,
            depth: 2,
        }))
        .await
        .expect("memory_graph should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["status"], json!("completed"));
    assert!(json["node_count"].as_u64().unwrap_or(0) >= 2);
    assert!(json["edge_count"].as_u64().unwrap_or(0) >= 1);
}

#[tokio::test]
async fn memory_graph_caps_query_seed_top_k() {
    let server = make_server();

    server
        .with_global_store(|store| {
            for idx in 0..(crate::MAX_SEARCH_TOP_K + 25) {
                let mut entry = make_entry(&format!("graph_cap_{idx}"));
                entry.path = format!("/graph/cap/{idx}");
                entry.topic = "graph cap sentinel".to_string();
                entry.summary = format!("graph cap sentinel summary {idx}");
                entry.text = format!("graph cap sentinel searchable row {idx}");
                entry.keywords = vec!["graph".to_string(), "cap".to_string()];
                store.upsert(&entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed graph cap memories");

    let result = server
        .memory_graph(Parameters(MemoryGraphParams {
            memory_id: None,
            query: Some("graph cap sentinel".to_string()),
            path_prefix: Some("/graph/cap".to_string()),
            project: None,
            top_k: 10_000,
            depth: 1,
        }))
        .await
        .expect("memory_graph query should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["status"], json!("completed"));
    assert_eq!(
        json["node_count"].as_u64(),
        Some(crate::MAX_SEARCH_TOP_K as u64)
    );
}

#[tokio::test]
async fn get_edges_explicit_named_project_read_uses_read_only_project_route() {
    let (server, _temp_home) = make_server_with_temp_home();
    let project = "graph_edges_named_read";
    let db_path = crate::path_utils::plan_c_global_db_path(project);
    std::fs::create_dir_all(db_path.parent().expect("named project DB parent"))
        .expect("create named project DB parent");
    let db_str = db_path.to_str().expect("named project DB path");
    let mut store =
        memory_core::MemoryStore::open_with_label(db_str, project).expect("open named project DB");

    let mut source = make_entry("project_edge_source");
    source.text = "Project-only graph edge source".to_string();
    let mut target = make_entry("project_edge_target");
    target.text = "Project-only graph edge target".to_string();
    store.upsert(&source).expect("seed project source");
    store.upsert(&target).expect("seed project target");
    store
        .add_edge(&memory_core::MemoryEdge {
            source_id: "project_edge_source".to_string(),
            target_id: "project_edge_target".to_string(),
            relation: "supports".to_string(),
            weight: 1.0,
            metadata: json!({}),
            created_at: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_to: None,
        })
        .expect("seed project edge");
    drop(store);

    let result = server
        .get_edges(Parameters(GetEdgesParams {
            memory_id: "project_edge_source".to_string(),
            direction: "outgoing".to_string(),
            relation_filter: None,
            project: Some(project.to_string()),
            scope: "project".to_string(),
        }))
        .await
        .expect("get_edges should read named project");
    let json: Value = serde_json::from_str(&result).expect("json");
    let edges = json.as_array().expect("edges array");

    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["db"], json!("project"));
    assert_eq!(edges[0]["source_id"], json!("project_edge_source"));
    assert_eq!(edges[0]["target_id"], json!("project_edge_target"));
}

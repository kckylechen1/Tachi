use super::*;

/// tachi#2126: save with supersedes closes prior validity and hides it from default search.
#[tokio::test]
async fn save_memory_supersedes_closes_validity_and_hides_from_search() {
    let server = make_server();
    let needle = format!("SupersedeCityNeedle{}", uuid::Uuid::new_v4().simple());

    let old_raw = server
        .save_memory(Parameters(SaveMemoryParams {
            text: format!("{needle} Alice lives in Berlin"),
            summary: format!("{needle} Berlin"),
            path: format!("/scratch/supersede/{needle}/old"),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "residence".to_string(),
            keywords: vec![needle.clone(), "berlin".into()],
            persons: Vec::new(),
            entities: vec!["Alice".into(), "Berlin".into()],
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: Some("2024-01-01T00:00:00.000Z".to_string()),
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            supersedes: Vec::new(),
        }))
        .await
        .expect("save old");
    let old_v: Value = serde_json::from_str(&old_raw).expect("json");
    assert!(old_v.get("error").is_none(), "body: {old_raw}");
    let old_id = old_v["id"].as_str().expect("old id").to_string();

    let new_raw = server
        .save_memory(Parameters(SaveMemoryParams {
            text: format!("{needle} Alice lives in Paris"),
            summary: format!("{needle} Paris"),
            path: format!("/scratch/supersede/{needle}/new"),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "residence".to_string(),
            keywords: vec![needle.clone(), "paris".into()],
            persons: Vec::new(),
            entities: vec!["Alice".into(), "Paris".into()],
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: Some("2025-06-01T00:00:00.000Z".to_string()),
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            supersedes: vec![old_id.clone()],
        }))
        .await
        .expect("save new");
    let new_v: Value = serde_json::from_str(&new_raw).expect("json");
    assert!(new_v.get("error").is_none(), "body: {new_raw}");
    let new_id = new_v["id"].as_str().expect("new id").to_string();
    let report = new_v.get("supersedes").expect("supersedes report");
    assert_eq!(
        report["applied"],
        json!([old_id]),
        "expected applied supersede: {new_raw}"
    );

    let (superseded_by, valid_until): (Option<String>, Option<String>) = server
        .with_global_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT superseded_by, valid_until FROM memories WHERE id = ?1",
                    [&old_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|e| e.to_string())
        })
        .expect("query old");
    assert_eq!(superseded_by.as_deref(), Some(new_id.as_str()));
    assert!(
        valid_until.is_some(),
        "valid_until must close on supersession"
    );

    // Default (no deep path_prefix): superseded rows are filtered out.
    // Deep prefixes (>=3 segments) intentionally surface history via
    // scoped_path_can_surface_superseded — that is not the default recall path.
    let search = server
        .search_memory(Parameters(SearchMemoryParams {
            query: needle.clone(),
            query_vec: None,
            top_k: 10,
            path_prefix: Some("/scratch".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
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
            conversation_scope: None,
            auto_conversation_scope: false,
            multi_evidence_pack: false,
            as_of: None,
            include_superseded: false,
            include_metadata: false,
            format: Some("json".to_string()),
        }))
        .await
        .expect("search");
    let rows: Value = serde_json::from_str(&search).expect("search json");
    let ids: Vec<String> = rows
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    assert!(
        ids.contains(&new_id),
        "current fact missing from search: {search}"
    );
    assert!(
        !ids.contains(&old_id),
        "superseded fact must be hidden by default: {search}"
    );

    // When history is explicitly requested, current still ranks above prior.
    let with_hist = server
        .search_memory(Parameters(SearchMemoryParams {
            query: needle.clone(),
            query_vec: None,
            top_k: 10,
            path_prefix: Some("/scratch".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
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
            conversation_scope: None,
            auto_conversation_scope: false,
            multi_evidence_pack: false,
            as_of: None,
            include_superseded: true,
            include_metadata: false,
            format: Some("json".to_string()),
        }))
        .await
        .expect("search with history");
    let hist_rows: Vec<Value> = serde_json::from_str(&with_hist).expect("json");
    let hist_ids: Vec<String> = hist_rows
        .iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    assert!(hist_ids.contains(&old_id), "history missing: {with_hist}");
    assert!(
        hist_ids.contains(&new_id),
        "current missing from history search: {with_hist}"
    );
    let pos_new = hist_ids.iter().position(|id| id == &new_id).unwrap();
    let pos_old = hist_ids.iter().position(|id| id == &old_id).unwrap();
    assert!(
        pos_new < pos_old,
        "current fact must rank above superseded prior: {with_hist}"
    );

    // get(id) still returns the historical row (not archived). Column
    // superseded_by is asserted via SQL above; slim get surfaces closed
    // valid_until so agents can see the fact was retired.
    let hit = crate::memory_ops::handle_get_memory(
        &server,
        GetMemoryParams {
            id: old_id.clone(),
            include_archived: false,
            project: None,
            as_of: None,
        },
    )
    .await
    .expect("get old");
    let got: Value = serde_json::from_str(&hit).expect("get json");
    assert!(got.get("error").is_none(), "body: {hit}");
    assert_eq!(got["id"], json!(old_id));
    assert!(
        got.get("valid_until").and_then(|v| v.as_str()).is_some(),
        "get must surface closed valid_until for superseded history: {hit}"
    );
    assert_ne!(
        got["archived"],
        json!(true),
        "must not archive on explicit supersede: {hit}"
    );
}

#[tokio::test]
async fn save_memory_supersedes_already_superseded_is_idempotent() {
    let server = make_server();
    let needle = format!("SupersedeIdemNeedle{}", uuid::Uuid::new_v4().simple());

    let old_raw = server
        .save_memory(Parameters(SaveMemoryParams {
            text: format!("{needle} favorite color is blue"),
            summary: String::new(),
            path: format!("/scratch/supersede/{needle}/old"),
            importance: 0.6,
            category: "preference".to_string(),
            topic: String::new(),
            keywords: vec![needle.clone()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            supersedes: Vec::new(),
        }))
        .await
        .expect("save old");
    let old_id = serde_json::from_str::<Value>(&old_raw).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mid_raw = server
        .save_memory(Parameters(SaveMemoryParams {
            text: format!("{needle} favorite color is green"),
            summary: String::new(),
            path: format!("/scratch/supersede/{needle}/mid"),
            importance: 0.6,
            category: "preference".to_string(),
            topic: String::new(),
            keywords: vec![needle.clone()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            supersedes: vec![old_id.clone()],
        }))
        .await
        .expect("first supersede");
    let mid_v: Value = serde_json::from_str(&mid_raw).expect("json");
    let mid_id = mid_v["id"].as_str().unwrap().to_string();
    assert_eq!(mid_v["supersedes"]["applied"], json!([old_id]), "{mid_raw}");

    let raw2 = server
        .save_memory(Parameters(SaveMemoryParams {
            text: format!("{needle} favorite color is red"),
            summary: String::new(),
            path: format!("/scratch/supersede/{needle}/new"),
            importance: 0.6,
            category: "preference".to_string(),
            topic: String::new(),
            keywords: vec![needle.clone()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            supersedes: vec![old_id.clone()],
        }))
        .await
        .expect("second supersede");
    let v2: Value = serde_json::from_str(&raw2).expect("json");
    let report = &v2["supersedes"];
    let applied = report["applied"].as_array().cloned().unwrap_or_default();
    let prior = report["prior_idempotent"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let refused = report["refused"].as_array().cloned().unwrap_or_default();
    assert!(
        applied.is_empty(),
        "already-superseded must not re-apply: {raw2}"
    );
    assert!(
        !prior.is_empty() || !refused.is_empty(),
        "expected prior_idempotent or refused: {raw2}"
    );

    let superseded_by: Option<String> = server
        .with_global_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = ?1",
                    [&old_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())
        })
        .expect("query");
    assert_eq!(superseded_by.as_deref(), Some(mid_id.as_str()));
}

#[tokio::test]
async fn search_include_superseded_surfaces_history() {
    let server = make_server();
    let needle = format!("SupersedeHistNeedle{}", uuid::Uuid::new_v4().simple());

    let old_raw = server
        .save_memory(Parameters(SaveMemoryParams {
            text: format!("{needle} lives in Berlin"),
            summary: String::new(),
            path: format!("/scratch/supersede/{needle}/old"),
            importance: 0.5,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![needle.clone()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            supersedes: Vec::new(),
        }))
        .await
        .expect("save old");
    let old_id = serde_json::from_str::<Value>(&old_raw).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let new_raw = server
        .save_memory(Parameters(SaveMemoryParams {
            text: format!("{needle} lives in Paris"),
            summary: String::new(),
            path: format!("/scratch/supersede/{needle}/new"),
            importance: 0.5,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![needle.clone()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
            supersedes: vec![old_id.clone()],
        }))
        .await
        .expect("save new");
    assert!(
        serde_json::from_str::<Value>(&new_raw).unwrap()["supersedes"]["applied"]
            == json!([old_id]),
        "{new_raw}"
    );

    let with_hist = server
        .search_memory(Parameters(SearchMemoryParams {
            query: needle.clone(),
            query_vec: None,
            top_k: 10,
            path_prefix: Some(format!("/scratch/supersede/{needle}")),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
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
            conversation_scope: None,
            auto_conversation_scope: false,
            multi_evidence_pack: false,
            as_of: None,
            include_superseded: true,
            include_metadata: false,
            format: Some("json".to_string()),
        }))
        .await
        .expect("search");
    let rows: Value = serde_json::from_str(&with_hist).expect("json");
    let ids: Vec<String> = rows
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    assert!(
        ids.contains(&old_id),
        "include_superseded must surface history: {with_hist}"
    );
}

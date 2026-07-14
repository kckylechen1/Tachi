use super::*;

fn adapter_params(action: &str) -> TachiDomainAdapterParams {
    TachiDomainAdapterParams {
        action: action.to_string(),
        project: None,
        project_explicit: false,
        domain: None,
        actor: None,
        session_id: None,
        character: None,
        entries: Vec::new(),
        project_events: false,
        dry_run: false,
    }
}

fn event_params(action: &str) -> TachiEventParams {
    TachiEventParams {
        action: action.to_string(),
        format: Some("json".to_string()),
        id: None,
        source_repo: None,
        adapter: None,
        project: None,
        project_explicit: false,
        domain: None,
        session_id: None,
        actor: None,
        event_type: None,
        authority: None,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: None,
        provenance: None,
        created_at: None,
        limit: 20,
        path_prefix: None,
        dry_run: false,
    }
}

#[tokio::test]
async fn lorebook_adapter_imports_romanbath_entries_as_worldbook_context() {
    let (server, home) = make_server_with_temp_home();
    let db_path = home
        .temp_home
        .join(".tachi/projects/romanbath-test/memory.db");
    std::fs::create_dir_all(db_path.parent().expect("named project parent"))
        .expect("create named project dir");
    let db_str = db_path.to_str().expect("utf-8 db path");
    memcore::MemoryStore::open_with_label(db_str, "romanbath-test")
        .expect("create named project DB");

    let mut import = adapter_params("lorebook_import");
    import.project = Some("romanbath-test".to_string());
    import.domain = Some("agent_os".to_string());
    import.character = Some("Mara".to_string());
    import.project_events = true;
    import.entries = vec![json!({
        "id": "entry-1",
        "keys": ["rain"],
        "secondary_keys": ["harbor"],
        "content": "Rain at the harbor changes Mara's route.",
        "enabled": true,
        "selective": true,
        "position": "before_char",
        "token_budget": 80,
        "priority": 12,
        "recursive": false
    })];

    let imported = crate::domain_adapter_ops::handle_tachi_domain_adapter(&server, import)
        .await
        .expect("import lorebook");
    let imported_json: Value = serde_json::from_str(&imported).expect("import JSON");
    assert_eq!(imported_json["adapter"], json!("lorebook_worldbook"));
    assert_eq!(imported_json["imported_count"], json!(1));
    assert_eq!(imported_json["projection"]["projected_count"], json!(1));

    let mut context = event_params("context");
    context.project = Some("romanbath-test".to_string());
    context.projection_hints = vec!["world_book".to_string()];
    let body = crate::event_ops::handle_tachi_event(&server, context)
        .await
        .expect("worldbook context");
    let parsed: Value = serde_json::from_str(&body).expect("context JSON");
    assert_eq!(parsed["lorebook"][0]["lorebook"]["keys"], json!(["rain"]));
    assert_eq!(
        parsed["lorebook"][0]["lorebook"]["secondary_keys"],
        json!(["harbor"])
    );
    assert_eq!(parsed["lorebook"][0]["lorebook"]["token_budget"], json!(80));
}

/// #1114 codex round-1 B1 discriminating test: a `project=` value that
/// arrived on `TachiDomainAdapterParams` as a TRANSPORT-injected session
/// default (`project_explicit: false` — never a caller decision) must still
/// carry that `false` through to the `TachiEventParams` this adapter emits,
/// so the S1 write-affinity gate (exercised via `action=project`'s
/// projection materialization) still scrutinizes it. Before the B1 fix,
/// `domain_adapter_ops::lorebook_import` re-derived `project_explicit` as
/// `project.is_some()` — always `true` whenever `project` was set at all —
/// which unconditionally treated this transport default as a deliberate
/// caller override and skipped the gate, silently landing the projection in
/// the wrong (session-bound) store instead of the one `agent_os` is
/// registered to.
#[tokio::test]
async fn lorebook_import_preserves_transport_default_marker_through_write_affinity() {
    let (server, home) = make_server_with_temp_home();

    // "romanbath-test" is where the transport-injected default points (e.g.
    // the session's bound project) — NOT where `agent_os` content actually
    // belongs.
    let bound_db = home
        .temp_home
        .join(".tachi/projects/romanbath-test/memory.db");
    std::fs::create_dir_all(bound_db.parent().expect("bound project parent"))
        .expect("create bound project dir");
    memcore::MemoryStore::open_with_label(
        bound_db.to_str().expect("utf-8 db path"),
        "romanbath-test",
    )
    .expect("create bound project DB");

    // "hapi" is the store `agent_os` is registered to — mounted and ready.
    let hapi_db = home.temp_home.join(".tachi/projects/hapi/memory.db");
    std::fs::create_dir_all(hapi_db.parent().expect("hapi parent")).expect("create hapi dir");
    memcore::MemoryStore::open_with_label(hapi_db.to_str().expect("utf-8 db path"), "hapi")
        .expect("create hapi DB");

    std::fs::write(
        home.temp_home.join(".tachi/routing.json"),
        r#"{"domain_routes":[{"project":"hapi","domains":["agent_os"]}]}"#,
    )
    .expect("write routing.json");

    let mut import = adapter_params("lorebook_import");
    import.project = Some("romanbath-test".to_string()); // transport-injected default
    import.project_explicit = false; // NOT a caller decision
    import.domain = Some("agent_os".to_string());
    import.character = Some("Mara".to_string());
    import.project_events = true;
    import.entries = vec![json!({
        "id": "entry-1",
        "keys": ["rain"],
        "content": "Rain at the harbor changes Mara's route.",
        "enabled": true
    })];

    crate::domain_adapter_ops::handle_tachi_domain_adapter(&server, import)
        .await
        .expect("import lorebook");

    // The world_book projection must have rerouted into "hapi" — the
    // registered store for agent_os content — not silently landed in the
    // transport-default-bound "romanbath-test".
    let in_hapi = server
        .with_named_project_store_read("hapi", |store| {
            store
                .list_by_path("/lorebook", 10, false)
                .map_err(|e| e.to_string())
        })
        .expect("read hapi lorebook");
    assert_eq!(
        in_hapi.len(),
        1,
        "agent_os lorebook projection must reroute into hapi: {in_hapi:?}"
    );

    let in_bound = server
        .with_named_project_store_read("romanbath-test", |store| {
            store
                .list_by_path("/lorebook", 10, false)
                .map_err(|e| e.to_string())
        })
        .expect("read romanbath-test lorebook");
    assert!(
        in_bound.is_empty(),
        "must NOT silently land in the transport-default-bound store: {in_bound:?}"
    );
}

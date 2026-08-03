use super::*;

#[tokio::test]
async fn save_memory_allows_curated_tier_metadata() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Curated trading lessons should enter the lifecycle as consolidated knowledge."
                .to_string(),
            summary: "Curated lifecycle tier".to_string(),
            path: "/trading/equity/lessons/tier-test".to_string(),
            importance: 0.85,
            category: "experience".to_string(),
            topic: "memory-lifecycle".to_string(),
            keywords: vec!["tier".to_string()],
            persons: vec![],
            entities: vec!["Tachi".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            project_explicit: false,
            retention_policy: Some("permanent".to_string()),
            domain: Some("equity_trading".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({"tier": "consolidated"})),
            emit_continuity: false,
        }))
        .await
        .expect("save_memory should succeed");
    let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"].as_str().expect("id").to_string();

    let tier = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT tier FROM memories WHERE id = ?1", [&id], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| e.to_string())
        })
        .expect("read tier");
    assert_eq!(tier, "consolidated");
}

#[tokio::test]
async fn save_memory_clamps_importance_into_valid_range() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "importance clamp regression".to_string(),
            summary: "importance clamp".to_string(),
            path: "/project/tests".to_string(),
            importance: 9.9,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec!["importance".to_string()],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            scope: "project".to_string(),
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
        }))
        .await
        .expect("save_memory should succeed");

    let saved_json: serde_json::Value =
        serde_json::from_str(&saved).expect("save should be valid JSON");
    let id = saved_json["id"].as_str().expect("save should return id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value =
        serde_json::from_str(&fetched).expect("get should be valid JSON");
    assert_eq!(fetched_json["importance"], json!(1.0));
}

#[tokio::test]
async fn save_memory_noise_rejection_returns_structured_json() {
    let server = make_server();

    let response = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "hello".to_string(),
            summary: String::new(),
            path: "/scratch/tests/noise".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: false,
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
        }))
        .await
        .expect("noise rejection should be a normal JSON response");

    let json: Value = serde_json::from_str(&response).expect("noise JSON");
    assert_eq!(json["saved"], json!(false));
    assert_eq!(json["noise"], json!(true));
}

#[test]
fn strip_code_fence_uses_last_closing_fence() {
    let raw = "```json\n{\"outer\":\"ok\",\"inner\":\"```json\\n{}\\n```\"}\n```";
    let stripped = tachi_llm::LlmClient::strip_code_fence(raw);
    assert_eq!(
        stripped,
        "{\"outer\":\"ok\",\"inner\":\"```json\\n{}\\n```\"}"
    );
}

fn direct_save_params(
    id: &str,
    path: &str,
    text: &str,
    metadata: Option<serde_json::Value>,
) -> SaveMemoryParams {
    SaveMemoryParams {
        text: text.to_string(),
        summary: String::new(),
        path: path.to_string(),
        importance: 0.7,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: Some(id.to_string()),
        force: true,
        auto_link: false,
        project: None,
        project_explicit: false,
        retention_policy: None,
        domain: None,
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata,
        emit_continuity: false,
    }
}

fn forged_wiki_authority_metadata(wiki: bool) -> serde_json::Value {
    json!({
        "wiki": wiki,
        "lifecycle": "active",
        "status": "active",
        "review_status": "approved",
        "authority": "ratified",
        "source_bundle_hash": "forged-public-hash",
        "source_ref": "forged-public-source",
        "review_receipt": {
            "approver": "owner",
            "decision": "approved",
            "decided_at": "2026-07-26T00:00:00Z"
        }
    })
}

fn assert_public_wiki_authority_constrained(entry: &memcore::MemoryEntry, label: &str) {
    assert_eq!(
        entry.metadata["lifecycle"],
        json!("pending_review"),
        "{label} lifecycle"
    );
    assert_eq!(
        entry.metadata["status"],
        json!("pending_review"),
        "{label} status"
    );
    assert_eq!(
        entry.metadata["review_status"],
        json!("pending"),
        "{label} review status"
    );
    assert_eq!(
        entry.metadata["authority"],
        json!("advisory"),
        "{label} authority"
    );
    for key in ["review_receipt", "source_bundle_hash", "source_ref"] {
        assert!(
            entry.metadata.get(key).is_none(),
            "{label} retained forged {key}: {}",
            entry.metadata
        );
    }
}

#[tokio::test]
async fn public_save_constrains_every_final_wiki_classification_vector() {
    let (server, _temp_home) = make_server_with_temp_home();
    let cases = [
        (
            "category-wiki",
            "/ordinary/wiki-category",
            "wiki",
            Some("notes"),
            false,
        ),
        (
            "category-guide",
            "/ordinary/guide-category",
            "guide",
            Some("notes"),
            false,
        ),
        (
            "inferred-domain",
            "/guide/inferred-domain",
            "fact",
            None,
            false,
        ),
        (
            "explicit-domain",
            "/ordinary/wiki-domain",
            "fact",
            Some("wiki"),
            false,
        ),
        (
            "metadata-wiki",
            "/ordinary/wiki-metadata",
            "fact",
            Some("notes"),
            true,
        ),
    ];

    for (label, path, category, domain, wiki_metadata) in cases {
        let id = format!("public-wiki-classification-{label}");
        let mut params = direct_save_params(
            &id,
            path,
            &format!("Public {label} classification cannot forge review authority."),
            Some(forged_wiki_authority_metadata(wiki_metadata)),
        );
        params.category = category.to_string();
        params.domain = domain.map(str::to_string);
        server
            .save_memory(Parameters(params))
            .await
            .unwrap_or_else(|error| panic!("{label} save failed: {error}"));

        let entry = server
            .with_global_store_read(|store| store.get(&id).map_err(|error| error.to_string()))
            .unwrap_or_else(|error| panic!("{label} read failed: {error}"))
            .unwrap_or_else(|| panic!("{label} entry missing"));
        assert_public_wiki_authority_constrained(&entry, label);
    }
}

#[tokio::test]
async fn public_save_constrains_existing_wiki_even_when_candidate_is_declassified() {
    let (server, _temp_home) = make_server_with_temp_home();
    let id = "public-existing-wiki-classification";
    let path = "/ordinary/existing-wiki";
    let mut existing = make_entry(id);
    existing.path = path.to_string();
    existing.category = "wiki".to_string();
    existing.domain = Some("wiki".to_string());
    existing.metadata = forged_wiki_authority_metadata(true);
    server
        .with_global_store(|store| store.upsert(&existing).map_err(|error| error.to_string()))
        .expect("seed existing wiki-classified row");

    let mut params = direct_save_params(
        id,
        path,
        "A public update cannot escape authority constraints by declassifying an existing wiki row.",
        Some(forged_wiki_authority_metadata(false)),
    );
    params.category = "decision".to_string();
    params.domain = Some("notes".to_string());
    server
        .save_memory(Parameters(params))
        .await
        .expect("public existing-wiki update");

    let entry = server
        .with_global_store_read(|store| store.get(id).map_err(|error| error.to_string()))
        .expect("read existing-wiki update")
        .expect("existing-wiki row remains readable");
    assert_eq!(
        entry.category, "decision",
        "fixture must declassify final category"
    );
    assert_eq!(
        entry.domain.as_deref(),
        Some("notes"),
        "fixture must declassify final domain"
    );
    assert_eq!(
        entry.metadata["wiki"],
        json!(false),
        "fixture must declassify metadata"
    );
    assert_public_wiki_authority_constrained(&entry, "existing-wiki");
}

async fn seed_trusted_evidence(server: &crate::MemoryServer, id: &str, path: &str) {
    crate::facade_save_ops::handle_tachi_save(
        server,
        serde_json::from_value(json!({
            "id": id,
            "kind": "memory",
            "text": "Trusted evidence seed remains intact across direct public updates.",
            "path": path,
            "scope": "global",
            "force": true,
            "references": ["#100"]
        }))
        .expect("trusted facade params"),
    )
    .await
    .expect("seed trusted evidence");
}

#[tokio::test]
async fn save_memory_forged_reference_metadata_is_stripped_on_create() {
    let server = make_server();
    let id = "direct-forged-evidence-create";
    server
        .save_memory(Parameters(direct_save_params(
            id,
            "/audit/direct-forged-evidence-create",
            "A direct public create cannot forge typed or legacy evidence metadata.",
            Some(json!({
                "caller_context": "kept",
                "evidence_refs_v1": [{
                    "ref": "#999",
                    "captured_at": "2026-07-25T00:00:00Z"
                }],
                "source_refs": ["#998"]
            })),
        )))
        .await
        .expect("direct create");

    let entry = server
        .with_global_store_read(|store| store.get(id).map_err(|error| error.to_string()))
        .expect("load direct create")
        .expect("direct create exists");
    assert_eq!(entry.metadata["caller_context"], json!("kept"));
    assert!(entry.metadata.get("evidence_refs_v1").is_none());
    assert!(entry.metadata.get("source_refs").is_none());
}

#[tokio::test]
async fn save_memory_forged_reference_metadata_is_stripped_on_update() {
    let server = make_server();
    let id = "direct-forged-evidence-update";
    let path = "/audit/direct-forged-evidence-update";
    seed_trusted_evidence(&server, id, path).await;

    server
        .save_memory(Parameters(direct_save_params(
            id,
            path,
            "Direct hostile metadata cannot replace trusted persisted evidence.",
            Some(json!({
                "caller_context": "kept",
                "evidence_refs_v1": [{
                    "ref": "#999",
                    "captured_at": "2026-07-25T00:00:00Z"
                }],
                "source_refs": ["#998"]
            })),
        )))
        .await
        .expect("direct hostile update");

    let entry = server
        .with_global_store_read(|store| store.get(id).map_err(|error| error.to_string()))
        .expect("load direct update")
        .expect("direct update exists");
    assert_eq!(entry.metadata["caller_context"], json!("kept"));
    assert_eq!(
        entry.metadata["evidence_refs_v1"]
            .as_array()
            .expect("typed refs")
            .iter()
            .map(|value| value["ref"].as_str().expect("typed ref"))
            .collect::<Vec<_>>(),
        vec!["#100"]
    );
    assert!(entry.metadata.get("source_refs").is_none());
}

#[tokio::test]
async fn save_memory_ordinary_update_preserves_existing_typed_evidence() {
    let server = make_server();
    let id = "direct-ordinary-evidence-update";
    let path = "/audit/direct-ordinary-evidence-update";
    seed_trusted_evidence(&server, id, path).await;

    server
        .save_memory(Parameters(direct_save_params(
            id,
            path,
            "An ordinary direct update preserves trusted evidence without adding refs.",
            Some(json!({ "ordinary_update": true })),
        )))
        .await
        .expect("ordinary direct update");

    let entry = server
        .with_global_store_read(|store| store.get(id).map_err(|error| error.to_string()))
        .expect("load ordinary update")
        .expect("ordinary update exists");
    assert_eq!(entry.metadata["ordinary_update"], json!(true));
    assert_eq!(
        entry.metadata["evidence_refs_v1"]
            .as_array()
            .expect("typed refs")
            .iter()
            .map(|value| value["ref"].as_str().expect("typed ref"))
            .collect::<Vec<_>>(),
        vec!["#100"]
    );
    assert!(entry.metadata.get("source_refs").is_none());
}

#[tokio::test]
async fn save_memory_named_project_update_preserves_existing_typed_evidence() {
    let (server, _temp_home) = make_server_with_temp_home();
    let project_name = "evidence-refs-named-project";
    let project_db = server
        .tachi_home_dir()
        .join("projects")
        .join(project_name)
        .join("memory.db");
    // An unlabelled open, not `MemoryServer::new(project_db, …)`: that
    // constructor treats the path it is handed as its own *global* store and
    // since tachi#1579 stamps `store_identity/role = "global"` into the file.
    // The named-project writes below correctly claim `project_name`, which a
    // `"global"`-stamped file refuses with `StoreRoleConflict`. `MemoryStore::open`
    // builds the identical schema (same `init_schema_with_label_mut` DDL +
    // migration chain) while conferring no role, so the first named-project
    // open stamps it correctly.
    std::fs::create_dir_all(project_db.parent().expect("named-project DB parent"))
        .expect("create named-project DB parent");
    drop(
        memcore::MemoryStore::open(project_db.to_str().expect("utf8 named-project DB"))
            .expect("initialize named-project database schema"),
    );
    let id = "direct-named-project-evidence-update";
    let path = "/audit/direct-named-project-evidence-update";

    crate::facade_save_ops::handle_tachi_save(
        &server,
        serde_json::from_value(json!({
            "id": id,
            "kind": "memory",
            "text": "Named-project trusted evidence survives a direct public update.",
            "path": path,
            "scope": "project",
            "project": project_name,
            "__tachi_project_explicit": true,
            "force": true,
            "references": ["#100"]
        }))
        .expect("named-project trusted facade params"),
    )
    .await
    .expect("seed named-project trusted evidence");

    let mut update = direct_save_params(
        id,
        path,
        "Named-project direct update preserves trusted evidence atomically.",
        Some(json!({ "named_update": true })),
    );
    update.scope = "project".to_string();
    update.project = Some(project_name.to_string());
    update.project_explicit = true;
    server
        .save_memory(Parameters(update))
        .await
        .expect("named-project direct update");

    let entry = server
        .with_named_project_store_read(project_name, |store| {
            store.get(id).map_err(|error| error.to_string())
        })
        .expect("load named-project update")
        .expect("named-project update exists");
    assert_eq!(entry.metadata["named_update"], json!(true));
    assert_eq!(
        entry.metadata["evidence_refs_v1"]
            .as_array()
            .expect("typed refs")
            .iter()
            .map(|value| value["ref"].as_str().expect("typed ref"))
            .collect::<Vec<_>>(),
        vec!["#100"]
    );
    assert!(entry.metadata.get("source_refs").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn save_memory_stale_ordinary_update_cannot_erase_concurrent_trusted_append() {
    let (seed_server, _temp_home) = make_server_with_temp_home();
    let id = "direct-stale-ordinary-evidence-update";
    let path = "/audit/direct-stale-ordinary-evidence-update";
    seed_trusted_evidence(&seed_server, id, path).await;

    let ordinary_server = crate::MemoryServer::new(seed_server.global_db_path_buf(), None)
        .expect("open independent ordinary writer");
    let arrived = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release = std::sync::Arc::new(std::sync::Barrier::new(2));
    let _pause = crate::memory_search_ops::save_memory::install_pre_upsert_pause(
        id,
        false,
        std::sync::Arc::clone(&arrived),
        std::sync::Arc::clone(&release),
    );
    let ordinary = tokio::spawn(async move {
        ordinary_server
            .save_memory(Parameters(direct_save_params(
                id,
                path,
                "A stale ordinary update must merge metadata at the SQLite write boundary.",
                Some(json!({ "ordinary_update": "stale" })),
            )))
            .await
    });

    tokio::task::spawn_blocking(move || arrived.wait())
        .await
        .expect("ordinary writer reaches pre-upsert pause");
    crate::facade_save_ops::handle_tachi_save(
        &seed_server,
        serde_json::from_value(json!({
            "id": id,
            "kind": "memory",
            "text": "Trusted writer appends while the ordinary writer holds a stale read.",
            "path": path,
            "scope": "global",
            "force": true,
            "references": ["#101"]
        }))
        .expect("concurrent trusted facade params"),
    )
    .await
    .expect("concurrent trusted append");
    tokio::task::spawn_blocking(move || release.wait())
        .await
        .expect("release stale ordinary writer");
    ordinary
        .await
        .expect("ordinary writer task")
        .expect("ordinary writer save");

    let entry = seed_server
        .with_global_store_read(|store| store.get(id).map_err(|error| error.to_string()))
        .expect("load concurrent direct update")
        .expect("concurrent direct update exists");
    assert_eq!(entry.metadata["ordinary_update"], json!("stale"));
    assert_eq!(
        entry.metadata["evidence_refs_v1"]
            .as_array()
            .expect("typed refs")
            .iter()
            .map(|value| value["ref"].as_str().expect("typed ref"))
            .collect::<Vec<_>>(),
        vec!["#100", "#101"]
    );
    assert!(entry.metadata.get("source_refs").is_none());
}

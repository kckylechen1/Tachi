use super::*;

#[tokio::test]
async fn tachi_memory_save_with_id_preserves_unspecified_fields() {
    let server = make_server();
    let memory_id = "save-patch-preserves-fields";

    let mut first = tachi_memory_params("save");
    first.format = Some("json".to_string());
    first.id = Some(memory_id.to_string());
    first.force = true;
    first.scope = Some("project".to_string());
    first.kind = Some("memory".to_string());
    first.category = Some("decision".to_string());
    first.path = Some("/scratch/save-patch".to_string());
    first.text = Some("Original durable memory body.".to_string());
    first.summary = Some("Original summary".to_string());
    first.topic = Some("save-patch-topic".to_string());
    first.keywords = vec!["patch".to_string(), "preserve".to_string()];
    first.entities = vec!["Tachi".to_string()];
    first.importance = Some(0.82);
    first.retention_policy = Some("durable".to_string());
    first.domain = Some("memory".to_string());
    first.metadata = Some(json!({"tier": "consolidated", "source": "test"}));
    first.valid_from = Some("2026-07-01T00:00:00Z".to_string());
    first.valid_until = Some("2026-12-31T00:00:00Z".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, first)
        .await
        .expect("first save succeeds");
    let original_entry = server
        .with_global_store_read(|store| store.get(memory_id).map_err(|e| e.to_string()))
        .expect("read original memory")
        .expect("original memory exists");

    let mut patch = tachi_memory_params("save");
    patch.format = Some("json".to_string());
    patch.id = Some(memory_id.to_string());
    patch.force = true;
    patch.scope = Some("project".to_string());
    patch.text = Some("Patched body only.".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, patch)
        .await
        .expect("patch save succeeds");

    let entry = server
        .with_global_store_read(|store| store.get(memory_id).map_err(|e| e.to_string()))
        .expect("read patched memory")
        .expect("patched memory exists");
    assert_eq!(entry.text, "Patched body only.");
    assert_eq!(entry.summary, "Original summary");
    assert_eq!(entry.category, "decision");
    assert_eq!(entry.path, "/scratch/save-patch");
    assert_eq!(entry.topic, "save-patch-topic");
    assert_eq!(entry.keywords, vec!["patch", "preserve"]);
    assert_eq!(entry.entities, vec!["Tachi"]);
    assert_eq!(entry.importance, 0.82);
    assert_eq!(entry.retention_policy.as_deref(), Some("durable"));
    assert_eq!(entry.domain.as_deref(), Some("memory"));
    assert_eq!(entry.metadata["tier"], json!("consolidated"));
    assert_eq!(entry.metadata["source"], json!("test"));
    assert_eq!(entry.valid_from, original_entry.valid_from);
    assert_eq!(entry.valid_until, original_entry.valid_until);
}

#[tokio::test]
async fn tachi_memory_save_persists_programming_agent_fields() {
    let server = make_server();
    let mem_id = "mcp-agent-fields-001";

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "save".to_string(),
            issue_ref: None,
            format: Some("markdown".to_string()),
            query: None,
            scope: Some("project".to_string()),
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: Some("fact".to_string()),
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            agent_role: None,
            text: Some("Refactored MCP save path for coding agents.".to_string()),
            title: None,
            summary: Some("MCP agent fields".to_string()),
            topic: Some("mcp".to_string()),
            keywords: vec!["rust".to_string(), "mcp".to_string()],
            entities: vec!["tachi-server".to_string(), "sigil".to_string()],
            importance: Some(0.75),
            retention_policy: None,
            kind: Some("memory".to_string()),
            path: Some("/project/sigil/mcp".to_string()),
            id: Some(mem_id.to_string()),
            force: true,
            source: None,
            valid_from: None,
            valid_until: None,
            project: None,
            project_explicit: false,
            domain: Some("rust".to_string()),
            metadata: None,
            compact: false,
            files: Vec::new(),
            references: Vec::new(),
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
        },
    )
    .await
    .expect("save should succeed");

    assert!(body.contains("Saved ->"));
    assert!(body.contains(&format!("`{mem_id}`")));

    let db_path = server.global_db_path_buf();
    let conn = rusqlite::Connection::open(db_path).expect("open test db");
    let (keywords, entities, domain, path): (String, String, Option<String>, String) = conn
        .query_row(
            "SELECT keywords, entities, domain, path FROM memories WHERE id=?1",
            [mem_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("row exists");
    assert_eq!(keywords, r#"["rust","mcp"]"#);
    assert_eq!(entities, r#"["tachi-server","sigil"]"#);
    assert_eq!(domain.as_deref(), Some("rust"));
    assert_eq!(path, "/project/sigil/mcp");
}

/// tachi#1288 (Fix B): `references[]` used to be silently dropped by the
/// plain "memory" save path -- only `kind="wiki"` consumed it. A memory save
/// with `references` must land as typed `metadata.evidence_refs_v1` (the
/// #1285-preferred shape; see `wiki_ops::provenance::preferred_wiki_references`)
/// -- not the legacy `metadata.source_refs` string array `tachi_wiki_write`
/// uses -- so a caller can read the evidence back via `get`/`search`.
#[tokio::test]
async fn tachi_memory_save_with_references_records_evidence_refs_v1() {
    let server = make_server();
    let memory_id = "save-with-references-001";

    let mut params = tachi_memory_params("save");
    params.id = Some(memory_id.to_string());
    params.force = true;
    params.kind = Some("memory".to_string());
    params.path = Some("/scratch/save-with-references".to_string());
    params.text = Some("Decision backed by an external doc and an issue.".to_string());
    params.references = vec!["https://example.com/doc".to_string(), "#1288".to_string()];
    crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("save with references succeeds");

    let entry = server
        .with_global_store_read(|store| store.get(memory_id).map_err(|e| e.to_string()))
        .expect("read memory")
        .expect("memory exists");

    let refs = entry.metadata["evidence_refs_v1"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "evidence_refs_v1 must be present as an array: {:?}",
                entry.metadata
            )
        });
    assert_eq!(refs.len(), 2, "expected both references recorded: {refs:?}");
    let ref_strings: Vec<&str> = refs
        .iter()
        .filter_map(|r| r.get("ref").and_then(Value::as_str))
        .collect();
    assert_eq!(ref_strings, vec!["https://example.com/doc", "#1288"]);
    assert!(
        entry.metadata.get("source_refs").is_none(),
        "memory save path must write the typed evidence_refs_v1 shape only, \
         not the legacy source_refs array wiki writes: {:?}",
        entry.metadata
    );
}

/// tachi#1288 (Fix B): an invalid reference must be a loud, rejected error --
/// not silently dropped -- on the "memory" save path, exactly like
/// `tachi_wiki_write` already rejects one via the same
/// `wiki_ops::validate_references` gate.
#[tokio::test]
async fn tachi_memory_save_rejects_invalid_reference_format() {
    let server = make_server();

    let mut params = tachi_memory_params("save");
    params.force = true;
    params.kind = Some("memory".to_string());
    params.path = Some("/scratch/save-invalid-reference".to_string());
    params.text = Some("Should be rejected before anything is persisted.".to_string());
    params.references = vec!["not-a-valid-reference".to_string()];

    let err = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect_err("invalid reference format must be rejected, not silently dropped");
    assert!(
        err.contains("Invalid reference format"),
        "expected the shared wiki_ops::validate_references error text: {err}"
    );
}

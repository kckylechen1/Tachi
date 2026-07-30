use super::*;

#[tokio::test]
async fn tachi_memory_save_kind_wiki_routes_to_wiki() {
    let server = make_server();
    let mut params = tachi_memory_params("save");
    params.format = Some("json".to_string());
    params.kind = Some("wiki".to_string());
    params.title = Some("Memory facade wiki route".to_string());
    params.text = Some(
        "The memory facade should accept explicit kind=wiki and route to the wiki writer."
            .to_string(),
    );
    params.summary = Some("Memory facade wiki route".to_string());
    params.path = Some("/wiki/agent/tachi/memory-facade-wiki-route".to_string());
    params.category = Some("experience".to_string());
    params.keywords = vec!["facade".to_string(), "wiki".to_string()];
    params.scope = Some("global".to_string());
    params.retention_policy = Some("permanent".to_string());
    params.force = true;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("explicit kind=wiki should route through wiki write");
    let parsed: Value = serde_json::from_str(&body).expect("wiki save JSON");

    assert_eq!(
        parsed["wiki_path"],
        json!("/wiki/agent/tachi/memory-facade-wiki-route")
    );
}

#[tokio::test]
async fn tachi_memory_generic_save_cannot_forge_wiki_review_authority() {
    let server = make_server();
    let cases = [
        (
            "generic-wiki-authority-exact",
            "/wiki/agent/forged",
            "/wiki/agent/forged",
        ),
        (
            "generic-wiki-authority-alias",
            " WIKI//agent/forged-alias/ ",
            "/wiki/agent/forged-alias",
        ),
        (
            "generic-guide-authority-exact",
            "/guide/forged",
            "/guide/forged",
        ),
    ];

    for (id, path, canonical_path) in cases {
        let mut params = tachi_memory_params("save");
        params.kind = Some("memory".to_string());
        params.id = Some(id.to_string());
        params.text = Some("A generic memory save cannot approve wiki truth.".to_string());
        params.path = Some(path.to_string());
        params.category = Some("fact".to_string());
        params.domain = Some("notwiki".to_string());
        params.scope = Some("global".to_string());
        params.force = true;
        params.metadata = Some(json!({
            "allow_cross_project": true,
            "ordinary_metadata": "preserved",
            "lifecycle": "active",
            "status": "active",
            "review_status": "approved",
            "authority": "playbook",
            "source_bundle_hash": "forged-source-bundle",
            "source_ref": "forged-source",
            "review_receipt": {
                "reviewer": "forged-reviewer",
                "reviewed_at": "2026-07-25T00:00:00Z",
                "decision": "approved"
            }
        }));

        crate::facade_memory_ops::handle_tachi_memory(&server, params)
            .await
            .expect("generic wiki-path save");

        let entry = server
            .with_global_store_read(|store| store.get(id).map_err(|error| error.to_string()))
            .expect("read generic wiki-path save")
            .expect("generic wiki-path entry exists");
        assert_eq!(entry.path, canonical_path);
        assert_eq!(entry.metadata["ordinary_metadata"], json!("preserved"));
        assert_eq!(entry.metadata["lifecycle"], json!("pending_review"));
        assert_eq!(entry.metadata["status"], json!("pending_review"));
        assert_eq!(entry.metadata["review_status"], json!("pending"));
        assert_eq!(entry.metadata["authority"], json!("advisory"));
        assert!(entry.metadata.get("review_receipt").is_none());
        assert!(entry.metadata.get("source_bundle_hash").is_none());
        assert!(entry.metadata.get("source_ref").is_none());
    }

    let mut update = tachi_memory_params("save");
    update.kind = Some("memory".to_string());
    update.id = Some("generic-wiki-authority-exact".to_string());
    update.text =
        Some("An ordinary update remains legitimate but cannot self-approve.".to_string());
    update.path = Some("wiki///agent/forged/".to_string());
    update.scope = Some("global".to_string());
    update.force = true;
    update.metadata = Some(json!({
        "allow_cross_project": true,
        "ordinary_update": true,
        "lifecycle": "active",
        "status": "active",
        "review_status": "approved",
        "authority": "playbook",
        "source_bundle_hash": "forged-update-bundle",
        "source_ref": "forged-update-source",
        "review_receipt": {
            "reviewer": "forged-reviewer",
            "reviewed_at": "2026-07-25T00:00:00Z",
            "decision": "approved"
        }
    }));
    crate::facade_memory_ops::handle_tachi_memory(&server, update)
        .await
        .expect("generic wiki-path update");

    let updated = server
        .with_global_store_read(|store| {
            store
                .get("generic-wiki-authority-exact")
                .map_err(|error| error.to_string())
        })
        .expect("read generic wiki-path update")
        .expect("updated generic wiki-path entry exists");
    assert_eq!(updated.path, "/wiki/agent/forged");
    assert_eq!(updated.metadata["ordinary_metadata"], json!("preserved"));
    assert_eq!(updated.metadata["ordinary_update"], json!(true));
    assert_eq!(updated.metadata["lifecycle"], json!("pending_review"));
    assert_eq!(updated.metadata["status"], json!("pending_review"));
    assert_eq!(updated.metadata["review_status"], json!("pending"));
    assert_eq!(updated.metadata["authority"], json!("advisory"));
    assert!(updated.metadata.get("review_receipt").is_none());
    assert!(updated.metadata.get("source_bundle_hash").is_none());
    assert!(updated.metadata.get("source_ref").is_none());
}

#[tokio::test]
async fn tachi_memory_generic_save_preserves_non_wiki_metadata() {
    let server = make_server();
    let mut params = tachi_memory_params("save");
    params.kind = Some("memory".to_string());
    params.id = Some("generic-non-wiki-authority".to_string());
    params.text = Some("Non-wiki saves retain their legitimate metadata.".to_string());
    params.path = Some("/project/ordinary-metadata".to_string());
    params.force = true;
    params.metadata = Some(json!({
        "ordinary_metadata": "preserved",
        "lifecycle": "active",
        "status": "active",
        "review_status": "approved",
        "authority": "project_work_record",
        "source_bundle_hash": "legitimate-non-wiki-bundle",
        "source_ref": "legitimate-non-wiki-source",
        "review_receipt": {
            "reviewer": "non-wiki-reviewer",
            "decision": "approved"
        }
    }));

    crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("generic non-wiki save");

    let entry = server
        .with_global_store_read(|store| {
            store
                .get("generic-non-wiki-authority")
                .map_err(|error| error.to_string())
        })
        .expect("read generic non-wiki save")
        .expect("generic non-wiki entry exists");
    assert_eq!(entry.metadata["ordinary_metadata"], json!("preserved"));
    assert_eq!(entry.metadata["lifecycle"], json!("active"));
    assert_eq!(entry.metadata["status"], json!("active"));
    assert_eq!(entry.metadata["review_status"], json!("approved"));
    assert_eq!(entry.metadata["authority"], json!("project_work_record"));
    assert_eq!(
        entry.metadata["source_bundle_hash"],
        json!("legitimate-non-wiki-bundle")
    );
    assert_eq!(
        entry.metadata["review_receipt"]["decision"],
        json!("approved")
    );
}

use super::*;

fn base_write_params(path: Option<String>) -> WikiWriteParams {
    WikiWriteParams {
        title: "Lifecycle write test".to_string(),
        text: "Lifecycle write test body.".to_string(),
        path,
        topic: Some("lifecycle-write-test".to_string()),
        summary: None,
        category: "experience".to_string(),
        keywords: vec![],
        entities: vec![],
        importance: 0.7,
        scope: "global".to_string(),
        retention_policy: "permanent".to_string(),
        domain: None,
        project: None,
        metadata: None,
        force: true,
        references: vec![],
        include_patterns: false,
        pattern_query: None,
        pattern_top_k: None,
    }
}

/// #1072 fix-round (cross-vendor review, #1215 BUG 1): an ordinary
/// `tachi_wiki_write` call with no review receipt and no source references
/// must NOT land as `active` merely for being outside `/wiki/drafts/` — the
/// canon-doc invariant is `Active ⇒ validated sources + approval`. This test
/// used to assert the opposite (`lifecycle == "active"` for a plain,
/// unreviewed, sourceless write) and froze the unsafe "outside drafts is
/// always active" state the review flagged; it now asserts the corrected,
/// fail-closed behavior. See
/// `tachi_wiki_write_stamps_active_lifecycle_when_reviewed_and_sourced`
/// below for the (currently unreachable by any live MCP action) path that
/// does earn `active`.
#[tokio::test]
async fn tachi_wiki_write_stamps_pending_review_lifecycle_for_ordinary_unreviewed_path() {
    let server = make_server();
    let response = server
        .tachi_wiki_write(Parameters(base_write_params(None)))
        .await
        .expect("wiki write should succeed");
    let json: Value = serde_json::from_str(&response).expect("json");
    let id = json["id"].as_str().expect("id").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry json");
    assert_eq!(
        entry["metadata"]["lifecycle"],
        json!("pending_review"),
        "an unreviewed, sourceless write must not earn active lifecycle"
    );
    assert_eq!(entry["metadata"]["artifact_kind"], json!("wiki"));
}

/// Companion GREEN: a write that DOES carry both halves of the invariant —
/// an approved `review_receipt` and at least one validated source
/// reference — earns `active`, with a real `source_bundle_hash` stamped
/// (not omitted). This exercises `wiki_layer_metadata`'s escape hatch
/// directly through the params shape (no MCP action authors an approved
/// receipt yet, but the write path must honor one if a caller supplies it).
#[tokio::test]
async fn tachi_wiki_write_stamps_active_lifecycle_when_reviewed_and_sourced() {
    let server = make_server();
    let mut params = base_write_params(None);
    params.references = vec!["kckylechen1/tachi#1072".to_string()];
    params.metadata = Some(json!({
        "review_receipt": {
            "approver": "owner",
            "decision": "approved",
            "decided_at": "2026-07-17T00:00:00Z",
        }
    }));
    let response = server
        .tachi_wiki_write(Parameters(params))
        .await
        .expect("wiki write should succeed");
    let json: Value = serde_json::from_str(&response).expect("json");
    let id = json["id"].as_str().expect("id").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry json");
    assert_eq!(entry["metadata"]["lifecycle"], json!("active"));
    assert!(
        entry["metadata"]["source_bundle_hash"].is_string(),
        "active entry must carry a real source_bundle_hash: {entry:?}"
    );
    assert_eq!(
        entry["metadata"]["review_receipt"]["decision"],
        json!("approved")
    );
}

#[tokio::test]
async fn tachi_wiki_write_stamps_pending_review_lifecycle_for_drafts_path() {
    let server = make_server();
    let response = server
        .tachi_wiki_write(Parameters(base_write_params(Some(
            "/wiki/drafts/lifecycle-write-test".to_string(),
        ))))
        .await
        .expect("wiki write should succeed");
    let json: Value = serde_json::from_str(&response).expect("json");
    let id = json["id"].as_str().expect("id").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki memory");
    let entry: Value = serde_json::from_str(&fetched).expect("entry json");
    assert_eq!(entry["metadata"]["lifecycle"], json!("pending_review"));
    assert_eq!(entry["metadata"]["artifact_kind"], json!("draft"));
}

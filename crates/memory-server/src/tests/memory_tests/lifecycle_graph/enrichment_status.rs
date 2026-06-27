use super::*;

#[tokio::test]
async fn get_memory_reports_pending_when_neither_summary_nor_vector_present() {
    let server = make_server();

    // Save a freshly-built entry: summary empty, vector None — the on-disk
    // shape immediately after a synchronous write but before the enrichment
    // batcher has flushed.
    let id = format!("pending-{}", uuid::Uuid::new_v4());
    let entry = make_entry(&id);
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("save: {e}")))
        .expect("save entry");

    let body = crate::memory_ops::handle_get_memory(
        &server,
        crate::tool_params::GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("get_memory");

    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["embedding_pending"], json!(true), "body: {body}");
    assert_eq!(v["summary_pending"], json!(true), "body: {body}");
    // No foundry job has been queued for this id — the field must be absent.
    assert!(
        v.get("foundry_jobs").is_none(),
        "expected no foundry_jobs field, got: {body}"
    );
}

#[tokio::test]
async fn get_memory_reports_complete_when_summary_and_vector_present() {
    let server = make_server();

    let id = format!("complete-{}", uuid::Uuid::new_v4());
    let mut entry = make_entry(&id);
    entry.summary = "a brief precomputed summary".to_string();
    entry.vector = Some(vec![0.0_f32; 1024]); // schema requires 1024-dim vectors
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("save: {e}")))
        .expect("save entry");

    let body = crate::memory_ops::handle_get_memory(
        &server,
        crate::tool_params::GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("get_memory");

    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["embedding_pending"], json!(false), "body: {body}");
    assert_eq!(v["summary_pending"], json!(false), "body: {body}");
}

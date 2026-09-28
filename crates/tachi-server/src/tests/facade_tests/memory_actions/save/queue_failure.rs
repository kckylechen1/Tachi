use super::*;

#[tokio::test]
async fn public_save_records_summary_queue_failure_only_when_summary_was_requested() {
    let server = make_server();
    server.close_enrichment_channel_for_test();

    let mut requires_summary = tachi_memory_params("save");
    requires_summary.format = Some("json".to_string());
    requires_summary.id = Some("save-summary-queue-closed".to_string());
    requires_summary.path = Some("/scratch/save-summary-queue-closed".to_string());
    requires_summary.text = Some("capture this summary failure durably".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, requires_summary)
        .await
        .expect("save returns a receipt despite asynchronous queue failure");

    let failed = server
        .with_global_store_read(|store| {
            store
                .get("save-summary-queue-closed")
                .map_err(|error| error.to_string())
        })
        .expect("read failed queue row")
        .expect("failed queue row exists");
    assert_eq!(
        failed.metadata["enrichment"]["failed_stage"],
        json!("summary")
    );
    assert_eq!(
        failed.metadata["enrichment"]["last_error"],
        json!("summary_enrichment_queue_unavailable: summary enrichment could not be queued; retry when the enrichment worker is available")
    );

    let mut supplied_summary = tachi_memory_params("save");
    supplied_summary.format = Some("json".to_string());
    supplied_summary.id = Some("save-no-summary-queue-closed".to_string());
    supplied_summary.path = Some("/scratch/save-no-summary-queue-closed".to_string());
    supplied_summary.text = Some("this caller supplied a summary".to_string());
    supplied_summary.summary = Some("already summarized".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, supplied_summary)
        .await
        .expect("save with supplied summary returns a receipt");
    let not_failed = server
        .with_global_store_read(|store| {
            store
                .get("save-no-summary-queue-closed")
                .map_err(|error| error.to_string())
        })
        .expect("read supplied-summary row")
        .expect("supplied-summary row exists");
    assert!(
        not_failed.metadata.get("enrichment").is_none(),
        "non-summary queue failure must not stamp a summary failure: {}",
        not_failed.metadata
    );
}

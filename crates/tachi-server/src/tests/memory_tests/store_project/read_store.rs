use super::*;

#[tokio::test]
async fn cached_global_read_store_sees_writes_and_stays_read_only() {
    let server = make_server();

    server
        .with_global_store(|store| {
            store
                .upsert(&make_entry("cached-read-visible"))
                .map_err(|e| format!("upsert failed: {e}"))
        })
        .expect("seed after cached read store startup");

    let found = server
        .with_global_store_read(|store| {
            store
                .get("cached-read-visible")
                .map_err(|e| format!("cached read get failed: {e}"))
        })
        .expect("cached read should see committed write");
    assert_eq!(found.expect("entry exists").id, "cached-read-visible");

    let write_err = server
        .with_global_store_read(|store| {
            store
                .upsert(&make_entry("cached-read-write-blocked"))
                .map_err(|e| format!("cached read write failed: {e}"))
        })
        .expect_err("cached read store must reject writes");
    assert!(
        write_err.contains("cached read write failed"),
        "{write_err}"
    );
}

use super::*;

#[tokio::test]
async fn sync_memories_errors_if_agent_state_persist_fails() {
    let server = make_server();

    server
        .with_global_store(|store| {
            store
                .upsert(&make_entry("sync-1"))
                .map_err(|e| format!("upsert failed: {e}"))
        })
        .expect("failed to seed memory");

    let global_db: String = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT file FROM pragma_database_list WHERE name = 'main'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("resolve global DB path");
    let offline =
        rusqlite::Connection::open(global_db).expect("open sync failure fixture connection");
    offline
        .execute_batch(
            r#"
            INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at)
            VALUES ('agent-sync-test', 'sync-state-blocker', 0, '');
            CREATE UNIQUE INDEX block_agent_known_state_insert
                ON agent_known_state (agent_id);
            "#,
        )
        .expect("failed to install blocking constraint");
    drop(offline);

    let params = SyncMemoriesParams {
        agent_id: "agent-sync-test".to_string(),
        path_prefix: Some("/".to_string()),
        limit: 10,
    };

    let err = server
        .sync_memories(Parameters(params))
        .await
        .expect_err("sync_memories should fail when state persistence fails");

    assert!(
        err.contains("failed to persist agent state"),
        "unexpected error: {err}"
    );
}

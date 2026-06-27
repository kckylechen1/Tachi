use super::*;

#[tokio::test]
async fn tachi_status_separates_active_worker_queue_from_terminal_history() {
    let (server, temp_home) = make_server_with_temp_home();
    let manifest_path = temp_home.temp_home.join(".tachi/manifest.json");
    let marker_path = temp_home
        .temp_home
        .join(".tachi/foundry-runs/.last_distill_run");
    std::fs::create_dir_all(marker_path.parent().unwrap()).expect("marker parent");
    std::fs::write(&marker_path, Utc::now().to_rfc3339()).expect("write marker");

    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO foundry_jobs
                     (id, kind, lane, status, target_db, path_prefix, memory_ids, metadata, created_at, updated_at)
                     VALUES ('job-completed', 'memory_neighborhood', 'maintenance', 'completed', 'project', '/', '[]', '{}', ?1, ?1)",
                    rusqlite::params![Utc::now().to_rfc3339()],
                )
                .map_err(|e| e.to_string())?;
            store
                .connection()
                .execute(
                    "INSERT INTO foundry_jobs
                     (id, kind, lane, status, target_db, path_prefix, memory_ids, metadata, created_at, updated_at)
                     VALUES ('job-skipped', 'forget_sweep', 'maintenance', 'skipped', 'project', '/', '[]', '{}', ?1, ?1)",
                    rusqlite::params![Utc::now().to_rfc3339()],
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed terminal-only status db");

    let manifest = crate::manifest::Manifest {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![crate::manifest::DbEntry {
            path: server.global_db_path_buf().display().to_string(),
            role: crate::manifest::DbRole::Global,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "global".to_string(),
            notes: String::new(),
        }],
    };
    manifest.save(&manifest_path).expect("save manifest");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    assert_eq!(parsed["databases"]["active_jobs"], json!(0));
    assert_eq!(parsed["databases"]["pending_jobs"], json!(0));
    assert_eq!(parsed["databases"]["failed_jobs"], json!(0));
    assert_eq!(parsed["databases"]["stuck_jobs"], json!(0));
    assert_eq!(parsed["databases"]["terminal_jobs"], json!(2));

    let queue = &parsed["databases"]["worker_queues"][0];
    assert_eq!(queue["queue_state"], json!("idle"));
    assert_eq!(queue["active_jobs"], json!(0));
    assert_eq!(queue["terminal_jobs"], json!(2));
    assert_eq!(queue["backfill"]["needed"], json!(false));
    assert!(
        queue["latest_active_job"].is_null(),
        "terminal history must not masquerade as active queue: {queue}"
    );
    assert_eq!(
        queue["latest_terminal_job"]["status"],
        json!("skipped"),
        "terminal history should still be available for audit"
    );
}

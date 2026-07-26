use super::*;

#[tokio::test]
async fn tachi_status_reports_failed_jobs_and_vector_backfill_hint() {
    let (server, temp_home) = make_server_with_temp_home();
    let manifest_path = temp_home.temp_home.join(".tachi/manifest.json");
    let marker_path = temp_home
        .temp_home
        .join(".tachi/foundry-runs/.last_distill_run");
    std::fs::create_dir_all(marker_path.parent().unwrap()).expect("marker parent");
    std::fs::write(&marker_path, Utc::now().to_rfc3339()).expect("write marker");

    server
        .with_global_store(|store| {
            let mut status_memory = make_entry("status-memory");
            status_memory.path = "/facts/status".to_string();
            status_memory.summary = "status".to_string();
            status_memory.text = "status diagnostic memory".to_string();
            status_memory.importance = 0.8;
            status_memory.category = "fact".to_string();
            status_memory.topic = "status".to_string();
            status_memory.source = "manual".to_string();
            status_memory.scope = "project".to_string();
            store.upsert(&status_memory).map_err(|e| e.to_string())?;
            store
                .connection()
                .execute(
                    "INSERT INTO foundry_jobs
                     (id, kind, lane, status, target_db, path_prefix, memory_ids, metadata, created_at, updated_at)
                     VALUES ('job-1', 'memory_distill', 'distill', 'failed', 'project', '/', '[]', ?1, ?2, ?2)",
                    rusqlite::params![
                        json!({"terminal_reason":{"reason":"403 Forbidden"}}).to_string(),
                        Utc::now().to_rfc3339()
                    ],
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed status db");

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
    server.llm.set_provider_secret_pool(
        "VOYAGE_API_KEY",
        vec![
            tachi_llm::ProviderSecret {
                key_id: "VOYAGE_API_KEY_1".to_string(),
                value: "voyage-secret-one".to_string(),
            },
            tachi_llm::ProviderSecret {
                key_id: "VOYAGE_API_KEY_2".to_string(),
                value: "voyage-secret-two".to_string(),
            },
        ],
    );
    server.llm.mark_provider_key_rate_limited_for_tests(
        "VOYAGE_API_KEY",
        "VOYAGE_API_KEY_1",
        Some(60),
    );

    let body = crate::status_ops::handle_tachi_status_full(&server, Some("json"))
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    assert!(parsed["runtime"]["pid"].as_u64().is_some());
    assert!(parsed["runtime"]["provider_secret_count"]
        .as_u64()
        .is_some());
    assert!(parsed["runtime"]["provider_health"]["source_of_truth"]
        .as_str()
        .is_some());
    assert!(parsed["runtime"]["provider_health"]["reload_ttl_secs"]
        .as_u64()
        .is_some());
    assert_eq!(
        parsed["runtime"]["provider_pools"][0]["logical_name"],
        json!("VOYAGE_API_KEY")
    );
    assert_eq!(
        parsed["runtime"]["provider_pools"][0]["available_keys"],
        json!(1)
    );
    assert_eq!(
        parsed["runtime"]["provider_pools"][0]["rate_limited_keys"][0]["key_id"],
        json!("VOYAGE_API_KEY_1")
    );
    assert!(
        !body.contains("voyage-secret-one") && !body.contains("voyage-secret-two"),
        "status must not expose provider secret values: {body}"
    );
    assert_eq!(parsed["runtime"]["vault"]["unlocked"], json!(false));
    assert_eq!(parsed["databases"]["failed_jobs"], json!(1));
    assert_eq!(parsed["distill"]["is_stale"], json!(false));
    assert!(
        parsed["databases"]["low_vector_coverage"][0]["backfill_command"]
            .as_str()
            .unwrap_or_default()
            .contains("tachi backfill-vectors")
    );
    assert!(parsed["databases"]["provider_auth_failures"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty()));
    assert_eq!(
        parsed["databases"]["provider_auth_failures"][0]["inferred_invalid_provider"],
        json!("SILICONFLOW")
    );
    assert_eq!(parsed["models"]["embedding"]["model"], json!("voyage-4"));
}

#[tokio::test]
async fn tachi_status_reports_durable_vector_sweep_state() {
    let (server, temp_home) = make_server_with_temp_home();
    let manifest_path = temp_home.temp_home.join(".tachi/manifest.json");

    server
        .with_global_store(|store| {
            let mut sweep_memory = make_entry("sweep-memory");
            sweep_memory.path = "/facts/sweep".to_string();
            sweep_memory.summary = "sweep".to_string();
            sweep_memory.text = "durable vector sweep test".to_string();
            sweep_memory.importance = 0.8;
            sweep_memory.category = "fact".to_string();
            sweep_memory.topic = "status".to_string();
            sweep_memory.source = "manual".to_string();
            sweep_memory.scope = "project".to_string();
            store.upsert(&sweep_memory).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed status db");

    crate::vector_backfill::record_vector_sweep_state(
        &server.global_db_path_buf(),
        crate::vector_backfill::VectorSweepStateUpdate {
            enabled: true,
            disabled_reason: None,
            skip_recall_cache: true,
            embedded_count: 3,
            failed_count: 1,
            last_error: Some("Voyage embed batch failed: 429".to_string()),
            last_provider_error: Some("Voyage embed batch failed: 429".to_string()),
            interval_secs: Some(1800),
            preserve_schedule: false,
            preserve_outcome: false,
        },
    )
    .expect("record sweep state");

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

    let body = crate::status_ops::handle_tachi_status_full(&server, Some("json"))
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let sweep = &parsed["databases"]["worker_queues"][0]["vector_sweep"];
    assert_eq!(sweep["enabled"], json!(true));
    assert_eq!(sweep["skip_recall_cache"], json!(true));
    assert_eq!(sweep["embedded_count"], json!(3));
    assert_eq!(sweep["failed_count"], json!(1));
    assert_eq!(sweep["interval_secs"], json!(1800));
    assert_eq!(sweep["current_total_count"], json!(1));
    assert_eq!(sweep["current_with_vector_count"], json!(0));
    assert_eq!(sweep["current_pending_count"], json!(1));
    assert!(sweep["current_pending_threshold"].as_u64().is_some());
    assert!(sweep["current_backfill_needed"].as_bool().is_some());
    assert!(sweep["last_run_at"].as_str().is_some());
    assert_eq!(
        sweep["last_provider_error"],
        json!("Voyage embed batch failed: 429")
    );
}

#[tokio::test]
async fn tachi_status_surfaces_malformed_vector_sweep_state() {
    let (server, temp_home) = make_server_with_temp_home();
    let manifest_path = temp_home.temp_home.join(".tachi/manifest.json");

    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| {
            connection.execute(
                "CREATE TABLE vector_sweep_state (
                        key TEXT PRIMARY KEY,
                        enabled TEXT NOT NULL
                    )",
                [],
            )?;
            connection.execute(
                "INSERT INTO vector_sweep_state (key, enabled) VALUES ('default', 'yes')",
                [],
            )?;
            Ok(())
        },
    )
    .expect("seed malformed sweep state");

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

    let body = crate::status_ops::handle_tachi_status_full(&server, Some("json"))
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let db = &parsed["databases"]["worker_queues"][0];
    assert_eq!(db["vector_sweep"], Value::Null);
    assert!(
        db["vector_sweep_error"]
            .as_str()
            .is_some_and(|err| err.contains("read vector sweep state")),
        "malformed sweep state must be visible: {}",
        db["vector_sweep_error"]
    );
}

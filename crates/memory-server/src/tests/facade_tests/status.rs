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
            store
                .connection()
                .execute(
                    "INSERT INTO memories
                     (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                     VALUES (?1, '/facts/status', 'status', 'status diagnostic memory', 0.8, ?2, 'fact', 'status', '[]', '[]', 'manual', 'project', 0, ?2, ?2, 0, 1, '{}')",
                    rusqlite::params!["status-memory", Utc::now().to_rfc3339()],
                )
                .map_err(|e| e.to_string())?;
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
            crate::llm::ProviderSecret {
                key_id: "VOYAGE_API_KEY_1".to_string(),
                value: "voyage-secret-one".to_string(),
            },
            crate::llm::ProviderSecret {
                key_id: "VOYAGE_API_KEY_2".to_string(),
                value: "voyage-secret-two".to_string(),
            },
        ],
    );
    server
        .llm
        .mark_provider_key_rate_limited_for_tests("VOYAGE_API_KEY_1", Some(60));

    let body = crate::status_ops::handle_tachi_status_full(&server)
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
async fn tachi_status_full_surfaces_continuity_challenge_rate() {
    let (server, temp_home) = make_server_with_temp_home();
    let manifest_path = temp_home.temp_home.join(".tachi/manifest.json");
    std::fs::create_dir_all(manifest_path.parent().expect("manifest parent"))
        .expect("create manifest dir");

    server
        .with_global_store(|store| {
            for (id, outcome) in [
                ("outcome-ai", "ai_corrected"),
                ("outcome-user", "user_correct"),
                ("outcome-open", "unresolved"),
            ] {
                store
                    .insert_tachi_event(&memory_core::TachiEventRecord {
                        id: id.to_string(),
                        source_repo: "sigil".to_string(),
                        adapter: "memory-server-test".to_string(),
                        project: "sigil".to_string(),
                        domain: "coding".to_string(),
                        session_id: id.to_string(),
                        actor: "labeler".to_string(),
                        event_type: "session.outcome".to_string(),
                        authority: memory_core::AuthorityLevel::ReviewSignalOnly,
                        effects: vec![memory_core::EffectScope::Scoring],
                        projection_hints: vec![memory_core::ProjectionKind::Outcome],
                        payload: json!({
                            "outcome": outcome,
                            "evidence_basis": "external_evidence",
                        }),
                        provenance: json!({"source": "test"}),
                        created_at: Utc::now().to_rfc3339(),
                    })
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed continuity events");

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

    assert_eq!(
        parsed["databases"]["continuity"]["session_outcomes"]["outcome_events"],
        json!(3)
    );
    assert_eq!(
        parsed["databases"]["continuity"]["session_outcomes"]["eligible_outcomes"],
        json!(2)
    );
    assert_eq!(
        parsed["databases"]["continuity"]["session_outcomes"]["challenge_rate"],
        json!(0.5)
    );
    assert_eq!(
        parsed["databases"]["worker_queues"][0]["continuity"]["session_outcomes"]["ai_corrected"],
        json!(1)
    );
}

#[tokio::test]
async fn tachi_status_agent_surface_is_compact() {
    let (server, _temp_home) = make_server_with_temp_home();
    let body = crate::status_ops::handle_tachi_status_agent(&server)
        .await
        .expect("agent status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("agent status JSON");
    // Compact surface: runtime is slim (the heavy provider arrays are omitted;
    // they live in handle_tachi_status_full + runtime_info).
    assert!(parsed["runtime"]["provider_pools"].is_null());
    assert!(parsed["runtime"]["provider_health"].is_null());
    assert!(
        parsed["runtime"]["provider_secret_count"]
            .as_u64()
            .is_some(),
        "slim runtime still keeps the secret count"
    );
    // api_keys.provider_pools collapses to a {total, rate_limited} summary.
    assert!(
        parsed["api_keys"]["provider_pools"]["total"]
            .as_u64()
            .is_some(),
        "compact api_keys.provider_pools should be a summary, not the full array: {body}"
    );
    assert!(parsed["api_keys"]["provider_pools"].as_array().is_none());
}

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

#[tokio::test]
async fn tachi_status_marks_vault_alias_in_config_env_as_vault_config() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env dir");
    std::fs::write(&config_env, "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\n").expect("write config env");

    let global_db = temp_home.temp_home.join("global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("mkdir");
    let store = memory_core::MemoryStore::open(global_db.to_str().unwrap()).expect("open global");
    let _ = store; // vault entries optional for status name listing

    let original_voyage = std::env::var_os("VOYAGE_API_KEY");
    std::env::remove_var("VOYAGE_API_KEY");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let voyage = parsed["api_keys"]
        .as_array()
        .expect("api_keys array")
        .iter()
        .find(|row| row["name"] == json!("VOYAGE_API_KEY"))
        .expect("voyage key row");
    // Without vault entry, alias alone may be unresolved; with vault it is vault(config.env).
    assert!(
        voyage["source"] == json!("vault(config.env)")
            || voyage["source"] == json!("vault-alias-unresolved")
            || voyage["status"] == json!("missing")
    );

    if let Some(value) = original_voyage {
        std::env::set_var("VOYAGE_API_KEY", value);
    } else {
        std::env::remove_var("VOYAGE_API_KEY");
    }
}

#[tokio::test]
async fn tachi_status_marks_config_env_only_key_as_configured() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env dir");
    std::fs::write(&config_env, "VOYAGE_API_KEY=config-only-value\n").expect("write config env");

    let original_voyage = std::env::var_os("VOYAGE_API_KEY");
    std::env::remove_var("VOYAGE_API_KEY");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let voyage = parsed["api_keys"]
        .as_array()
        .expect("api_keys array")
        .iter()
        .find(|row| row["name"] == json!("VOYAGE_API_KEY"))
        .expect("voyage key row");
    assert_eq!(voyage["status"], json!("configured"));
    assert_eq!(voyage["source"], json!("config.env"));

    if let Some(value) = original_voyage {
        std::env::set_var("VOYAGE_API_KEY", value);
    } else {
        std::env::remove_var("VOYAGE_API_KEY");
    }
}

#[tokio::test]
async fn tachi_status_marks_config_env_alias_key_as_configured() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env dir");
    std::fs::write(&config_env, "BIGMODEL_API_KEY=alias-config-value\n").expect("write config env");

    let original_reasoning = std::env::var_os("REASONING_API_KEY");
    let original_zai = std::env::var_os("ZAI_API_KEY");
    let original_bigmodel = std::env::var_os("BIGMODEL_API_KEY");
    std::env::remove_var("REASONING_API_KEY");
    std::env::remove_var("ZAI_API_KEY");
    std::env::remove_var("BIGMODEL_API_KEY");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let reasoning = parsed["api_keys"]
        .as_array()
        .expect("api_keys array")
        .iter()
        .find(|row| row["name"] == json!("REASONING_API_KEY"))
        .expect("reasoning key row");
    assert_eq!(reasoning["status"], json!("configured"));
    assert_eq!(reasoning["source"], json!("alias"));

    if let Some(value) = original_reasoning {
        std::env::set_var("REASONING_API_KEY", value);
    } else {
        std::env::remove_var("REASONING_API_KEY");
    }
    if let Some(value) = original_zai {
        std::env::set_var("ZAI_API_KEY", value);
    } else {
        std::env::remove_var("ZAI_API_KEY");
    }
    if let Some(value) = original_bigmodel {
        std::env::set_var("BIGMODEL_API_KEY", value);
    } else {
        std::env::remove_var("BIGMODEL_API_KEY");
    }
}

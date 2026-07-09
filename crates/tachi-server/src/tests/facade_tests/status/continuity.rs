use super::*;

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
                    .insert_tachi_event(&memcore::TachiEventRecord {
                        id: id.to_string(),
                        source_repo: "sigil".to_string(),
                        adapter: "memory-server-test".to_string(),
                        project: "sigil".to_string(),
                        domain: "coding".to_string(),
                        session_id: id.to_string(),
                        actor: "labeler".to_string(),
                        event_type: "session.outcome".to_string(),
                        authority: memcore::AuthorityLevel::ReviewSignalOnly,
                        effects: vec![memcore::EffectScope::Scoring],
                        projection_hints: vec![memcore::ProjectionKind::Outcome],
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

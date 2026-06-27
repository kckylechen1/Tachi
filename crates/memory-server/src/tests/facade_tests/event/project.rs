use super::*;

#[tokio::test]
async fn tachi_event_project_materializes_pattern_idempotently() {
    let server = make_server();

    let mut emit = tachi_event_params("emit");
    emit.id = Some("pattern-observed-1".to_string());
    emit.source_repo = Some("sigil".to_string());
    emit.adapter = Some("facade-test".to_string());
    emit.domain = Some("agent_os".to_string());
    emit.session_id = Some("session-pattern".to_string());
    emit.actor = Some("codex".to_string());
    emit.event_type = Some("pattern.observed".to_string());
    emit.authority = Some("collect_only".to_string());
    emit.projection_hints = vec!["pattern".to_string()];
    emit.payload = Some(json!({
        "pattern_key": "continuity-first",
        "summary": "Continuity-first project management",
        "text": "User treats memory, subagent eval, lorebook, and emotion as one continuity substrate.",
    }));
    crate::event_ops::handle_tachi_event(&server, emit)
        .await
        .expect("emit pattern event");

    let mut project = tachi_event_params("project");
    project.projection_hints = vec!["pattern".to_string()];
    let first = crate::event_ops::handle_tachi_event(&server, project.clone())
        .await
        .expect("project pattern");
    let first_json: Value = serde_json::from_str(&first).expect("project JSON");
    assert_eq!(first_json["status"], json!("completed"));
    assert_eq!(first_json["projected_count"], json!(1));
    assert_eq!(
        first_json["projections"][0]["already_projected"],
        json!(false)
    );
    let memory_id = first_json["projections"][0]["memory_id"]
        .as_str()
        .expect("memory id")
        .to_string();

    let second = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project pattern again");
    let second_json: Value = serde_json::from_str(&second).expect("second project JSON");
    assert_eq!(
        second_json["projections"][0]["already_projected"],
        json!(true)
    );

    let entry = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read projection")
        .expect("projection entry exists");
    assert!(entry.path.starts_with("/user/patterns/agent_os/"));
    assert_eq!(entry.metadata["projection_kind"], json!("pattern"));
    assert_eq!(entry.metadata["counters"]["seen"], json!(1));
    assert_eq!(
        entry.metadata["projected_event_ids"],
        json!(["pattern-observed-1"])
    );
}

#[tokio::test]
async fn tachi_event_project_updates_pattern_hit_miss_counters() {
    let server = make_server();

    for (id, event_type, created_at) in [
        (
            "pattern-counter-candidate",
            "pattern.candidate",
            "2026-06-24T00:00:00Z",
        ),
        ("pattern-counter-hit", "pattern.hit", "2026-06-24T00:01:00Z"),
        (
            "pattern-counter-miss",
            "pattern.miss",
            "2026-06-24T00:02:00Z",
        ),
    ] {
        let mut emit = tachi_event_params("emit");
        emit.id = Some(id.to_string());
        emit.source_repo = Some("sigil".to_string());
        emit.adapter = Some("facade-test".to_string());
        emit.domain = Some("agent_os".to_string());
        emit.session_id = Some("session-pattern-counters".to_string());
        emit.actor = Some("codex".to_string());
        emit.event_type = Some(event_type.to_string());
        emit.authority = Some("collect_only".to_string());
        emit.projection_hints = vec!["pattern".to_string()];
        emit.created_at = Some(created_at.to_string());
        emit.payload = Some(json!({
            "pattern_key": "counter-continuity",
            "summary": "Counter continuity pattern",
            "text": "Candidate projection starts as evidence; hit and miss callbacks update confidence.",
        }));
        crate::event_ops::handle_tachi_event(&server, emit)
            .await
            .expect("emit counter event");
    }

    let mut project = tachi_event_params("project");
    project.projection_hints = vec!["pattern".to_string()];
    project.limit = 10;
    let projected = crate::event_ops::handle_tachi_event(&server, project.clone())
        .await
        .expect("project counter events");
    let projected_json: Value = serde_json::from_str(&projected).expect("project JSON");
    assert_eq!(projected_json["projected_count"], json!(3));
    assert_eq!(projected_json["promotion_candidate_count"], json!(1));
    assert_eq!(
        projected_json["promotion_candidates"][0]["reason"],
        json!("hit_threshold")
    );
    let memory_id = projected_json["promotion_candidates"][0]["memory_id"]
        .as_str()
        .expect("promotion candidate memory id")
        .to_string();

    let entry = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read projected counter entry")
        .expect("projection entry exists");
    assert_eq!(entry.metadata["counters"]["seen"], json!(3));
    assert_eq!(entry.metadata["counters"]["hit"], json!(1));
    assert_eq!(entry.metadata["counters"]["miss"], json!(1));
    assert_eq!(entry.metadata["counters"]["confidence"], json!(1.0 / 3.0));
    assert_eq!(entry.tier, "consolidated");

    let second = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project counter events again");
    let second_json: Value = serde_json::from_str(&second).expect("second project JSON");
    assert_eq!(second_json["promotion_candidate_count"], json!(1));
    let entry_after = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read projected counter entry again")
        .expect("projection entry exists after second pass");
    assert_eq!(entry_after.metadata["counters"]["seen"], json!(3));
    assert_eq!(entry_after.metadata["counters"]["hit"], json!(1));
    assert_eq!(entry_after.metadata["counters"]["miss"], json!(1));
}

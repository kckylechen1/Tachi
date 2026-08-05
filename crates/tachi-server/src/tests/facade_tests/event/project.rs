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
    assert_eq!(projected_json["promotion_candidate_count"], json!(0));
    let memory_id = projected_json["projections"][0]["memory_id"]
        .as_str()
        .expect("projection memory id")
        .to_string();

    let entry = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read projected counter entry")
        .expect("projection entry exists");
    assert_eq!(entry.metadata["counters"]["seen"], json!(3));
    assert_eq!(entry.metadata["counters"]["hit"], json!(1));
    assert_eq!(entry.metadata["counters"]["miss"], json!(1));
    assert_eq!(entry.metadata["counters"]["confidence"], json!(0.5));
    assert_eq!(entry.tier, "raw");

    let second = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project counter events again");
    let second_json: Value = serde_json::from_str(&second).expect("second project JSON");
    assert_eq!(second_json["promotion_candidate_count"], json!(0));
    let entry_after = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read projected counter entry again")
        .expect("projection entry exists after second pass");
    assert_eq!(entry_after.metadata["counters"]["seen"], json!(3));
    assert_eq!(entry_after.metadata["counters"]["hit"], json!(1));
    assert_eq!(entry_after.metadata["counters"]["miss"], json!(1));
}

#[tokio::test]
async fn tachi_event_project_promotes_only_when_hits_beat_misses() {
    let server = make_server();

    for (id, event_type, created_at) in [
        (
            "pattern-brake-candidate",
            "pattern.candidate",
            "2026-06-24T00:00:00Z",
        ),
        ("pattern-brake-seen", "pattern.seen", "2026-06-24T00:01:00Z"),
        ("pattern-brake-hit", "pattern.hit", "2026-06-24T00:02:00Z"),
        ("pattern-brake-miss", "pattern.miss", "2026-06-24T00:03:00Z"),
    ] {
        let mut emit = tachi_event_params("emit");
        emit.id = Some(id.to_string());
        emit.source_repo = Some("sigil".to_string());
        emit.adapter = Some("facade-test".to_string());
        emit.domain = Some("agent_os".to_string());
        emit.session_id = Some("session-pattern-brake".to_string());
        emit.actor = Some("codex".to_string());
        emit.event_type = Some(event_type.to_string());
        emit.authority = Some("collect_only".to_string());
        emit.projection_hints = vec!["pattern".to_string()];
        emit.created_at = Some(created_at.to_string());
        emit.payload = Some(json!({
            "pattern_key": "miss-brake",
            "summary": "Miss brake pattern",
            "text": "A pattern with as many misses as hits must not promote.",
            "confidence": 1.0,
        }));
        crate::event_ops::handle_tachi_event(&server, emit)
            .await
            .expect("emit brake event");
    }

    let mut project = tachi_event_params("project");
    project.projection_hints = vec!["pattern".to_string()];
    project.limit = 10;
    let projected = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project brake events");
    let projected_json: Value = serde_json::from_str(&projected).expect("project JSON");

    assert_eq!(projected_json["promotion_candidate_count"], json!(0));
    let memory_id = projected_json["projections"][0]["memory_id"]
        .as_str()
        .expect("projection memory id")
        .to_string();
    let entry = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read brake projection")
        .expect("projection exists");
    assert_eq!(entry.metadata["counters"]["seen"], json!(4));
    assert_eq!(entry.metadata["counters"]["hit"], json!(1));
    assert_eq!(entry.metadata["counters"]["miss"], json!(1));
    assert_eq!(entry.metadata["counters"]["confidence"], json!(0.5));
    assert_eq!(entry.tier, "raw");
}

#[tokio::test]
async fn tachi_event_promote_creates_review_artifacts_for_mature_pattern() {
    let server = make_server();

    for (id, event_type, created_at) in [
        (
            "pattern-promote-candidate",
            "pattern.candidate",
            "2026-06-25T00:00:00Z",
        ),
        (
            "pattern-promote-seen",
            "pattern.seen",
            "2026-06-25T00:01:00Z",
        ),
        ("pattern-promote-hit", "pattern.hit", "2026-06-25T00:02:00Z"),
    ] {
        let mut emit = tachi_event_params("emit");
        emit.id = Some(id.to_string());
        emit.source_repo = Some("sigil".to_string());
        emit.adapter = Some("facade-test".to_string());
        emit.domain = Some("agent_os".to_string());
        emit.session_id = Some("session-pattern-promote".to_string());
        emit.actor = Some("codex".to_string());
        emit.event_type = Some(event_type.to_string());
        emit.authority = Some("collect_only".to_string());
        emit.projection_hints = vec!["pattern".to_string()];
        emit.created_at = Some(created_at.to_string());
        emit.payload = Some(json!({
            "pattern_key": "promote-continuity",
            "summary": "Promote continuity pattern",
            "text": "Mature patterns should generate review artifacts without auto-promoting.",
        }));
        crate::event_ops::handle_tachi_event(&server, emit)
            .await
            .expect("emit promote event");
    }

    let mut project = tachi_event_params("project");
    project.projection_hints = vec!["pattern".to_string()];
    project.limit = 10;
    let projected = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project promotion pattern");
    let projected_json: Value = serde_json::from_str(&projected).expect("project JSON");
    let memory_id = projected_json["promotion_candidates"][0]["memory_id"]
        .as_str()
        .expect("promotion memory id")
        .to_string();

    let mut dry_run = tachi_event_params("promote");
    dry_run.id = Some(memory_id.clone());
    dry_run.dry_run = true;
    let planned = crate::event_ops::handle_tachi_event(&server, dry_run)
        .await
        .expect("dry-run promote");
    let planned_json: Value = serde_json::from_str(&planned).expect("planned JSON");
    assert_eq!(planned_json["status"], json!("planned"));
    assert_eq!(planned_json["planned"]["wiki_draft"], json!(true));
    assert_eq!(planned_json["gate"]["final_ready"], json!(false));

    let mut promote = tachi_event_params("promote");
    promote.id = Some(memory_id.clone());
    let promoted = crate::event_ops::handle_tachi_event(&server, promote)
        .await
        .expect("promote pattern");
    let promoted_json: Value = serde_json::from_str(&promoted).expect("promoted JSON");
    assert_eq!(promoted_json["status"], json!("completed"));
    assert_eq!(promoted_json["auto_promote"], json!(false));
    assert_eq!(
        promoted_json["gate"]["missing"],
        json!(["external_validation", "cold_seat_review"])
    );
    assert_eq!(
        promoted_json["wiki_draft"]["wiki_path"],
        json!("/wiki/drafts/patterns/promote-continuity")
    );
    assert_eq!(
        promoted_json["skill_candidate"]["review_status"],
        json!("pending")
    );
    assert_eq!(
        promoted_json["agent_profile_proposal"]["event_type"],
        json!("agent_profile.proposal")
    );

    let profile_events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    event_type: Some("agent_profile.proposal".to_string()),
                    limit: 10,
                    ..memcore::TachiEventQuery::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("list profile proposal events");
    assert_eq!(profile_events.len(), 1);
    assert_eq!(profile_events[0].payload["write"], json!(false));
    assert_eq!(
        profile_events[0].payload["pattern_ref"]["id"],
        json!(memory_id)
    );
    assert_eq!(
        profile_events[0].payload["gate"]["final_ready"],
        json!(false)
    );
}

#[tokio::test]
async fn tachi_event_project_persists_explicit_timeline_graph_edges() {
    let server = make_server();

    let mut pattern = tachi_event_params("emit");
    pattern.id = Some("timeline-edge-pattern".to_string());
    pattern.source_repo = Some("sigil".to_string());
    pattern.adapter = Some("facade-test".to_string());
    pattern.domain = Some("agent_os".to_string());
    pattern.session_id = Some("session-timeline-edge".to_string());
    pattern.actor = Some("codex".to_string());
    pattern.event_type = Some("pattern.observed".to_string());
    pattern.authority = Some("collect_only".to_string());
    pattern.projection_hints = vec!["pattern".to_string()];
    pattern.payload = Some(json!({
        "pattern_key": "timeline-edge-pattern",
        "summary": "Timeline edge pattern",
        "text": "Timeline graph edges only persist when endpoints are known memory ids.",
    }));
    crate::event_ops::handle_tachi_event(&server, pattern)
        .await
        .expect("emit pattern");

    let mut project_pattern = tachi_event_params("project");
    project_pattern.projection_hints = vec!["pattern".to_string()];
    let pattern_projected = crate::event_ops::handle_tachi_event(&server, project_pattern)
        .await
        .expect("project pattern");
    let pattern_json: Value =
        serde_json::from_str(&pattern_projected).expect("pattern project JSON");
    let pattern_id = pattern_json["projections"][0]["memory_id"]
        .as_str()
        .expect("pattern memory id")
        .to_string();

    let mut timeline = tachi_event_params("emit");
    timeline.id = Some("timeline-edge-event".to_string());
    timeline.source_repo = Some("sigil".to_string());
    timeline.adapter = Some("facade-test".to_string());
    timeline.domain = Some("agent_os".to_string());
    timeline.session_id = Some("session-timeline-edge".to_string());
    timeline.actor = Some("codex".to_string());
    timeline.event_type = Some("timeline.candidate".to_string());
    timeline.authority = Some("collect_only".to_string());
    timeline.projection_hints = vec!["timeline".to_string()];
    timeline.payload = Some(json!({
        "projection_key": "timeline-edge",
        "summary": "Timeline edge projection",
        "discoveries": ["explicit endpoints can become graph edges"],
        "causal_edges": [
            {
                "source_id": pattern_id,
                "target_id": "$projection",
                "relation": "supports",
                "weight": 0.8
            },
            {
                "from": "natural language only",
                "to": "not persisted"
            }
        ],
    }));
    crate::event_ops::handle_tachi_event(&server, timeline)
        .await
        .expect("emit timeline");

    let mut project_timeline = tachi_event_params("project");
    project_timeline.projection_hints = vec!["timeline".to_string()];
    let projected = crate::event_ops::handle_tachi_event(&server, project_timeline)
        .await
        .expect("project timeline");
    let projected_json: Value = serde_json::from_str(&projected).expect("timeline project JSON");
    let timeline_projection = projected_json["projections"]
        .as_array()
        .expect("projections")
        .iter()
        .find(|projection| projection["event_id"] == json!("timeline-edge-event"))
        .expect("timeline projection");
    let timeline_id = timeline_projection["memory_id"]
        .as_str()
        .expect("timeline memory id");
    assert_eq!(timeline_projection["graph_edges"]["saved_count"], json!(1));
    assert_eq!(
        timeline_projection["graph_edges"]["skipped_count"],
        json!(1)
    );

    // tachi#1646: the emitting event's authority is `collect_only` (the
    // lowest tier — set above), so the payload's `relation: "supports"`
    // (activation weight 0.90) is restricted down to `references` (0.70),
    // and the payload's `weight: 0.8` is capped to
    // `CALLER_ASSERTED_WEIGHT_CAP` (0.6) — no `supports` edge is written at
    // all.
    let no_supports_edges = server
        .with_global_store_read(|store| {
            store
                .get_edges(&pattern_id, "outgoing", Some("supports"))
                .map_err(|e| e.to_string())
        })
        .expect("read graph edges (supports)");
    assert!(
        no_supports_edges.is_empty(),
        "a collect_only-tier causal_edges payload must not be able to assert 'supports'"
    );

    let edges = server
        .with_global_store_read(|store| {
            store
                .get_edges(&pattern_id, "outgoing", Some("references"))
                .map_err(|e| e.to_string())
        })
        .expect("read graph edges (references)");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].target_id, timeline_id);
    assert_eq!(
        edges[0].metadata["source_event_id"],
        json!("timeline-edge-event")
    );
    assert!(
        (edges[0].weight - 0.6).abs() < 1e-9,
        "collect_only-tier weight must be capped at 0.6, got {}",
        edges[0].weight
    );
}

#[tokio::test]
async fn tachi_event_promote_rejects_immature_pattern_without_force() {
    let server = make_server();

    let mut emit = tachi_event_params("emit");
    emit.id = Some("pattern-promote-immature".to_string());
    emit.source_repo = Some("sigil".to_string());
    emit.adapter = Some("facade-test".to_string());
    emit.domain = Some("agent_os".to_string());
    emit.session_id = Some("session-pattern-promote-immature".to_string());
    emit.actor = Some("codex".to_string());
    emit.event_type = Some("pattern.candidate".to_string());
    emit.authority = Some("collect_only".to_string());
    emit.projection_hints = vec!["pattern".to_string()];
    emit.payload = Some(json!({
        "pattern_key": "immature-continuity",
        "summary": "Immature continuity pattern",
        "text": "A single candidate is not mature enough to promote.",
    }));
    crate::event_ops::handle_tachi_event(&server, emit)
        .await
        .expect("emit immature pattern");

    let mut project = tachi_event_params("project");
    project.projection_hints = vec!["pattern".to_string()];
    let projected = crate::event_ops::handle_tachi_event(&server, project)
        .await
        .expect("project immature pattern");
    let projected_json: Value = serde_json::from_str(&projected).expect("project JSON");
    let memory_id = projected_json["projections"][0]["memory_id"]
        .as_str()
        .expect("pattern memory id")
        .to_string();

    let mut promote = tachi_event_params("promote");
    promote.id = Some(memory_id);
    let err = crate::event_ops::handle_tachi_event(&server, promote)
        .await
        .expect_err("immature promote should fail");
    assert!(err.contains("not mature enough to promote"), "err: {err}");
}

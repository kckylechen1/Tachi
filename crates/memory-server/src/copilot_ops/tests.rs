use super::*;

#[test]
fn build_debug_checklist_prefers_wiki_guidance() {
    let checklist = build_debug_checklist(&[json!({
        "path": "/wiki/debug/mcp-args",
        "text": "Checklist:\n- Verify schema -> client serialization -> server deserialization before editing transport.\n- Add a failing boundary test at the API boundary before retrying the same layer.\n- Stop after two failed patches in the same layer and ask another agent.",
        "summary": "MCP argument debugging"
    })]);

    assert!(checklist[0].contains("schema -> client serialization -> server deserialization"));
    assert!(checklist
        .iter()
        .any(|item| item.contains("failing boundary test at the API boundary")));
}

#[test]
fn build_debug_checklist_falls_back_without_wiki_hits() {
    let checklist = build_debug_checklist(&[json!({
        "path": "/behavior/global_rules/retry-policy",
        "text": "This is not a wiki entry and should not override the fallback checklist.",
    })]);

    assert_eq!(checklist.len(), DEBUG_CHECKLIST_LIMIT);
    assert_eq!(checklist[0], FALLBACK_DEBUG_CHECKLIST[0]);
}

#[test]
fn wiki_slug_preserves_cjk_and_readable_separators() {
    assert_eq!(
        wiki_slug("MCP hub_call arguments 丢失：从 schema 层排查"),
        "MCP-hub_call-arguments-丢失-从-schema-层排查"
    );
}

#[test]
fn skill_scoring_ignores_generic_fix_tokens() {
    // Migrated (#517 cut 2): the original test asserted integer scores from the
    // deleted `score_capability` against the old duplicate tokenizer. The
    // discriminating property is preserved here through the consolidated
    // `recommend_skills_light` path: for a task sharing domain vocabulary
    // ("arguments", "hub_call", "serialization") with the MCP-schema-debug skill
    // but NO domain token with the unrelated frontend-design skill, the MCP skill
    // must be recommended and frontend-design must be ABSENT from the
    // recommendation set entirely.
    //
    // This is a frozen guarantee (hard bounds), not a soft ranking:
    //   - frontend-design shares no domain token with the query, so it must NOT
    //     appear in the recommendation set at all (frontend_score.is_none()).
    //   - the MCP skill must be present AND clear a score threshold (>= 3.0),
    //     pinning that the domain tokens actually drove the match.
    // The query deliberately avoids tokens that appear in frontend-design's
    // description ("Visual layout and design surface workflow") so the comparison
    // isolates domain-token relevance rather than coincidental substring hits.
    use crate::tests::make_server;

    let server = make_server();
    server
        .with_global_store(|store| {
            let frontend = HubCapability {
                id: "skill:frontend-design".to_string(),
                cap_type: "skill".to_string(),
                name: "frontend-design".to_string(),
                version: 1,
                description: "Visual layout and design surface workflow".to_string(),
                definition: serde_json::json!({"policy": {"visibility": "discoverable"}})
                    .to_string(),
                enabled: true,
                review_status: "approved".to_string(),
                health_status: "healthy".to_string(),
                last_error: None,
                last_success_at: None,
                last_failure_at: None,
                fail_streak: 0,
                active_version: None,
                exposure_mode: "direct".to_string(),
                uses: 0,
                successes: 0,
                failures: 0,
                avg_rating: 0.0,
                last_used: None,
                created_at: String::new(),
                updated_at: String::new(),
            };
            store.hub_register(&frontend).map_err(|e| e.to_string())?;
            let mcp = HubCapability {
                id: "skill:mcp-schema-debug".to_string(),
                name: "mcp-schema-debug".to_string(),
                description: "Debug MCP schema arguments and hub_call serialization".to_string(),
                ..frontend.clone()
            };
            store.hub_register(&mcp).map_err(|e| e.to_string())
        })
        .expect("seed skills");

    let skills =
        super::support::recommend_skills_light(&server, "Exa hub_call arguments serialization", 10)
            .expect("recommend light");
    let mcp_score = skills
        .iter()
        .find(|skill| skill.get("id").and_then(|v| v.as_str()) == Some("skill:mcp-schema-debug"))
        .map(|skill| skill.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0));
    let frontend_score = skills
        .iter()
        .find(|skill| skill.get("id").and_then(|v| v.as_str()) == Some("skill:frontend-design"))
        .map(|skill| skill.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0));
    // Hard lower-bound on the MCP skill: domain tokens must clear a threshold so
    // the match is driven by vocabulary, not incidental substring noise.
    assert!(
        mcp_score.is_some_and(|m| m >= 3.0),
        "expected MCP-specific skill to match the task tokens with score >= 3.0, got {mcp_score:?}"
    );
    // Hard exclusion: frontend-design shares no domain token with the query, so
    // it must NOT appear in the recommendation set at all (not merely
    // lower-ranked). This restores the frozen bound the migration had loosened.
    assert!(
        frontend_score.is_none(),
        "frontend-design shares no domain token with the query and must be ABSENT from \
         recommendations, not merely lower-ranked; got {frontend_score:?} in {skills:?}"
    );
}

#[test]
fn task_brief_router_selects_review_sop_for_pr_review() {
    let intent = classify_task_intent("看看这几个 PR 下面 Gemini 的回复");
    let sops = build_selected_sops(intent, &[]);
    let plan = build_tool_plan(intent);

    assert_eq!(intent, "review_request");
    assert!(sops
        .iter()
        .any(|sop| sop.get("id").and_then(|v| v.as_str()) == Some("skill:waza-check")));
    assert!(plan.iter().any(|step| {
        step.get("tool").and_then(|v| v.as_str()) == Some("tachi_task")
            && step.get("action").and_then(|v| v.as_str()) == Some("board")
    }));
}

#[test]
fn task_brief_router_selects_targeted_verification_for_build_run() {
    let intent = classify_task_intent("帮我编译二进制并且跑起来验证功能");
    let sops = build_selected_sops(intent, &[]);

    assert_eq!(intent, "test_request");
    assert!(sops.iter().any(|sop| {
        sop.get("id").and_then(|v| v.as_str()) == Some("skill:coding-test-strategy")
    }));
}

#[test]
fn task_brief_router_maps_waza_capability_intents() {
    let cases = [
        (
            "帮我做一个前端页面截图视觉检查",
            "design_request",
            "skill:waza-design",
        ),
        (
            "润色这段 release notes",
            "write_request",
            "skill:waza-write",
        ),
        (
            "读一下 https://example.com/report.pdf",
            "read_request",
            "skill:waza-read",
        ),
        (
            "检查 agent MCP 配置健康度",
            "health_request",
            "skill:waza-health",
        ),
    ];
    for (task, expected_intent, expected_skill) in cases {
        let intent = classify_task_intent(task);
        let sops = build_selected_sops(intent, &[]);
        assert_eq!(intent, expected_intent, "task: {task}");
        assert!(
            sops.iter()
                .any(|sop| { sop.get("id").and_then(|v| v.as_str()) == Some(expected_skill) }),
            "expected {expected_skill} for {task}, got {sops:?}"
        );
    }
}

#[test]
fn task_brief_router_avoids_ascii_substring_false_positives() {
    assert_eq!(
        classify_task_intent("explain why this failed"),
        "explain_request"
    );
    assert_eq!(
        classify_task_intent("decide whether this is specific enough"),
        "other"
    );
    assert_eq!(classify_task_intent("run ci checks"), "test_request");
}

#[test]
fn task_brief_router_generic_kankan_is_not_always_review() {
    assert_eq!(classify_task_intent("看看这个报错"), "fix_request");
    assert_eq!(
        classify_task_intent("看看这几个 PR 下面 Gemini 的回复"),
        "review_request"
    );
    assert_eq!(classify_task_intent("看一下 PRs"), "review_request");
}

#[test]
fn task_brief_router_classifies_exploration_as_research() {
    assert_eq!(
        classify_task_intent(
            "List the .rs files under crates/memory-core/src and produce a one-line summary of each."
        ),
        "research_request"
    );
    assert_eq!(
        classify_task_intent("explore the codebase and give an overview"),
        "research_request"
    );
    assert_eq!(
        classify_task_intent("梳理一下这个模块的结构"),
        "research_request"
    );
    assert_eq!(
        classify_task_intent("map out the module dependency graph"),
        "research_request"
    );
    // Gemini guard: a coding task phrased with "map the ..." must NOT be
    // misread as research (the reason "map the" was narrowed to "map out").
    assert_ne!(
        classify_task_intent("map the array values into the new struct fields"),
        "research_request"
    );
}

#[test]
fn feature_board_filter_matches_stable_fields_only() {
    let needles = vec!["flow_20260608t000000z_feature".to_string()];

    assert!(value_contains_any(
        &json!({
            "dispatch_id": "dispatch-1",
            "summary": "work for flow_20260608T000000Z_feature",
            "metadata": {
                "debug_note": "unrelated"
            }
        }),
        &needles
    ));
    assert!(!value_contains_any(
        &json!({
            "dispatch_id": "dispatch-2",
            "summary": "unrelated work",
            "metadata": {
                "debug_note": "flow_20260608T000000Z_feature"
            }
        }),
        &needles
    ));
}

#[test]
fn task_brief_router_appends_hub_skill_recommendations() {
    let recommended = vec![json!({
        "id": "skill:mcp-schema-debug",
        "name": "mcp-schema-debug",
        "description": "Debug MCP schema arguments",
        "score": 5
    })];
    let sops = build_selected_sops("fix_request", &recommended);

    assert!(sops
        .iter()
        .any(|sop| sop.get("id").and_then(|v| v.as_str()) == Some("skill:waza-hunt")));
    assert!(sops.iter().any(|sop| {
        sop.get("id").and_then(|v| v.as_str()) == Some("skill:mcp-schema-debug")
            && sop.get("source").and_then(|v| v.as_str()) == Some("hub_recommendation")
    }));
}

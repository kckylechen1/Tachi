use super::*;

#[tokio::test]
async fn tachi_skill_loadout_resolves_dispatch_profile_skills_and_bundle() {
    let server = make_server();

    server
        .with_global_store(|store| {
            for (id, name, description) in [
                (
                    "skill:superpowers-writing-plans",
                    "superpowers-writing-plans",
                    "Write implementation plans and validate architecture before execution.",
                ),
                (
                    "skill:waza-think",
                    "waza-think",
                    "Think through architecture decisions and tradeoffs.",
                ),
                (
                    "skill:superpowers-subagent-driven-development",
                    "superpowers-subagent-driven-development",
                    "Split implementation plans into bounded subagent work.",
                ),
                (
                    "skill:coding-architecture-decision",
                    "coding-architecture-decision",
                    "Record architecture decisions for coding tasks.",
                ),
            ] {
                store
                    .hub_register(&make_skill_capability(id, name, description, "listed"))
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed skill registry");

    server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("loadout-eval-001".to_string()),
            task: "Plan a dispatch policy change before implementation".to_string(),
            agent: "claude".to_string(),
            outcome: "success".to_string(),
            task_type: Some("plan_request".to_string()),
            profile: Some("claude_plan".to_string()),
            risk: Some("medium".to_string()),
            duration_ms: Some(12_000),
            skills_used: vec![
                "skill:superpowers-writing-plans".to_string(),
                "skill:waza-think".to_string(),
            ],
            cost_tokens: Some(1000),
            cost_usd: Some(0.02),
            quality_score: Some(0.9),
            notes: Some("Seed loadout eval feedback.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow-loadout-eval".to_string()),
            issue_ref: Some("kckylechen1/tachi#194".to_string()),
            pr_ref: None,
            evidence_refs: vec![
                "docs/engineering/architecture/dispatch-policy-learning-spec.md".to_string(),
            ],
            tests_run: vec!["cargo test -p memory-server skill_tests".to_string()],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
        }))
        .await
        .expect("seed loadout eval row");

    let result = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: Some("plan a dispatch policy change before implementation".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(50),
            skill_id: None,
            args: None,
            profile: Some("claude_plan".to_string()),
            host: Some("codex".to_string()),
            skill_limit: Some(3),
            capability_limit: Some(2),
            pack_limit: Some(1),
            include_section: Some(true),
        }))
        .await
        .expect("tachi_skill loadout should succeed");

    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["profile"], json!("claude_plan"));
    assert_eq!(json["role"], json!("planner"));
    assert!(json["resolved_skills"]
        .as_array()
        .expect("resolved skills")
        .iter()
        .any(|skill| skill == "skill:superpowers-subagent-driven-development"));
    assert_eq!(
        json["skill_loadout"]["passive_traits"][0],
        json!("plan_before_execute")
    );
    assert!(json["strong_against"]
        .as_array()
        .expect("strong_against array")
        .contains(&json!("planning")));
    assert!(json["weak_against"]
        .as_array()
        .expect("weak_against array")
        .contains(&json!("direct_execution")));
    assert!(json["capability_bundle"]["section"]["block"]
        .as_str()
        .unwrap_or("")
        .contains("Capability Bundle"));
    assert_eq!(json["eval_feedback"]["source"], json!("live_eval"));
    assert_eq!(json["eval_feedback"]["profile_samples"], json!(1));
    assert!(json["eval_feedback"]["performance_by_task"]
        .as_array()
        .expect("performance_by_task array")
        .iter()
        .any(|row| row["task_type"] == json!("plan_request")));
    assert!(json["eval_feedback"]["guidance"]
        .as_array()
        .expect("guidance array")
        .iter()
        .any(|note| note
            .as_str()
            .is_some_and(|note| note.contains("low_sample"))));
    assert_eq!(json["mbit_card"]["auto_capability_bundle"], json!(true));
}

#[tokio::test]
async fn tachi_skill_loadout_rejects_unknown_profile() {
    let server = make_server();

    let err = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: Some("plan a dispatch policy change".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: None,
            skill_id: None,
            args: None,
            profile: Some("unknown_profile".to_string()),
            host: None,
            skill_limit: None,
            capability_limit: None,
            pack_limit: None,
            include_section: None,
        }))
        .await
        .expect_err("unknown loadout profile should fail");

    assert!(err.contains("Unknown dispatch profile 'unknown_profile'"));
    assert!(err.contains("claude_plan"));
}

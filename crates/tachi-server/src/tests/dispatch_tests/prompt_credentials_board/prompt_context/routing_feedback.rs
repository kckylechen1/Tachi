use super::*;

#[tokio::test]
async fn dispatch_prompt_includes_task_route_overlay() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &dispatch_params(Some("codex"), "帮我编译二进制并且跑起来验证功能"),
    )
    .await;

    assert!(prompt.contains("## Tachi task route"), "{prompt}");
    assert!(prompt.contains("intent: test_request"), "{prompt}");
    assert!(prompt.contains("skill:coding-test-strategy"), "{prompt}");
    // #1690 C3 S1 re-anchor: the task-route overlay keeps the SOP advisory, but
    // with empty skills the prompt must NOT synthesize a Required skill
    // invocation section from it (task-selected SOP promotion is retired).
    assert!(
        !prompt.contains("## Required skill invocation"),
        "empty skills must not auto-inject a skill-invocation section from task-selected SOPs: {prompt}"
    );
    assert!(prompt.contains("tachi_unstick(check)"), "{prompt}");
}

#[tokio::test]
async fn dispatch_prompt_injects_applicable_feedback_rules_separately() {
    let server = make_server();
    let rule_id = save_grep_evidence_feedback_rule(&server).await;

    let mut params = dispatch_params(
        Some("codex"),
        "Review the repo for unused functions and dead code claims.",
    );
    params.profile = Some("codex_55_review".to_string());
    params.stage = Some("review".to_string());

    let assembly = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params).await;
    let prompt = assembly.prompt;
    assert!(prompt.contains("## Applicable feedback rules"), "{prompt}");
    assert!(prompt.contains("Subagent audit prompts require explicit search evidence"));
    assert!(prompt.contains("grep_commands"));
    assert!(prompt.contains("paths_searched"));
    assert!(!prompt.contains("## Relevant context from Tachi memory/wiki"));
    assert_eq!(assembly.feedback_rules["status"], json!("applied"));
    assert_eq!(assembly.feedback_rules["rules"][0]["id"], json!(rule_id));
}

#[tokio::test]
async fn applicable_feedback_rules_fall_back_from_project_to_global_rules() {
    let (server, _temp_home) = make_server_with_temp_home();
    let rule_id = save_grep_evidence_feedback_rule(&server).await;

    let rules = crate::feedback_rule_ops::applicable_feedback_rules(
        &server,
        crate::feedback_rule_ops::FeedbackRuleQuery {
            task: "Review unused code and require grep evidence".to_string(),
            task_type: Some("code_audit".to_string()),
            profile: Some("codex_55_review".to_string()),
            stage: Some("review".to_string()),
            keywords: vec!["grep".to_string(), "unused".to_string()],
            project: Some("missing-project-feedback-fallback".to_string()),
        },
    )
    .await;

    assert!(
        rules.iter().any(|rule| rule.id == rule_id
            && rule.scope == "global"
            && rule.authority == "behavior_patch"),
        "expected global fallback rule, got {rules:#?}"
    );
}

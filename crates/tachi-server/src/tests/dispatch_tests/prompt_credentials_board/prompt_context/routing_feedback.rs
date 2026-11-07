use super::*;
use crate::feedback_rule_ops::{applicable_feedback_rules, FeedbackRuleQuery};

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
    assert!(prompt.contains("tachi_memory(alerts)"), "{prompt}");
}

#[tokio::test]
async fn dispatch_prompt_injects_applicable_feedback_rules_separately() {
    let server = make_server();
    let rule_id = save_grep_evidence_feedback_rule(&server, "global").await;

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
    let rule_id = save_grep_evidence_feedback_rule(&server, "global").await;

    let rules = crate::feedback_rule_ops::applicable_feedback_rules(
        &server,
        crate::feedback_rule_ops::FeedbackRuleQuery {
            task: "Review unused code and require grep evidence".to_string(),
            task_type: Some("code_audit".to_string()),
            profile: Some("codex_55_review".to_string()),
            stage: Some("review".to_string()),
            keywords: vec!["grep".to_string(), "unused".to_string()],
            scope_hints: Vec::new(),
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

#[tokio::test]
async fn feedback_routing_hints_preserve_applicability_and_trigger_matching() {
    let server = make_server();
    let mut rule = make_entry("feedback-routing-hints");
    rule.path = "/feedback/audit/search-evidence".to_string();
    rule.topic = "Search evidence for code audits".to_string();
    rule.summary = "Review unused functions and dead code with grep evidence.".to_string();
    rule.text = rule.summary.clone();
    rule.category = "prompt_rule".to_string();
    rule.metadata = json!({
        "kind": "feedback_rule",
        "applies_to": {
            "task_type": ["review::request"],
            "profiles": ["CodeAuditReviewer"],
            "stage": ["pre_review"]
        },
        "trigger_keywords": ["unused", "dead code", "flow_20261107_audit"],
        "prompt_patch": "Require search evidence for dead-code claims."
    });
    server
        .with_global_store(|store| store.upsert(&rule).map_err(|error| error.to_string()))
        .expect("seed feedback with metadata-only routing labels");

    let query = FeedbackRuleQuery {
        task: "Review unused functions and dead code claims with grep evidence.".to_string(),
        task_type: Some("review::request".to_string()),
        profile: Some("CodeAuditReviewer".to_string()),
        stage: Some("pre_review".to_string()),
        keywords: Vec::new(),
        scope_hints: vec![
            "flow_20261107_audit".to_string(),
            "7f298071823a497294cca57f1bd630ae".to_string(),
        ],
        project: None,
    };
    let hits = applicable_feedback_rules(&server, query.clone()).await;
    let hit = hits
        .iter()
        .find(|hit| hit.id == rule.id)
        .expect("routing and scope labels must not require matching literal object fields");
    let without_scope = applicable_feedback_rules(
        &server,
        FeedbackRuleQuery {
            scope_hints: Vec::new(),
            ..query.clone()
        },
    )
    .await;
    let without_scope = without_scope.iter().find(|hit| hit.id == rule.id).unwrap();
    assert!(
        hit.score > without_scope.score,
        "raw scope references must still match metadata trigger keywords"
    );

    for (field, mismatch) in [
        (
            "task_type",
            FeedbackRuleQuery {
                task_type: Some("implementation".to_string()),
                ..query.clone()
            },
        ),
        (
            "profile",
            FeedbackRuleQuery {
                profile: Some("other_reviewer".to_string()),
                ..query.clone()
            },
        ),
        (
            "stage",
            FeedbackRuleQuery {
                stage: Some("implementation".to_string()),
                ..query
            },
        ),
    ] {
        let hits = applicable_feedback_rules(&server, mismatch).await;
        assert!(
            hits.iter().all(|hit| hit.id != rule.id),
            "mismatched {field} must still refuse the rule: {hits:#?}"
        );
    }
}

#[tokio::test]
async fn feedback_routing_hints_do_not_replace_user_object_evidence() {
    let server = make_server();
    let rule_id = save_grep_evidence_feedback_rule(&server, "global").await;
    let task = "Review the repo for unused functions and dead code claims.";
    let query = FeedbackRuleQuery {
        task: task.to_string(),
        // This routing label also exists in the rule's keywords. It must not
        // let an unrelated user object pass the identifier gate's OR match.
        task_type: Some("code_audit".to_string()),
        profile: Some("codex_55_review".to_string()),
        stage: Some("review".to_string()),
        keywords: Vec::new(),
        scope_hints: Vec::new(),
        project: None,
    };
    for (case, task, keywords, expected) in [
        (
            "stored inline object",
            format!("{task} `grep`"),
            vec![],
            true,
        ),
        (
            "missing camel-case object",
            format!("{task} QuartzLedgerCutover"),
            vec![],
            false,
        ),
        (
            "missing inline object",
            format!("{task} `qzx`"),
            vec![],
            false,
        ),
        (
            "stored explicit keyword",
            task.to_string(),
            vec!["code_audit".to_string()],
            true,
        ),
        (
            "missing explicit keyword",
            task.to_string(),
            vec!["QuartzLedgerCutover".to_string()],
            false,
        ),
    ] {
        let hits = applicable_feedback_rules(
            &server,
            FeedbackRuleQuery {
                task,
                keywords,
                ..query.clone()
            },
        )
        .await;
        if expected {
            assert!(
                hits.iter().any(|hit| hit.id == rule_id),
                "{case}: {hits:#?}"
            );
        } else {
            assert!(hits.is_empty(), "{case}: {hits:#?}");
        }
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn task_briefing_preserves_feedback_with_automatic_scope_hints() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let runs_root = crate::dispatch_ops::runs_dir_for_server(&server);
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", &runs_root);
    let rule_id = save_grep_evidence_feedback_rule(&server, "global").await;

    let mut params = crate::tests::dispatch_tests::task_params("brief");
    params.task = Some("Review the repo for unused functions and dead code claims.".to_string());
    params.task_type = Some("review_request".to_string());
    params.profile = Some("codex_55_review".to_string());
    params.stage = Some("review".to_string());
    params.flow_id = Some("flow_20261107T000000Z_feedback_hints".to_string());
    params.issue_ref = Some("example/ReviewWorkflowNotes#2225".to_string());
    params.pr_ref = Some("example/ReviewWorkflowNotes#2226".to_string());
    params.include_global = true;
    params.compact = Some(false);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("briefing with scope references should succeed");
    let briefing: Value = serde_json::from_str(&raw).expect("briefing JSON");
    assert_eq!(briefing["feedback_rules"]["status"], json!("applied"));
    assert!(briefing["feedback_rules"]["rules"]
        .as_array()
        .is_some_and(|rules| rules.iter().any(|rule| rule["id"] == json!(rule_id))));
}

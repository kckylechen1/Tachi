use super::*;

#[tokio::test]
async fn tachi_complete_scrubs_secretish_eval_metadata() {
    let server = make_server();
    let secret = "eval-redaction-fixture-token";

    let resp = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("eval-secret-redaction".to_string()),
            task: "Verify eval secret hygiene".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("high".to_string()),
            duration_ms: Some(100),
            skills_used: vec!["skill:check".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.8),
            notes: Some(format!("Do not persist api_key={secret}")),
            trajectory: Some(json!([
                {
                    "step": "run",
                    "env": {
                        "OPENAI_API_KEY": secret
                    }
                }
            ])),
            diff: Some(format!("+OPENAI_API_KEY={secret}\n")),
            worktree: None,
            subagents: vec![TachiSubagentEvalParams {
                role: "reviewer".to_string(),
                agent: "kimi".to_string(),
                model: Some("kimi-for-coding".to_string()),
                task: Some("Review secret hygiene".to_string()),
                task_type: Some("review_request".to_string()),
                outcome: Some("useful".to_string()),
                usefulness_score: Some(0.9),
                failure_mode: None,
                verification_impact: Some("changed_plan".to_string()),
                verification_present: true,
                evaluator: Some("leader".to_string()),
                plan_delta: Some("modified".to_string()),
                human_override: false,
                retry_count: 0,
                notes: Some(format!("Saw token={secret} in draft evidence")),
                latency_ms: Some(10),
                input_tokens: None,
                output_tokens: None,
                cost_tokens: None,
                cost_usd: None,
                ..Default::default()
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: vec![format!("evidence token={secret}")],
            tests_run: vec![format!("cargo test # token={secret}")],
            diff_present: None,
            scope: Some("project".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
        }))
        .await
        .expect("tachi_complete should succeed");

    assert!(!resp.contains(secret), "response leaked secret: {resp}");
    // Receipt contract (#528): the default response signals redaction via the
    // secret_redactions count; the redacted content itself lives in storage
    // (asserted below), not echoed back.
    let bundle: Value = serde_json::from_str(&resp).expect("complete JSON");
    assert!(
        bundle["secret_redactions"].as_u64().unwrap_or(0) > 0,
        "redaction count should be reported: {bundle:#}"
    );
    let memory_id = bundle["eval_entry"]["id"].as_str().expect("memory id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: memory_id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("eval memory should be readable");
    assert!(
        !fetched.contains(secret),
        "eval memory leaked secret: {fetched}"
    );
    assert!(
        fetched.contains("[REDACTED]"),
        "eval memory should persist redacted evidence"
    );
}

#[tokio::test]
async fn tachi_complete_scrubs_harness_native_subagent_fields() {
    let server = make_server();
    let native_agent_secret = "sk-nativeAgentSecret000000000001";
    let next_prompt_secret = "ghp_nextPromptDeltaSecret000000000001";
    let harness_bearer = "harnessBearerSecret000000000001";
    let harness_secret = format!("Authorization: Bearer {harness_bearer}");

    let resp = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("eval-harness-native-redaction".to_string()),
            task: "Verify harness-native subagent redaction".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("high".to_string()),
            duration_ms: Some(100),
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.9),
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: vec![TachiSubagentEvalParams {
                role: "executor".to_string(),
                agent: "codex".to_string(),
                outcome: Some("useful".to_string()),
                verification_present: true,
                native_agent_id: Some(format!("native worker {native_agent_secret}")),
                next_prompt_delta: Some(format!("carry forward {next_prompt_secret}")),
                harness: Some(format!("codex-cli {harness_secret}")),
                ..Default::default()
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: vec!["cargo test harness-native redaction".to_string()],
            diff_present: None,
            scope: Some("project".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
        }))
        .await
        .expect("tachi_complete should succeed");

    assert!(
        !resp.contains(native_agent_secret)
            && !resp.contains(next_prompt_secret)
            && !resp.contains(harness_bearer),
        "response leaked harness-native secret: {resp}"
    );
    let bundle: Value = serde_json::from_str(&resp).expect("complete JSON");
    assert!(
        bundle["secret_redactions"].as_u64().unwrap_or(0) >= 3,
        "redaction count should cover each shaped subagent secret: {bundle:#}"
    );

    let memory_id = bundle["eval_entry"]["id"].as_str().expect("memory id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: memory_id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("eval memory should be readable");

    for raw_secret in [native_agent_secret, next_prompt_secret, harness_bearer] {
        assert!(
            !fetched.contains(raw_secret),
            "eval memory leaked raw secret {raw_secret}: {fetched}"
        );
    }
    for redacted in [
        "native worker [REDACTED]",
        "carry forward [REDACTED]",
        "Authorization: Bearer [REDACTED]",
    ] {
        assert!(
            fetched.contains(redacted),
            "eval memory should persist redacted field value {redacted}: {fetched}"
        );
    }
}

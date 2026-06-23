use super::*;

#[tokio::test]
async fn dispatch_prompt_does_not_require_self_complete_without_tachi_mcp() {
    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "read-only worker");
    params.inject_tachi_mcp = Some(false);
    params.mcp_access = Some(DispatchMcpAccessParams {
        inject_tachi_mcp: Some(false),
        inject_hub_mcps: Some(false),
        allowed_facades: vec!["tachi_memory".to_string()],
        allowed_mcp_servers: Vec::new(),
        github_read: Some(false),
        write_actions: Some(false),
        issue_refs: Vec::new(),
        pr_refs: Vec::new(),
        fallback: None,
    });

    let prompt = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params)
        .await
        .prompt;
    assert!(
        prompt.contains("leader will call `tachi_task(action=\"complete\")`"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("- Call `tachi_task(action=\"complete\")` when done"),
        "{prompt}"
    );

    params.inject_tachi_mcp = Some(true);
    let prompt = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params)
        .await
        .prompt;
    assert!(
        prompt.contains("- Call `tachi_task(action=\"complete\")` when done"),
        "{prompt}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_rejects_unsupported_mcp_injection_before_run_dir() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "unsupported mcp injection");
    params.inject_tachi_mcp = Some(true);

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("codex mcp injection should be rejected before run creation");
    assert!(err.contains("not supported for the codex backend"), "{err}");

    let runs_dir = temp_home.path().join("runs");
    assert!(
        !runs_dir.exists()
            || std::fs::read_dir(&runs_dir)
                .expect("read runs dir")
                .next()
                .is_none(),
        "unsupported dispatch validation must not leave orphaned run dirs"
    );
}

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
    assert!(prompt.contains("## Required skill invocation"), "{prompt}");
    assert!(prompt.contains("tachi_progress_check(check)"), "{prompt}");
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

#[tokio::test]
async fn dispatch_prompt_invokes_stage_and_waza_skills_for_execute_slice() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            agent: Some("claude".to_string()),
            profile: None,
            task: "修好 memory-server 报错，先找根因再改".to_string(),
            cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: None,
            stage: Some("execute:runtime".to_string()),
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        },
    )
    .await;

    assert!(prompt.contains("## Required skill invocation"), "{prompt}");
    assert!(
        prompt.contains("Using skills: <ids>"),
        "worker should be told to declare skill usage: {prompt}"
    );
    assert!(
        prompt.contains("### skill:superpowers-executing-plans"),
        "stage skill should be injected for execute:* stage: {prompt}"
    );
    assert!(
        prompt.contains("### skill:waza-hunt"),
        "debug task should inject Waza hunt: {prompt}"
    );
    assert!(
        prompt.contains("embedded_contract"),
        "child prompt should include fallback contract when tachi_skill MCP is unavailable: {prompt}"
    );
}

#[tokio::test]
async fn dispatch_prompt_invokes_native_subagent_factory_for_dispatch_stage() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            agent: Some("codex".to_string()),
            profile: None,
            task: "Split this implementation plan into worker slices and run review gates"
                .to_string(),
            cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: None,
            stage: Some("dispatch".to_string()),
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        },
    )
    .await;

    assert!(
        prompt.contains("### skill:superpowers-subagent-driven-development"),
        "dispatch stage should invoke the worker factory skill: {prompt}"
    );
    assert!(
        prompt.contains("### skill:superpowers-executing-plans"),
        "dispatch stage should still carry the execution skill: {prompt}"
    );
    assert!(
        prompt.contains("### skill:waza-tachi"),
        "dispatch stage should carry the Tachi workflow skill: {prompt}"
    );
}

#[tokio::test]
async fn dispatch_prompt_injects_sft_examples_as_style_only_context() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("live-dispatch-memory");
            live.path = "/scratch/sigil/live-dispatch".to_string();
            live.text = "fix dispatch routing regression current implementation fact.".to_string();
            live.summary = "Current dispatch fact".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut sft = make_entry("sft-dispatch-example");
            sft.path = "/sft/v4/strict/engineering/42".to_string();
            sft.text = "[结论] fix dispatch routing regression historical answer shape.\n[根因] Historical root cause.\n[方案] Historical proposal.\n[反方案] Historical anti-pattern.\n[验证] Historical verification.".to_string();
            sft.summary = "SFT dispatch example".to_string();
            sft.topic = "sft-memory".to_string();
            sft.importance = 1.0;
            store.upsert(&sft).map_err(|e| e.to_string())
        })
        .expect("seed dispatch SFT example");

    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &dispatch_params(Some("codex"), "Fix dispatch routing regression"),
    )
    .await;

    assert!(
        prompt.contains("## Relevant context from Tachi memory/wiki"),
        "{prompt}"
    );
    assert!(
        prompt.contains("/scratch/sigil/live-dispatch")
            && prompt.contains("current implementation fact"),
        "live memory should still be normal context: {prompt}"
    );
    assert!(
        prompt.contains("## SFT gold examples (style only, not live facts)"),
        "{prompt}"
    );
    assert!(
        prompt.contains("/sft/v4/strict/engineering/42"),
        "SFT example should be isolated under the SFT section: {prompt}"
    );
    assert!(
        prompt.contains("Do not treat historical SFT samples as current project truth"),
        "{prompt}"
    );
}

#[tokio::test]
async fn dispatch_prompt_includes_profile_overlay_and_capability_bundle() {
    let server = make_server();
    server
        .with_global_store(|store| {
            store
                .set_state(
                    "dispatch_profile_card_overlays",
                    "claude_plan",
                    &json!({
                        "kind": "profile_card_loadout_overlay",
                        "profile": "claude_plan",
                        "add_signature_skills": ["skill:planning-ux-review"],
                        "add_passive_traits": ["evidence_backed_planning"],
                        "add_evidence_required": ["acceptance_criteria"],
                        "add_weak_against": ["plan_request"],
                        "demotion_targets": ["skill:superpowers-writing-plans"],
                        "source_proposal_ids": ["proposal-fixture"],
                    })
                    .to_string(),
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed profile/card overlay");
    let mut params = dispatch_params(Some("claude"), "Plan profile-based MCP access");
    params.profile = Some("claude_plan".to_string());
    params.stage = Some("plan".to_string());
    params.tool_profile = Some("delegate".to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.flow_id = Some("flow-194".to_string());
    params.auto_capability_bundle = Some(true);
    params.mcp_access = Some(DispatchMcpAccessParams {
        inject_tachi_mcp: Some(true),
        inject_hub_mcps: Some(false),
        allowed_facades: vec!["tachi_memory".to_string(), "tachi_wiki".to_string()],
        allowed_mcp_servers: Vec::new(),
        github_read: Some(true),
        write_actions: Some(false),
        issue_refs: vec!["kckylechen1/tachi#194".to_string()],
        pr_refs: Vec::new(),
        fallback: Some("report unavailable context instead of guessing".to_string()),
    });

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(prompt.contains("## Dispatch profile"), "{prompt}");
    assert!(prompt.contains("profile: claude_plan"), "{prompt}");
    assert!(prompt.contains("tachi_tool_profile: delegate"), "{prompt}");
    assert!(
        prompt.contains("issue_ref: kckylechen1/tachi#194"),
        "{prompt}"
    );
    assert!(prompt.contains("- skill_loadout:"), "{prompt}");
    assert!(
        prompt.contains("skill:superpowers-subagent-driven-development"),
        "{prompt}"
    );
    assert!(
        prompt.contains("skill:coding-architecture-decision"),
        "{prompt}"
    );
    assert!(prompt.contains("skill:planning-ux-review"), "{prompt}");
    assert!(
        prompt.contains("projected_signature_skills: skill:planning-ux-review"),
        "{prompt}"
    );
    assert!(
        prompt.contains("projection_status: applied_overlay"),
        "{prompt}"
    );
    assert!(
        prompt.contains("projected_passive_traits: evidence_backed_planning"),
        "{prompt}"
    );
    assert!(
        prompt.contains("passive_traits: plan_before_execute"),
        "{prompt}"
    );
    assert!(prompt.contains("- evidence_contract:"), "{prompt}");
    assert!(
        prompt.contains("required: plan, risks, validation_plan, acceptance_criteria"),
        "{prompt}"
    );
    assert!(
        prompt.contains("projected_required: acceptance_criteria"),
        "{prompt}"
    );
    assert!(
        prompt.contains("evidence_projection_status: applied_overlay"),
        "{prompt}"
    );
    assert!(prompt.contains("- mbit_card_evolution:"), "{prompt}");
    assert!(
        prompt.contains("projected_weak_against: plan_request"),
        "{prompt}"
    );
    assert!(
        prompt.contains("demotion_targets: skill:superpowers-writing-plans"),
        "{prompt}"
    );
    assert!(prompt.contains("## Capability Bundle"), "{prompt}");
}

#[tokio::test]
async fn dispatch_prompt_trace_records_capability_bundle_injection() {
    let server = make_server();
    let mut params = dispatch_params(Some("claude"), "Plan profile-based MCP access");
    params.profile = Some("claude_plan".to_string());
    params.stage = Some("plan".to_string());
    params.auto_capability_bundle = Some(true);

    let assembly = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params).await;

    assert!(
        assembly.prompt.contains("## Capability Bundle"),
        "{}",
        assembly.prompt
    );
    assert_eq!(assembly.capability_bundle["requested"], json!(true));
    assert_eq!(assembly.capability_bundle["status"], json!("injected"));
    assert_eq!(assembly.capability_bundle["source"], json!("params"));
    assert_eq!(assembly.capability_bundle["disabled"], json!(false));
    assert_eq!(assembly.capability_bundle["injected"], json!(true));
    assert!(
        assembly.capability_bundle["section"]["block"]
            .as_str()
            .is_some_and(|block| block.contains("## Capability Bundle")),
        "trace should retain the injected section: {}",
        assembly.capability_bundle
    );
}

#[tokio::test]
async fn dispatch_prompt_trace_records_capability_bundle_disabled() {
    let server = make_server();
    let mut params = dispatch_params(Some("claude"), "Plan profile-based MCP access");
    params.profile = Some("claude_plan".to_string());
    params.stage = Some("plan".to_string());
    params.auto_capability_bundle = Some(false);

    let assembly = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params).await;

    assert!(
        !assembly.prompt.contains("## Capability Bundle"),
        "disabled bundle should not be injected: {}",
        assembly.prompt
    );
    assert_eq!(assembly.capability_bundle["requested"], json!(false));
    assert_eq!(assembly.capability_bundle["status"], json!("disabled"));
    assert_eq!(assembly.capability_bundle["source"], json!("params"));
    assert_eq!(assembly.capability_bundle["disabled"], json!(true));
    assert_eq!(assembly.capability_bundle["injected"], json!(false));
    assert_eq!(
        assembly.capability_bundle["reason"],
        json!("auto_capability_bundle=false")
    );
}

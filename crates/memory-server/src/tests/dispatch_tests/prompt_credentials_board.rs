use super::super::{make_entry, make_server, make_server_with_temp_home};
use super::{
    dispatch_params, save_grep_evidence_feedback_rule, wait_for_dispatch_result, EnvVarGuard,
};
use crate::tool_params::{DispatchMcpAccessParams, TachiBoardParams, TachiDispatchParams};
use crate::vault_ops::{VaultInitParams, VaultSetParams};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
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

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dispatch_response_and_flow_card_link_capability_bundle_artifact() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let flow_id = format!(
        "flow_20260609T000003Z_capability_bundle_card_{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id.as_str()).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow_id.clone(),
            "status": "active",
            "dispatch_ids": [],
        }))
        .expect("serialize status"),
    )
    .expect("seed status");

    let mut params = dispatch_params(Some("custom"), "smoke capability bundle artifact");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.flow_id = Some(flow_id.clone());
    params.auto_capability_bundle = Some(true);

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    assert_eq!(response["capability_bundle"]["requested"], json!(true));
    assert_eq!(response["capability_bundle"]["status"], json!("injected"));
    assert_eq!(response["capability_bundle"]["injected"], json!(true));
    assert_eq!(response["feedback_rules"]["status"], json!("none"));
    let artifact_file = response["capability_bundle_file"]
        .as_str()
        .expect("capability bundle file");
    let artifact: Value =
        serde_json::from_str(&std::fs::read_to_string(artifact_file).expect("artifact"))
            .expect("artifact JSON");
    assert_eq!(artifact["requested"], json!(true));
    assert_eq!(artifact["status"], json!("injected"));
    assert_eq!(artifact["injected"], json!(true));
    assert_eq!(artifact["feedback_rules"]["status"], json!("none"));

    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["capability_bundle"]["requested"], json!(true));
    assert_eq!(card["capability_bundle"]["injected"], json!(true));
    assert_eq!(
        card["capability_bundle_file"].as_str(),
        Some(artifact_file),
        "flow card should link the same capability bundle artifact"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dispatch_explicit_false_writes_disabled_capability_bundle_artifact() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let mut params = dispatch_params(Some("custom"), "smoke disabled capability bundle artifact");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.auto_capability_bundle = Some(false);

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(response["capability_bundle"]["status"], json!("disabled"));
    assert_eq!(response["capability_bundle"]["requested"], json!(false));
    assert_eq!(response["capability_bundle"]["disabled"], json!(true));
    assert_eq!(response["capability_bundle"]["injected"], json!(false));

    let artifact_file = response["capability_bundle_file"]
        .as_str()
        .expect("capability bundle file");
    let artifact: Value =
        serde_json::from_str(&std::fs::read_to_string(artifact_file).expect("artifact"))
            .expect("artifact JSON");
    assert_eq!(artifact["status"], json!("disabled"));
    assert_eq!(artifact["source"], json!("params"));

    let prompt_file = response["prompt_file"].as_str().expect("prompt file");
    let prompt = std::fs::read_to_string(prompt_file).expect("prompt");
    assert!(
        !prompt.contains("## Capability Bundle"),
        "disabled dispatch should not inject bundle section: {prompt}"
    );
}

#[test]
fn dispatch_ids_are_unique_within_same_second() {
    let now = chrono::Utc::now();
    let id_a = crate::dispatch_ops::new_dispatch_id(now, "my agent/here");
    let id_b = crate::dispatch_ops::new_dispatch_id(now, "my agent/here");
    assert_ne!(
        id_a, id_b,
        "same second + same agent must still produce unique IDs"
    );
    let prefix = format!("{}-my-agent-here", now.format("%Y%m%dT%H%M%SZ"));
    assert!(
        id_a.starts_with(&prefix),
        "id_a should start with expected prefix: {id_a}"
    );
    assert!(
        id_b.starts_with(&prefix),
        "id_b should start with expected prefix: {id_b}"
    );
}

#[tokio::test]
async fn dispatch_rejects_unknown_agent_with_fleet_hint() {
    let server = make_server();
    let err = crate::dispatch_ops::handle_tachi_dispatch(
        &server,
        TachiDispatchParams {
            agent: Some("gemini".to_string()),
            profile: None,
            task: "noop".to_string(),
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
            stage: None,
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
    .await
    .expect_err("gemini should not be in the fleet");
    assert!(err.contains("Unknown agent"), "err: {err}");
    assert!(err.contains("claude"), "err: {err}");
    assert!(err.contains("grok"), "err: {err}");
}

#[tokio::test]
async fn custom_dispatch_rejects_mcp_injection() {
    let server = make_server();
    let mut params = dispatch_params(Some("custom"), "should fail before subprocess");
    params.inject_tachi_mcp = Some(true);
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("custom backend must reject MCP injection");
    assert!(
        err.contains("custom backend"),
        "unexpected custom injection error: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_response_includes_suggested_complete_payload() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let mut params = dispatch_params(Some("custom"), "smoke custom dispatch completion skeleton");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.flow_id = Some("flow-complete-skeleton".to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let suggested = &response["suggested_complete_command"];

    assert_eq!(suggested["tool"], serde_json::json!("tachi_task"));
    assert_eq!(
        suggested["arguments"]["action"],
        serde_json::json!("complete")
    );
    assert_eq!(
        suggested["arguments"]["dispatch_id"], response["dispatch_id"],
        "completion skeleton should carry dispatch_id"
    );
    assert_eq!(
        suggested["arguments"]["profile"],
        serde_json::json!("glm_51_impl")
    );
    assert_eq!(
        suggested["arguments"]["flow_id"],
        serde_json::json!("flow-complete-skeleton")
    );
    assert!(suggested["arguments"]["tests_run"].is_array());
    assert!(suggested["arguments"]["evidence_refs"].is_array());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_injects_env_without_response_secret() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        credentials_dir.join("dispatch.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "credential_profiles": {
                "dispatch_env_profile": {
                    "provider": "test",
                    "entries": {
                        "api_key": "DISPATCH_PROFILE_SECRET"
                    },
                    "allowed_consumers": {
                        "agents": ["custom"]
                    },
                    "materializers": [
                        {
                            "type": "env",
                            "source": "api_key",
                            "target": "PROFILE_ENV_SECRET"
                        }
                    ]
                }
            }
        }))
        .expect("serialize credential profile"),
    )
    .expect("write credential profile");
    let secret_value = "dispatch-profile-secret-value";

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch credential password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "DISPATCH_PROFILE_SECRET".to_string(),
            value: secret_value.to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch credential test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(Some("custom"), "smoke credential dispatch env");
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.credential_profiles = vec!["dispatch_env_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; print('present' if os.environ.get('PROFILE_ENV_SECRET') == 'dispatch-profile-secret-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with materialized credential");
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["credentials"][0]["steps"][0]["output"],
        serde_json::json!("env:PROFILE_ENV_SECRET")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["status"],
        serde_json::json!("prepared_env")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["redacted"],
        serde_json::json!(true)
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("present"),
        "subprocess should receive credential env without printing it; result={result}"
    );
    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory present");
    assert!(
        trajectory.contains("\"event\":\"credentials_materialized\""),
        "trajectory should record redacted credential materialization: {trajectory}"
    );
    assert!(
        !trajectory.contains(secret_value),
        "trajectory must not leak secret value: {trajectory}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_injects_config_overlay_env_without_response_secret() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "credential_profiles": {
                "opencode_config_profile": {
                    "provider": "opencode",
                    "entries": {
                        "api_key": "OPENCODE_ROUTER_SECRET"
                    },
                    "allowed_consumers": {
                        "agents": ["custom"]
                    },
                    "materializers": [
                        {
                            "type": "config_overlay",
                            "source": "api_key",
                            "target": "OPENCODE_CONFIG_CONTENT",
                            "template": {
                                "provider": "openai",
                                "apiKey": "{{secret}}"
                            }
                        }
                    ]
                }
            }
        }))
        .expect("serialize credential profile"),
    )
    .expect("write credential profile");
    let secret_value = "opencode-router-secret-value";

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch config credential password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENCODE_ROUTER_SECRET".to_string(),
            value: secret_value.to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch config credential test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(Some("custom"), "smoke credential config overlay dispatch");
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.credential_profiles = vec!["opencode_config_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import json, os; cfg=json.loads(os.environ.get('OPENCODE_CONFIG_CONTENT','{}')); print('config-present' if cfg.get('apiKey') == 'opencode-router-secret-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with materialized config overlay");
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["credentials"][0]["steps"][0]["output"],
        serde_json::json!("config_overlay:OPENCODE_CONFIG_CONTENT")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["status"],
        serde_json::json!("prepared_config_env")
    );
    assert_eq!(
        response["credentials"][0]["steps"][0]["redacted"],
        serde_json::json!(true)
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("config-present"),
        "subprocess should receive rendered config without printing it; result={result}"
    );
    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory present");
    assert!(
        trajectory.contains("\"event\":\"credentials_materialized\""),
        "trajectory should record redacted credential materialization: {trajectory}"
    );
    assert!(
        !trajectory.contains(secret_value),
        "trajectory must not leak secret value: {trajectory}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_declared_credentials_materialize_without_explicit_params() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "credential_profiles": {
                "opencode_shared": {
                    "provider": "opencode",
                    "entries": {
                        "api_key": "OPENCODE_SHARED_TEST_SECRET"
                    },
                    "allowed_consumers": {
                        "profiles": ["opencode_builder"]
                    },
                    "materializers": [
                        {
                            "type": "config_overlay",
                            "source": "api_key",
                            "target": "OPENCODE_CONFIG_CONTENT",
                            "template": {
                                "provider": "openai",
                                "apiKey": "{{secret}}"
                            }
                        }
                    ]
                }
            }
        }))
        .expect("serialize credential profile"),
    )
    .expect("write credential profile");
    let secret_value = "opencode-shared-profile-secret";

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch profile credential password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENCODE_SHARED_TEST_SECRET".to_string(),
            value: secret_value.to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch profile credential binding test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(None, "smoke profile-declared credential dispatch");
    params.profile = Some("opencode_builder".to_string());
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import json, os; cfg=json.loads(os.environ.get('OPENCODE_CONFIG_CONTENT','{}')); print('profile-config-present' if cfg.get('apiKey') == 'opencode-shared-profile-secret' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with profile-declared credential");
    assert!(
        !raw.contains(secret_value),
        "dispatch response must not leak secret value: {raw}"
    );
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["profile"]["credential_profiles"][0],
        serde_json::json!("opencode_shared")
    );
    assert_eq!(
        response["credentials"][0]["profile"],
        serde_json::json!("opencode_shared")
    );
    assert_eq!(
        response["credentials"][0]["consumer"],
        serde_json::json!("opencode_builder")
    );

    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("profile-config-present"),
        "subprocess should receive profile-declared credential config; result={result}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_declared_credentials_respect_profile_allowlist() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        r#"{
          "credential_profiles": {
            "opencode_shared": {
              "entries": { "api_key": "OPENCODE_SHARED_DENIED_SECRET" },
              "allowed_consumers": { "profiles": ["opencode_builder"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "OPENCODE_DENIED_ENV" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch profile denied password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENCODE_SHARED_DENIED_SECRET".to_string(),
            value: "denied-profile-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch profile allowlist denial test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(None, "should fail before spawn");
    params.profile = Some("glm_51_impl".to_string());
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["opencode_shared".to_string()];
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("non-allowed selected profile should be denied before spawn");
    assert!(err.contains("denied_consumer"), "unexpected error: {err}");
    assert!(err.contains("glm_51_impl"), "unexpected error: {err}");
    assert!(
        !err.contains("denied-profile-secret-value"),
        "error must not leak secret value: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_profile_credentials_can_allow_backend_agent_name() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("dispatch.json"),
        r#"{
          "credential_profiles": {
            "backend_agent_profile": {
              "entries": { "api_key": "BACKEND_AGENT_SECRET" },
              "allowed_consumers": { "agents": ["custom"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "BACKEND_AGENT_ENV" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch backend password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "BACKEND_AGENT_SECRET".to_string(),
            value: "backend-agent-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "backend agent allowlist test".to_string(),
            allowed_agents: Some(vec!["custom".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(None, "profile selected but agent allowlist is backend");
    params.profile = Some("glm_51_impl".to_string());
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["backend_agent_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; print('present' if os.environ.get('BACKEND_AGENT_ENV') == 'backend-agent-secret-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("backend agent allowlist should work even with selected dispatch profile");
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(
        response["credentials"][0]["consumer"],
        serde_json::json!("custom"),
        "credential consumer should be backend agent when allowed_consumers.agents matches"
    );
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(result.contains("present"), "result={result}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_requires_unlocked_vault_before_spawn() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("dispatch.json"),
        r#"{
          "credential_profiles": {
            "locked_env_profile": {
              "entries": { "api_key": "LOCKED_DISPATCH_SECRET" },
              "allowed_consumers": { "agents": ["custom"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "LOCKED_ENV_SECRET" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch locked password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "LOCKED_DISPATCH_SECRET".to_string(),
            value: "locked-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch credential lock test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    let mut params = dispatch_params(Some("custom"), "should fail before spawn");
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["locked_env_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('should-not-run')".to_string(),
    ];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("locked vault should fail dispatch before spawn");
    assert!(err.contains("requires unlocked Vault secret"), "err: {err}");
    assert!(err.contains("Vault is locked"), "err: {err}");
    let runs_dir = temp_home.path().join("runs");
    let run_dir = std::fs::read_dir(&runs_dir)
        .expect("runs dir exists")
        .next()
        .expect("failed dispatch should leave run status")
        .expect("run dir entry")
        .path();
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status should exist"),
    )
    .expect("status JSON");
    assert_eq!(status["state"], serde_json::json!("TASK_STATE_FAILED"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_credential_profile_denies_consumer_before_decrypting_secret() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("dispatch.json"),
        r#"{
          "credential_profiles": {
            "denied_env_profile": {
              "entries": { "api_key": "DENIED_DISPATCH_SECRET" },
              "allowed_consumers": { "agents": ["other-agent"] },
              "materializers": [
                { "type": "env", "source": "api_key", "target": "DENIED_ENV_SECRET" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch denied password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "DENIED_DISPATCH_SECRET".to_string(),
            value: "denied-secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "dispatch credential deny test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    let mut params = dispatch_params(Some("custom"), "should fail before decrypt");
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.credential_profiles = vec!["denied_env_profile".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('should-not-run')".to_string(),
    ];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("denied consumer should fail dispatch before decrypt");
    assert!(
        err.contains("not ready for consumer 'custom'"),
        "err: {err}"
    );
    assert!(err.contains("denied_consumer"), "err: {err}");
    assert!(
        !err.contains("Vault is locked"),
        "denied profile should fail before decrypting/unlocking secret: {err}"
    );
    assert!(
        !err.contains("denied-secret-value"),
        "redacted readiness error must not leak secret: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_legacy_vault_env_binding_still_injects_without_credential_profile() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let project = tempfile::tempdir().expect("temp project");
    let nested = project.path().join("src/nested");
    std::fs::create_dir_all(project.path().join(".tachi")).expect("create .tachi dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        project.path().join(".tachi/vault.env"),
        "LEGACY_DISPATCH_ENV=vault:LEGACY_DISPATCH_SECRET\n",
    )
    .expect("write vault.env");

    server
        .vault_init(Parameters(VaultInitParams {
            password: "dispatch legacy password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "LEGACY_DISPATCH_SECRET".to_string(),
            value: "legacy-dispatch-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "legacy dispatch env test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let mut params = dispatch_params(Some("custom"), "smoke legacy vault env dispatch");
    params.cwd = Some(nested.to_string_lossy().to_string());
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; print('present' if os.environ.get('LEGACY_DISPATCH_ENV') == 'legacy-dispatch-value' else 'missing')".to_string(),
    ];

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("legacy env dispatch should start");
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert!(
        response["credentials"]
            .as_array()
            .is_some_and(|v| v.is_empty()),
        "legacy env path should not synthesize credential reports: {response:#}"
    );
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_dispatch_result(&run_dir).await;
    assert!(
        result.contains("present"),
        "legacy vault env should still reach subprocess; result={result}"
    );
    let trajectory =
        std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory present");
    assert!(
        trajectory.contains("\"event\":\"legacy_vault_env_injected\""),
        "legacy env injection should be auditable without values: {trajectory}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_surfaces_dispatch_run_ledger() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "99991231T235959Z-test-run-ledger-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create run ledger fixture");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "custom",
            "task": "smoke board run ledger",
            "state": "TASK_STATE_WORKING",
            "updated_at": "9999-12-31T23:59:59Z",
            "exit_code": null,
            "result_written": false,
            "harness_transport": "acpx",
            "execution_backend": "acpx",
            "acpx": {
                "agent": "codex",
                "mode": "session",
                "session": "raven",
                "permissions": "approve-reads"
            },
            "acpx_events": {
                "events_file": "/tmp/acpx_events.jsonl",
                "mapped_events": 2,
                "final_response_extracted": true
            },
        }))
        .expect("serialize status fixture"),
    )
    .expect("write status fixture");

    let board_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
        },
    )
    .await
    .expect("board should render");
    let board: serde_json::Value = serde_json::from_str(&board_raw).expect("board JSON");
    assert!(
        board["run_count"].as_u64().unwrap_or(0) >= 1,
        "board must include run-ledger rows even if kanban search misses: {board:#}"
    );
    assert!(
        board["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| { task["dispatch_id"].as_str() == Some(dispatch_id.as_str()) }),
        "board tasks should include dispatch {dispatch_id}: {board:#}"
    );
    assert!(
        board["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task.get("run_dir").and_then(|v| v.as_str()).is_some()
        }),
        "dispatch should carry run_dir from run ledger: {board:#}"
    );
    assert!(
        board["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task["execution_backend"].as_str() == Some("acpx")
                && task["acpx"]["session"].as_str() == Some("raven")
                && task["acpx_events"]["final_response_extracted"].as_bool() == Some(true)
        }),
        "dispatch should carry acpx metadata from run ledger: {board:#}"
    );
    let _ = std::fs::remove_dir_all(&run_dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_marks_abandoned_working_run_as_failed() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "20260608T000000Z-stale-run-ledger-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create stale run fixture");
    let stale_updated_at = (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "stale run should not clog working board",
            "state": "TASK_STATE_WORKING",
            "updated_at": stale_updated_at,
            "exit_code": null,
            "result_written": false,
            "timeout_secs": 5,
        }))
        .expect("serialize stale status fixture"),
    )
    .expect("write stale status fixture");

    let failed_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("failed".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
        },
    )
    .await
    .expect("failed board should render");
    let failed: serde_json::Value = serde_json::from_str(&failed_raw).expect("failed board JSON");
    assert!(
        failed["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task["state"].as_str() == Some("TASK_STATE_FAILED")
                && task["stale"].as_bool() == Some(true)
                && task["stale_reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("WORKING"))
        }),
        "failed board should include stale derived run: {failed:#}"
    );

    let working_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("working".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
        },
    )
    .await
    .expect("working board should render");
    let working: serde_json::Value =
        serde_json::from_str(&working_raw).expect("working board JSON");
    assert!(
        !working["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["dispatch_id"].as_str() == Some(dispatch_id.as_str())),
        "stale run should not remain on working board: {working:#}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_caps_corrupt_huge_timeout_before_duration_math() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "20260608T000001Z-huge-timeout-run-ledger-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create huge-timeout run fixture");
    let stale_updated_at = (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339();
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "corrupt huge timeout must not panic board",
            "state": "TASK_STATE_WORKING",
            "updated_at": stale_updated_at,
            "exit_code": null,
            "result_written": false,
            "timeout_secs": i64::MAX,
        }))
        .expect("serialize huge-timeout status fixture"),
    )
    .expect("write huge-timeout status fixture");

    let failed_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("failed".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
        },
    )
    .await
    .expect("board should not panic on corrupt huge timeout");
    let failed: serde_json::Value = serde_json::from_str(&failed_raw).expect("failed board JSON");
    assert!(
        failed["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task["state"].as_str() == Some("TASK_STATE_FAILED")
                && task["stale"].as_bool() == Some(true)
        }),
        "capped huge timeout should still allow stale classification: {failed:#}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}

// ─── Phase 6: Dispatch V2 two-stage smoke test ──────────────────────────────
//
// Spawns the full V2 flow against a fake `claude` binary that emits a
// canned plan envelope. Marked `#[ignore]` because:
//   * it writes under a temp `TACHI_HOME` and shells out to `bash`;
//   * it requires `bash` on PATH and a writable temp dir;
//   * it mutates env vars (CLAUDE_BIN, DISPATCH_V2_ENABLED, TACHI_HOME)
//     so it must not run concurrently with other env-sensitive tests.
//
// Run explicitly via:
//     cargo test -p memory-server v2_two_stage_smoke -- --ignored --test-threads=1

#[tokio::test]
#[ignore]
async fn v2_two_stage_smoke() {
    use std::io::Write;

    // Isolated TACHI_HOME so run files don't pollute real one.
    let temp_home = std::env::temp_dir().join(format!("tachi-v2-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_home).expect("create temp tachi home");

    // Fake claude binary: prints a JSON envelope matching the pool's
    // expected `{"result": "..."}` shape, embedding a valid plan.
    let fake_claude = temp_home.join("claude-test");
    {
        let mut f = std::fs::File::create(&fake_claude).expect("create fake claude");
        // The pool invokes `claude -p --output-format json --dangerously-skip-permissions`
        // with the prompt on stdin. We ignore stdin and emit a fixed envelope.
        writeln!(
            f,
            "#!/usr/bin/env bash\ncat <<'JSON'\n{{\"result\":\"## Goal\\nDo the smoke test.\\n\\n## Steps\\n1. inspect\\n2. ship\\n\\n## Files\\n- src/lib.rs\\n\\n## Validation\\n- cargo test\\n\"}}\nJSON"
        )
        .unwrap();
    }
    let mut perms = std::fs::metadata(&fake_claude).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&fake_claude, perms).unwrap();

    // Activate V2 + isolate.
    std::env::set_var("TACHI_HOME", &temp_home);
    std::env::set_var("CLAUDE_BIN", &fake_claude);
    std::env::set_var("TACHI_CLAUDE_SKIP_PERMISSIONS", "true");
    std::env::set_var("DISPATCH_V2_ENABLED", "true");
    std::env::set_var("DISPATCH_V2_PLAN_REVIEW", "false");

    let server = make_server();

    // We can't easily call the private handle_tachi_dispatch from
    // outside the crate, but tests live inside the crate so the
    // `pub(crate)` visibility is accessible via crate path.
    let mut params = dispatch_params(Some("custom"), "smoke v2");
    // Execute stage uses a no-op command so the test doesn't need
    // a working claude/codex CLI for Stage 2.
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let resp_json = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("v2 dispatch should succeed");

    let resp: serde_json::Value = serde_json::from_str(&resp_json).expect("v2 response JSON");
    assert_eq!(resp["v2"], serde_json::json!(true), "response: {resp:#}");
    let run_dir = resp["run_dir"].as_str().expect("run_dir present");
    let run_dir = std::path::PathBuf::from(run_dir);

    // Stage 1 artifacts exist immediately.
    let plan = std::fs::read_to_string(run_dir.join("plan.md")).expect("plan.md written");
    assert!(plan.contains("## Goal"), "plan.md content: {plan}");
    assert!(plan.contains("## Validation"), "plan.md content: {plan}");

    let trajectory_path = run_dir.join("trajectory.jsonl");
    let mut trajectory = std::fs::read_to_string(&trajectory_path).expect("trajectory present");
    for _ in 0..30 {
        if trajectory.contains("\"event\":\"execute_started\"") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        trajectory = std::fs::read_to_string(&trajectory_path).expect("trajectory present");
    }
    assert!(trajectory.contains("\"event\":\"dispatch_started\""));
    assert!(trajectory.contains("\"event\":\"plan_generated\""));
    assert!(trajectory.contains("\"event\":\"execute_started\""));
    let progress =
        std::fs::read_to_string(run_dir.join("progress.jsonl")).expect("progress present");
    assert!(progress.contains("\"event\":\"dispatch_started\""));
    assert!(progress.contains("\"event\":\"plan_generated\""));

    // Wait briefly for the spawned stage-2 task to write final status.json.
    for _ in 0..30 {
        if let Ok(raw) = std::fs::read_to_string(run_dir.join("status.json")) {
            if let Ok(status) = serde_json::from_str::<serde_json::Value>(&raw) {
                if status["duration_ms_plan"].as_u64().is_some()
                    && status["duration_ms_execute"].as_u64().is_some()
                {
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status.json written"),
    )
    .expect("status.json valid");
    assert_eq!(status["v2"], serde_json::json!(true));
    assert!(status["duration_ms_plan"].as_u64().is_some());
    assert_eq!(status["plan_review_status"], serde_json::json!("approved"));

    // Cleanup.
    std::env::remove_var("CLAUDE_BIN");
    std::env::remove_var("DISPATCH_V2_ENABLED");
    std::env::remove_var("DISPATCH_V2_PLAN_REVIEW");
    std::env::remove_var("TACHI_HOME");
    let _ = std::fs::remove_dir_all(&temp_home);
}

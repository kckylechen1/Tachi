use super::*;

/// #1690 C3 S1 discriminator: with empty `params.skills` and no profile, a
/// dispatch prompt must carry NO skill invocation at all — stage-default and
/// task-selected-SOP auto-derivation are retired (frozen law: no hidden
/// automatic skill injection into prompts; the delete list names "skill
/// recommendation and auto-selection"). RED pre-repair: this prompt contained
/// `skill:superpowers-executing-plans` (stage=execute default) and
/// `skill:waza-hunt` (task-routed SOP promotion); GREEN post-repair: neither
/// the section nor any derived skill renders. The stage→instruction advisory
/// (a projection of the explicit stage choice) still renders.
#[tokio::test]
async fn empty_skills_dispatch_prompt_injects_no_auto_derived_stage_or_sop_skills() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            staffing_reason: tachi_params::TachiDispatchReason::ExplicitUserRequest,
            agent: Some("claude".to_string()),
            profile: None,
            task: "修好 tachi-server 报错，先找根因再改".to_string(),
            execution_level: None,
            cwd: None,
            env_id: None,
            unmanaged_cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            completion_predicate: None,
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
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            verbose: None,
            inject_card: None,
        },
    )
    .await;

    assert!(
        !prompt.contains("## Required skill invocation"),
        "empty skills must not synthesize a skill-invocation section: {prompt}"
    );
    assert!(
        !prompt.contains("### skill:superpowers-executing-plans"),
        "the stage=execute default skill must not be auto-injected: {prompt}"
    );
    assert!(
        !prompt.contains("### skill:waza-hunt"),
        "a task-routed SOP must not be auto-promoted into the prompt: {prompt}"
    );
}

/// #1690 C3 S1 discriminator: empty skills + stage=dispatch must not
/// auto-inject the old dispatch-stage default loadout (subagent factory,
/// execution skill, waza-tachi).
#[tokio::test]
async fn empty_skills_dispatch_prompt_injects_no_dispatch_stage_default_skills() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            staffing_reason: tachi_params::TachiDispatchReason::ExplicitUserRequest,
            agent: Some("codex".to_string()),
            profile: None,
            task: "Split this implementation plan into worker slices and run review gates"
                .to_string(),
            execution_level: None,
            cwd: None,
            env_id: None,
            unmanaged_cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            completion_predicate: None,
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
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            verbose: None,
            inject_card: None,
        },
    )
    .await;

    assert!(
        !prompt.contains("skill:superpowers-subagent-driven-development"),
        "dispatch stage must not auto-inject the subagent factory: {prompt}"
    );
    assert!(
        !prompt.contains("skill:superpowers-executing-plans"),
        "dispatch stage must not auto-inject the execution skill: {prompt}"
    );
    assert!(
        !prompt.contains("skill:waza-tachi"),
        "dispatch stage must not auto-inject waza-tachi: {prompt}"
    );
}

/// #1690 C3 S1 GREEN keeper: the EXPLICIT `skills` param still renders — only
/// the auto-derivation is retired, not the invocation surface itself.
#[tokio::test]
async fn explicit_skills_still_render_in_dispatch_prompt() {
    let server = make_server();
    let prompt = crate::dispatch_ops::assemble_prompt(
        &server,
        &TachiDispatchParams {
            staffing_reason: tachi_params::TachiDispatchReason::ExplicitUserRequest,
            agent: Some("claude".to_string()),
            profile: None,
            task: "修好 tachi-server 报错，先找根因再改".to_string(),
            execution_level: None,
            cwd: None,
            env_id: None,
            unmanaged_cwd: None,
            skills: vec!["skill:superpowers-executing-plans".to_string()],
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            completion_predicate: None,
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
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            verbose: None,
            inject_card: None,
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
        "an explicitly requested skill must still render its invocation contract: {prompt}"
    );
    assert!(
        prompt.contains("embedded_contract"),
        "child prompt should include fallback contract when tachi_skill MCP is unavailable: {prompt}"
    );
}

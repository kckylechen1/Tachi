use super::*;

mod opencode_transport;
mod profile_resolution;
mod profiles_listing;
mod risk_classifier;
mod route_policy;
mod scoring;

fn params() -> TachiDispatchParams {
    TachiDispatchParams {
        agent: None,
        profile: Some("claude_plan".to_string()),
        task: "Plan issue #194".to_string(),
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
        stage: None,
        credential_profiles: Vec::new(),
        issue_ref: Some("kckylechen1/tachi#194".to_string()),
        pr_ref: None,
        flow_id: Some("flow-194".to_string()),
        tool_profile: None,
        auto_capability_bundle: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        verbose: None,
        inject_card: None,
    }
}

use super::*;

#[test]
fn dispatch_profile_selects_backend_and_mcp_contract() {
    let mut params = params();
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(params.agent.as_deref(), Some("claude"));
    assert_eq!(params.stage.as_deref(), Some("plan"));
    assert_eq!(params.tool_profile.as_deref(), Some("delegate"));
    assert_eq!(params.inject_tachi_mcp, Some(true));
    assert_eq!(resolved.selected_profile.as_deref(), Some("claude_plan"));
    assert_eq!(resolved.mcp_access.github_read, Some(true));
    assert_eq!(
        resolved.mcp_access.issue_refs,
        vec!["kckylechen1/tachi#194".to_string()]
    );
    assert!(resolved.auto_capability_bundle);
    assert!(params
        .skills
        .iter()
        .any(|skill| skill == SUPERPOWER_WRITING_PLANS));
    assert!(params
        .skills
        .iter()
        .any(|skill| skill == CODING_ARCHITECTURE_DECISION));
    assert_eq!(
        profile_skill_loadout_json(resolve_dispatch_profile("claude_plan").unwrap())
            ["passive_traits"][0],
        json!("plan_before_execute")
    );
    let profile_payload = profile_json(resolve_dispatch_profile("claude_plan").unwrap());
    assert_eq!(profile_payload["card_archetype"], json!("raven"));
    assert_eq!(profile_payload["mbit_card"]["archetype"], json!("raven"));
    assert_eq!(
        profile_payload["mbit_card"]["authority"]["write_code"],
        json!(false)
    );
    assert_eq!(
        profile_payload["mbit_card"]["guidance"]["superpowers"][0],
        json!(SUPERPOWER_WRITING_PLANS)
    );
    assert!(profile_payload["mbit_card"]["moves"]["waza"]
        .as_array()
        .expect("waza moves")
        .contains(&json!(WAZA_THINK)));
    assert_eq!(
        profile_payload["mbit_card"]["personality"]["risk_control"],
        profile_payload["mbit_card"]["stats"]["risk_control"]
    );
    assert_eq!(
        profile_json(resolve_dispatch_profile("glm_51_impl").unwrap())["mbit_card"]["archetype"],
        json!("scv")
    );
    assert_eq!(
        profile_json(resolve_dispatch_profile("deepseek_explore").unwrap())["mbit_card"]
            ["archetype"],
        json!("poke")
    );
}

#[test]
fn dispatch_profile_treats_blank_agent_as_missing() {
    let mut params = params();
    params.agent = Some("   ".to_string());
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(params.agent.as_deref(), Some("claude"));
    assert_eq!(resolved.agent, "claude");
}

#[test]
fn credentialed_dispatch_profile_applies_default_credential_profiles() {
    let mut params = params();
    params.profile = Some("opencode_builder".to_string());
    params.issue_ref = None;
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(params.agent.as_deref(), Some("custom"));
    assert_eq!(params.stage.as_deref(), Some("execute"));
    assert_eq!(params.credential_profiles, vec!["opencode_shared"]);
    assert_eq!(
        resolved.credential_profiles,
        vec!["opencode_shared".to_string()]
    );
    assert_eq!(
        profile_json(resolve_dispatch_profile("opencode_builder").unwrap())["credential_profiles"]
            [0],
        json!("opencode_shared")
    );
}

#[test]
fn dispatch_profile_merges_default_and_explicit_credential_profiles() {
    let mut params = params();
    params.profile = Some("opencode_builder".to_string());
    params.credential_profiles = vec!["extra_project_secret".to_string()];
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(
        params.credential_profiles,
        vec!["extra_project_secret", "opencode_shared"]
    );
    assert_eq!(
        resolved.credential_profiles,
        vec![
            "extra_project_secret".to_string(),
            "opencode_shared".to_string()
        ]
    );
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("profile requires credential profile(s): opencode_shared")));
}

#[test]
fn dispatch_profile_writes_fallback_mcp_access_back_to_params() {
    let mut params = params();
    params.profile = None;
    params.agent = Some("claude".to_string());
    params.inject_tachi_mcp = Some(true);
    params.allowed_mcp_servers = vec!["github".to_string()];
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();

    assert_eq!(
        params
            .mcp_access
            .as_ref()
            .map(|access| access.allowed_mcp_servers.as_slice()),
        Some(&["github".to_string()][..])
    );
    assert_eq!(
        resolved.mcp_access.allowed_mcp_servers,
        vec!["github".to_string()]
    );
}

#[test]
fn explicit_agent_can_override_profile_backend() {
    let mut params = params();
    params.agent = Some("codex".to_string());
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(resolved.agent, "codex");
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("overrides profile backend")));
}

#[test]
fn custom_profile_populates_opencode_command() {
    let mut params = params();
    params.profile = Some("deepseek_explore".to_string());
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();

    assert_eq!(resolved.agent, "custom");
    assert_eq!(
        params.command,
        vec![
            "opencode".to_string(),
            "--pure".to_string(),
            "run".to_string(),
            "--model".to_string(),
            "deepseek/deepseek-v4-flash".to_string()
        ]
    );
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("opencode custom command")));
}

#[test]
fn kimi_ux_profile_is_registered_as_read_only_experience_reviewer() {
    let profile = resolve_dispatch_profile("kimi_ux").expect("kimi_ux profile");
    assert_eq!(profile.backend, "kimi");
    assert_eq!(profile.role, "ux_researcher");
    assert!(!profile.write_actions);
    assert!(profile
        .evidence_required
        .iter()
        .any(|item| item == &"ux_findings"));
    assert!(profile
        .strong_against
        .iter()
        .any(|item| item == &"tool_surface_friction"));
}

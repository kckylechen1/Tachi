use super::*;
use crate::skill_policy::{CODING_ARCHITECTURE_DECISION, SUPERPOWER_WRITING_PLANS, WAZA_THINK};

/// #1814 discriminator: the typed Staff boundary must retain the server
/// resolver's profile semantics without constructing a flat dispatch carrier.
#[test]
fn typed_staff_server_resolver_matches_flat_profile_semantics_table() {
    let server = crate::staffing_ops::tests::test_server();
    let cases = [
        ("canonical codex", "codex_55_review", Some("codex"), None),
        ("deprecated alias", "glm_51_impl", None, None),
        (
            "opencode defaults",
            "opencode_builder",
            Some("opencode"),
            None,
        ),
        (
            "worker override",
            "codex_55_review",
            Some("claude"),
            Some("review"),
        ),
    ];

    for (name, profile, worker, stage) in cases {
        let mut flat = params();
        flat.profile = Some(profile.to_string());
        flat.agent = worker.map(str::to_string);
        flat.stage = stage.map(str::to_string);
        let flat_resolved = resolve_and_apply_dispatch_profile_for_server(&server, &mut flat)
            .unwrap_or_else(|error| panic!("{name}: flat resolver: {error}"));

        let mut staff = tachi_params::StaffAssignmentRequest {
            task: format!("{name} typed resolver parity"),
            staffing_reason: tachi_params::TachiDispatchReason::DurableCrossSession,
            profile: Some(profile.to_string()),
            worker: worker.map(str::to_string),
            stage: stage.map(str::to_string),
            execution_level: None,
            issue_ref: Some("kckylechen1/tachi#194".to_string()),
            pr_ref: None,
            flow_id: None,
            project: None,
            completion_predicate: None,
            recommendation_ref: None,
        };
        let typed = crate::dispatch_profile::resolve_and_apply_staff_assignment_profile_for_server(
            &server, &mut staff,
        )
        .unwrap_or_else(|error| panic!("{name}: typed resolver: {error}"));

        assert_eq!(
            typed.selected_profile, flat_resolved.selected_profile,
            "{name}: profile"
        );
        assert_eq!(typed.agent, flat_resolved.agent, "{name}: worker/backend");
        assert_eq!(
            typed.selected_model, flat_resolved.selected_model,
            "{name}: model/stage defaults"
        );
        assert_eq!(
            serde_json::to_value(&typed.mcp_access).expect("typed MCP JSON"),
            serde_json::to_value(&flat_resolved.mcp_access).expect("flat MCP JSON"),
            "{name}: MCP/tools"
        );
        assert_eq!(
            typed.credential_profiles, flat_resolved.credential_profiles,
            "{name}: credentials"
        );
        assert_eq!(
            typed.launch_command, flat_resolved.launch_command,
            "{name}: OpenCode launch defaults"
        );
        assert_eq!(
            typed.evidence_required, flat_resolved.evidence_required,
            "{name}: evidence"
        );
        assert_eq!(
            typed.fallback_chain, flat_resolved.fallback_chain,
            "{name}: fallback"
        );
        assert_eq!(
            typed.identity_receipt, flat_resolved.identity_receipt,
            "{name}: identity"
        );
    }

    let mut refused = tachi_params::StaffAssignmentRequest {
        task: "typed resolver refusal".to_string(),
        staffing_reason: tachi_params::TachiDispatchReason::DurableCrossSession,
        profile: Some("not-a-profile".to_string()),
        worker: None,
        stage: None,
        execution_level: None,
        issue_ref: None,
        pr_ref: None,
        flow_id: None,
        project: None,
        completion_predicate: None,
        recommendation_ref: None,
    };
    let error = crate::dispatch_profile::resolve_and_apply_staff_assignment_profile_for_server(
        &server,
        &mut refused,
    )
    .expect_err("unknown typed Staff profile must refuse before launch");
    assert!(
        error.contains("Unknown") || error.contains("unknown"),
        "refusal: {error}"
    );
}

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
        tachi_dispatch::profile_skill_loadout_json(
            resolve_dispatch_profile("claude_plan").unwrap()
        )["passive_traits"][0],
        json!("plan_before_execute")
    );
    let profile_payload = profile_json(resolve_dispatch_profile("claude_plan").unwrap());
    assert_eq!(profile_payload["card_archetype"], json!("raven"));
    assert_eq!(profile_payload["authority"]["write_code"], json!(false));
    assert_eq!(
        profile_payload["guidance"]["superpowers"][0],
        json!(SUPERPOWER_WRITING_PLANS)
    );
    assert!(profile_payload["moves"]["waza"]
        .as_array()
        .expect("waza moves")
        .contains(&json!(WAZA_THINK)));
    assert_eq!(
        profile_json(resolve_dispatch_profile("glm_51_impl").unwrap())["card_archetype"],
        json!("scv")
    );
    assert_eq!(
        profile_json(resolve_dispatch_profile("deepseek_explore").unwrap())["card_archetype"],
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
    assert_eq!(params.agent.as_deref(), Some("opencode"));
    assert_eq!(resolved.agent, "opencode");
    assert_eq!(resolved.host_adapter.as_deref(), Some("opencode"));
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
    let profile = profile_json(resolve_dispatch_profile("opencode_builder").unwrap());
    assert_eq!(profile["backend"], json!("opencode"));
    assert_eq!(profile["host_adapter"], json!("opencode"));
    assert_eq!(params.harness_transport.as_deref(), Some("opencode_cli"));
}

#[test]
fn review_stage_profiles_disable_capability_bundle_by_default() {
    const EXPLANATION: &str = "auto_capability_bundle disabled by default for review-stage dispatch (#457); pass auto_capability_bundle=true to override";

    let mut review_params = params();
    review_params.profile = Some("codex_55_review".to_string());
    review_params.stage = None;
    review_params.auto_capability_bundle = None;
    let resolved_review = resolve_and_apply_dispatch_profile(&mut review_params).unwrap();
    assert_eq!(review_params.auto_capability_bundle, Some(false));
    assert!(!resolved_review.auto_capability_bundle);
    assert!(resolved_review
        .route_explanation
        .iter()
        .any(|line| line == EXPLANATION));
    assert!(
        !resolve_dispatch_profile("codex_55_review")
            .expect("codex review profile")
            .auto_capability_bundle
    );

    let mut explicit_review_params = params();
    explicit_review_params.profile = Some("codex_55_review".to_string());
    explicit_review_params.auto_capability_bundle = Some(true);
    let resolved_explicit =
        resolve_and_apply_dispatch_profile(&mut explicit_review_params).unwrap();
    assert_eq!(explicit_review_params.auto_capability_bundle, Some(true));
    assert!(resolved_explicit.auto_capability_bundle);

    let mut execute_params = params();
    execute_params.profile = Some("glm_51_impl".to_string());
    execute_params.stage = None;
    execute_params.auto_capability_bundle = None;
    let resolved_execute = resolve_and_apply_dispatch_profile(&mut execute_params).unwrap();
    assert_eq!(execute_params.auto_capability_bundle, Some(true));
    assert!(resolved_execute.auto_capability_bundle);

    assert!(
        !resolve_dispatch_profile("kimi_ux")
            .expect("kimi ux profile")
            .auto_capability_bundle
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
    params.harness_transport = Some("opencode_cli".to_string());
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
        .any(|line| line.contains("OpenCode CLI transport")));
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

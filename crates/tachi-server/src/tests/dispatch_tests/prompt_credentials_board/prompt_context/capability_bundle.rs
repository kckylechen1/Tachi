use super::*;

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

use super::*;

/// #1690 C3 discriminator K1(b): a seeded signature-skill overlay row does
/// NOT alter the dispatch prompt — the static baseline loadout still renders,
/// the `projected_signature_skills` label is gone, and the overlay row is
/// inert history. The evidence-contract half (what a packet must carry) is
/// enforcement and still projects its overlay into the prompt.
///
/// RED pre-repair: the seeded `add_signature_skills` renders as
/// `projected_signature_skills: skill:planning-ux-review`; GREEN post-repair:
/// the label never renders and the baseline is what shows.
#[tokio::test]
async fn dispatch_prompt_includes_profile_overlay() {
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
                        "add_evidence_required": ["acceptance_criteria"],
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
    // The STATIC baseline signature skills still render.
    assert!(
        prompt.contains("skill:superpowers-subagent-driven-development"),
        "{prompt}"
    );
    assert!(
        prompt.contains("skill:coding-architecture-decision"),
        "{prompt}"
    );
    // The retired overlay-merged skill must NOT render — no label, no skill.
    assert!(
        !prompt.contains("projected_signature_skills"),
        "the retired loadout-evolution projection label must not render: {prompt}"
    );
    assert!(
        !prompt.contains("skill:planning-ux-review"),
        "a seeded signature-skill overlay row must be inert in the prompt: {prompt}"
    );
    assert!(
        prompt.contains("passive_traits: plan_before_execute"),
        "{prompt}"
    );
    // The static loadout renders its own baseline projection marker.
    assert!(
        prompt.contains("projection_status: baseline"),
        "the static baseline loadout must render its projection status: {prompt}"
    );
    // The surviving enforcement half still projects its overlay.
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
    // #1690 C3: the capability-bundle auto-injection is retired end-to-end —
    // the prompt never carries a "## Capability Bundle" section, regardless of
    // what the caller passes.
    assert!(!prompt.contains("## Capability Bundle"), "{prompt}");
    // #1690 slice B: the MBIT/card-personality evolution surface is retired —
    // the overlay prompt must not render it, even with a legacy overlay that
    // once carried the keys.
    assert!(!prompt.contains("mbit_card_evolution"), "{prompt}");
    assert!(!prompt.contains("projected_weak_against"), "{prompt}");
    assert!(!prompt.contains("demotion_targets"), "{prompt}");
}

/// #1690 C3 discriminator (5b): `auto_capability_bundle` is no longer a
/// `TachiDispatchParams` field, so a caller that still passes the key through
/// JSON (serde ignores unknown fields on this struct) must get a prompt with
/// NO capability-bundle section and a trace that no longer carries the bundle
/// payload. RED pre-fix: the params field exists and the prompt gains a
/// "## Capability Bundle" section; GREEN post-fix: no section, no bundle trace.
#[tokio::test]
async fn dispatch_prompt_ignores_retired_bundle_key_and_injects_no_bundle_section() {
    let server = make_server();
    // Deserialize through JSON so the retired key is present on the wire even
    // though it no longer exists on the struct (asserted at the prompt-assembly
    // layer: the assembled prompt is the live entry the dispatch spawns from).
    let params: TachiDispatchParams = serde_json::from_value(json!({
        "agent": "claude",
        "profile": "claude_plan",
        "stage": "plan",
        "task": "Plan profile-based MCP access",
        "staffing_reason": "explicit_user_request",
        "auto_capability_bundle": true,
        "include_capability_bundle": true,
    }))
    .expect("retired bundle keys must be ignored, not rejected");

    let assembly = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params).await;

    assert!(
        !assembly.prompt.contains("## Capability Bundle"),
        "retired bundle key must not inject a bundle section: {}",
        assembly.prompt
    );
    assert!(
        !assembly.prompt.contains("capability_bundle"),
        "retired bundle key must not leak bundle trace into the prompt: {}",
        assembly.prompt
    );
}

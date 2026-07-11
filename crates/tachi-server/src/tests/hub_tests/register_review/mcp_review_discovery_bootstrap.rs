use super::*;
use crate::tool_params::HubReviewParams;
use tachi_hub::capability_callable;

/// Regression test for #968 review-fix: registration intentionally withholds
/// `discovery_status` until review (see `mcp_discovery.rs`'s
/// `hub_register_defers_mcp_discovery_until_review`), so the *discovery-refresh*
/// gate in `refresh_mcp_capability_state` must NOT require `discovery_status`
/// to already be present/"ready" — otherwise an approved+enabled MCP could
/// never reach its first discovery attempt at all, permanently bricking
/// legitimate registrations (and `hub_quick_add`, which drives the same
/// review path).
#[tokio::test]
async fn hub_review_approval_bootstraps_first_discovery_for_capability_missing_discovery_status() {
    let server = make_server();
    let params = HubRegisterParams {
        id: "mcp:bootstrap-on-approval".to_string(),
        cap_type: "mcp".to_string(),
        name: "bootstrap-on-approval".to_string(),
        description: "test discovery bootstrap on approval".to_string(),
        definition: json!({
            "transport": "stdio",
            "command": "/tmp/not-on-allowlist",
            "args": [],
        })
        .to_string(),
        version: 1,
        scope: "global".to_string(),
    };

    server
        .hub_register(Parameters(params))
        .await
        .expect("hub_register should return response");

    // Precondition, pinned by mcp_discovery.rs: registration must NOT have
    // written discovery_status yet.
    let pre_review_cap = server
        .get_capability("mcp:bootstrap-on-approval")
        .expect("capability should be persisted after register");
    let pre_review_def: serde_json::Value = serde_json::from_str(&pre_review_cap.definition)
        .expect("stored definition should be valid JSON");
    assert!(
        pre_review_def.get("discovery_status").is_none(),
        "precondition: registration should not persist discovery_status before approval"
    );
    assert!(
        !capability_callable(&pre_review_cap),
        "precondition: capability missing discovery_status must not be callable"
    );

    // Approve (and implicitly enable, per hub_set_review's enabled_override
    // default for review_status = "approved").
    let review_response = server
        .hub_review(Parameters(HubReviewParams {
            id: "mcp:bootstrap-on-approval".to_string(),
            review_status: "approved".to_string(),
            enabled: None,
        }))
        .await
        .expect("hub_review should return response");
    let review_data: serde_json::Value =
        serde_json::from_str(&review_response).expect("hub_review response should be JSON");
    assert_eq!(review_data.get("updated"), Some(&json!(true)));
    assert_eq!(review_data.get("review_status"), Some(&json!("approved")));

    // The observable that proves the fix: discovery was actually *attempted*
    // during the review-approval refresh, not silently skipped because
    // discovery_status was absent. Since the command is not on the MCP
    // allowlist, discovery fails fast, but that failure is itself proof the
    // discovery path ran (set_mcp_discovery_failure writes discovery_status
    // = "failed", not "missing").
    let post_review_cap = server
        .get_capability("mcp:bootstrap-on-approval")
        .expect("capability should still be persisted after review");
    let post_review_def: serde_json::Value = serde_json::from_str(&post_review_cap.definition)
        .expect("stored definition should be valid JSON after review");
    assert_eq!(
        post_review_def.get("discovery_status"),
        Some(&json!("failed")),
        "approval should have driven the capability through its first discovery attempt \
         (bootstrap), writing a discovery_status rather than leaving it absent"
    );
    assert!(
        post_review_cap.last_error.is_some(),
        "failed discovery attempt should record the error on the capability"
    );

    // Execution gate is untouched by this fix: still fail-closed. A capability
    // whose (attempted, but failed) discovery_status is not "ready" must
    // remain not callable.
    assert!(
        !capability_callable(&post_review_cap),
        "capability_callable must still fail closed when discovery_status != \"ready\""
    );
}

/// Companion pin (base-commit behavior, unchanged by this fix): a capability
/// with an unresolved/pending discovery status must remain not callable for
/// execution regardless of how discovery-refresh eligibility is computed.
#[tokio::test]
async fn capability_with_pending_discovery_status_remains_not_callable() {
    let mut cap = HubCapability {
        id: "mcp:pending-discovery".to_string(),
        cap_type: "mcp".to_string(),
        name: "pending-discovery".to_string(),
        version: 1,
        description: String::new(),
        definition: json!({
            "transport": "stdio",
            "command": "/opt/homebrew/bin/pending-tool",
            "discovery_status": "pending",
        })
        .to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "direct".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    };

    assert!(
        !capability_callable(&cap),
        "discovery_status = \"pending\" (not \"ready\") must not be callable"
    );

    cap.definition = json!({
        "transport": "stdio",
        "command": "/opt/homebrew/bin/pending-tool",
    })
    .to_string();
    assert!(
        !capability_callable(&cap),
        "missing discovery_status must not be callable"
    );
}

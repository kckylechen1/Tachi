use super::make_server;
use crate::tool_params::{
    AgentRegisterParams, ChainSkillsParams, ChainStep, GetMemoryParams, HandoffCheckParams,
    HandoffLeaveParams, HubRegisterParams,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

// ─── Handoff Tests ───────────────────────────────────────────────────────────

fn chain_skill_register_params(id: &str, name: &str, definition: Value) -> HubRegisterParams {
    HubRegisterParams {
        id: id.to_string(),
        cap_type: "skill".to_string(),
        name: name.to_string(),
        description: format!("test chain skill {name}"),
        definition: definition.to_string(),
        version: 1,
        scope: "global".to_string(),
    }
}

#[tokio::test]
async fn chain_skills_mock_step_reports_simulated_raw_output() {
    let server = make_server();
    server
        .hub_register(Parameters(chain_skill_register_params(
            "skill:chain-mock",
            "chain-mock",
            json!({
                "prompt": "Process {{input}}",
                "mock_response": "mocked command output",
                "policy": {"visibility": "discoverable"},
                "inputSchema": {"type": "object"}
            }),
        )))
        .await
        .expect("register mock chain skill");

    let response = server
        .chain_skills(Parameters(ChainSkillsParams {
            initial_input: "start".to_string(),
            steps: vec![ChainStep {
                skill_id: "skill:chain-mock".to_string(),
                extra_args: None,
            }],
        }))
        .await
        .expect("chain_skills mock step");
    let json: Value = serde_json::from_str(&response).expect("chain response json");

    assert_eq!(json["simulated"], json!(true));
    assert_eq!(json["steps"][0]["execution"], json!("mock_response"));
    assert_eq!(
        json["output"],
        json!(format!(
            "{}\n\nmocked command output",
            crate::hub_ops::SIMULATED_SKILL_OUTPUT_MARKER
        ))
    );
    assert_eq!(
        json["warning"],
        json!(crate::hub_ops::SIMULATED_SKILL_OUTPUT_WARNING)
    );
}

#[tokio::test]
async fn chain_skills_document_steps_pipe_verbatim_without_simulation_marker() {
    let server = make_server();
    const FIRST_DOCUMENT: &str = "# First workflow\n\nRead the input.";
    const SECOND_DOCUMENT: &str = "# Second workflow\n\nReturn this exact document.";
    for (id, name, content) in [
        ("skill:chain-doc-one", "chain-doc-one", FIRST_DOCUMENT),
        ("skill:chain-doc-two", "chain-doc-two", SECOND_DOCUMENT),
    ] {
        server
            .hub_register(Parameters(chain_skill_register_params(
                id,
                name,
                json!({
                    "execution": "document",
                    "prompt": "LLM path should be bypassed.",
                    "content": content,
                    "policy": {"visibility": "discoverable"},
                    "inputSchema": {"type": "object"}
                }),
            )))
            .await
            .expect("register document chain skill");
    }

    let response = server
        .chain_skills(Parameters(ChainSkillsParams {
            initial_input: "start".to_string(),
            steps: vec![
                ChainStep {
                    skill_id: "skill:chain-doc-one".to_string(),
                    extra_args: None,
                },
                ChainStep {
                    skill_id: "skill:chain-doc-two".to_string(),
                    extra_args: None,
                },
            ],
        }))
        .await
        .expect("chain_skills document steps");
    let json: Value = serde_json::from_str(&response).expect("chain response json");

    assert_eq!(json["simulated"], json!(false));
    assert_eq!(json["output"], json!(SECOND_DOCUMENT));
    assert!(!json["output"]
        .as_str()
        .expect("output text")
        .starts_with(crate::hub_ops::SIMULATED_SKILL_OUTPUT_MARKER));
    assert_eq!(json["steps"][0]["execution"], json!("document"));
    assert_eq!(json["steps"][1]["execution"], json!("document"));
    assert!(json.get("warning").is_none());
}

#[tokio::test]
async fn handoff_leave_and_check_roundtrip() {
    let server = make_server();

    // Register as "agent-a" first
    server
        .agent_register(Parameters(AgentRegisterParams {
            agent_id: "agent-a".to_string(),
            display_name: Some("Agent A".to_string()),
            capabilities: vec![],
            tool_filter: None,
            rate_limit_rpm: None,
            rate_limit_burst: None,
        }))
        .await
        .expect("register agent-a");

    // Leave a handoff memo targeted at agent-b
    let leave = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Refactored the auth module, tests still failing on OAuth flow".to_string(),
            next_steps: vec![
                "Fix OAuth callback handler".to_string(),
                "Add integration test for token refresh".to_string(),
            ],
            target_agent: Some("agent-b".to_string()),
            context: Some(json!({"files": ["src/auth.rs", "src/oauth.rs"]})),
        }))
        .await
        .expect("handoff_leave should succeed");
    let leave_json: serde_json::Value = serde_json::from_str(&leave).expect("should be JSON");
    assert_eq!(leave_json["status"], json!("memo_left"));
    assert_eq!(leave_json["from_agent"], json!("agent-a"));
    assert!(leave_json["memo_id"].is_string());

    // Check as agent-b — should see the memo
    let check_b = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-b".to_string()),
            acknowledge: false,
        }))
        .await
        .expect("handoff_check for agent-b");
    let check_b_json: serde_json::Value = serde_json::from_str(&check_b).expect("should be JSON");
    assert_eq!(check_b_json["pending_memos"], json!(1));
    assert_eq!(check_b_json["memos"][0]["from_agent"], json!("agent-a"));

    // Check as agent-c — should NOT see it (targeted at agent-b)
    let check_c = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-c".to_string()),
            acknowledge: false,
        }))
        .await
        .expect("handoff_check for agent-c");
    let check_c_json: serde_json::Value = serde_json::from_str(&check_c).expect("should be JSON");
    assert_eq!(check_c_json["pending_memos"], json!(0));

    // Acknowledge as agent-b
    let ack = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-b".to_string()),
            acknowledge: true,
        }))
        .await
        .expect("handoff_check with acknowledge");
    let ack_json: serde_json::Value = serde_json::from_str(&ack).expect("should be JSON");
    assert_eq!(ack_json["pending_memos"], json!(1)); // Returns memos before acking

    // After acknowledgment, check again — should be empty
    let check_after = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-b".to_string()),
            acknowledge: false,
        }))
        .await
        .expect("handoff_check after ack");
    let check_after_json: serde_json::Value =
        serde_json::from_str(&check_after).expect("should be JSON");
    assert_eq!(check_after_json["pending_memos"], json!(0));
}

#[tokio::test]
async fn handoff_untargeted_memo_visible_to_all() {
    let server = make_server();

    // Leave a memo without a target agent
    server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Build system needs migration to Bazel".to_string(),
            next_steps: vec!["Read BUILD.bazel files".to_string()],
            target_agent: None,
            context: None,
        }))
        .await
        .expect("handoff_leave should succeed");

    // Any agent should see the untargeted memo
    let check = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("any-agent".to_string()),
            acknowledge: false,
        }))
        .await
        .expect("handoff_check");
    let check_json: serde_json::Value = serde_json::from_str(&check).expect("should be JSON");
    assert_eq!(check_json["pending_memos"], json!(1));
}

#[tokio::test]
async fn handoff_leave_persists_to_memory_store() {
    let server = make_server();

    let leave = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Persisted handoff test".to_string(),
            next_steps: vec!["Verify persistence".to_string()],
            target_agent: None,
            context: None,
        }))
        .await
        .expect("handoff_leave should succeed");
    let leave_json: serde_json::Value = serde_json::from_str(&leave).expect("should be JSON");
    let memo_id = leave_json["memo_id"].as_str().unwrap();

    // Verify the memo was persisted to the global memory store
    let memory = server
        .get_memory(Parameters(GetMemoryParams {
            id: format!("handoff:{}", memo_id),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory for handoff should succeed");
    let mem_json: serde_json::Value = serde_json::from_str(&memory).expect("should be JSON");
    assert_eq!(mem_json["category"], json!("handoff"));
    assert_eq!(mem_json["retention_policy"], json!("pinned"));
    assert!(
        mem_json["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Persisted handoff test"),
        "persisted memory text should contain the summary"
    );
}

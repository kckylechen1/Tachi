use super::make_server;
use crate::tool_params::{ChainSkillsParams, ChainStep, HubRegisterParams};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

// ─── Chain Skills Tests ────────────────────────────────────────────────────
//
// #1099: this file used to be `handoff_tests.rs` and also covered
// `handoff_leave`/`handoff_check` — those routes are retired (see
// `handoff_ops.rs`'s module doc). Renamed to reflect what actually remains.

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

//! #1319-D2 discriminator: every GENERATED runtime instruction that guides a
//! model toward launching an external worker must use the corrected Staff
//! contract.
//!
//! The retired `tachi_task(action='dispatch')` call and the old
//! `dispatch_reason` field must not appear in generated server/agent/intake/
//! UX guidance, and every `tachi_staff(action='start', ...)` example must name
//! BOTH required fields (`task=` and the typed `staffing_reason=`) — otherwise
//! the generated example deterministically fails the handler's admission gate
//! (`staff_start` rejects a missing reason with zero artifacts).

use serde_json::json;

/// Every `tachi_staff(...)` occurrence in `guidance` whose action is `start`,
/// extracted as the balanced parenthesized example.
fn staff_start_examples(guidance: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = guidance;
    while let Some(start) = rest.find("tachi_staff(") {
        let after = &rest[start + "tachi_staff(".len()..];
        let end = after.find(')').map(|i| i + 1).unwrap_or(after.len());
        let example = format!("tachi_staff({})", &after[..end]);
        if example.contains("action='start'") || example.contains("action=\"start\"") {
            out.push(example);
        }
        rest = &after[end..];
    }
    out
}

fn assert_guidance_contract(label: &str, guidance: &str) {
    assert!(
        !guidance.contains("tachi_task(action='dispatch'")
            && !guidance.contains("tachi_task(action=\"dispatch\""),
        "{label}: generated guidance must not call the retired tachi_task dispatch"
    );
    assert!(
        !guidance.contains("dispatch_reason"),
        "{label}: generated guidance must not use the old dispatch_reason field"
    );
    let examples = staff_start_examples(guidance);
    assert!(
        !examples.is_empty(),
        "{label}: generated guidance must contain a tachi_staff(action='start') example"
    );
    for example in examples {
        assert!(
            example.contains("task=") && example.contains("staffing_reason="),
            "{label}: every staff start example must name BOTH required fields \
             (task= and staffing_reason=): {example}"
        );
    }
}

#[test]
fn server_instructions_name_only_the_corrected_staff_contract() {
    assert_guidance_contract(
        "mcp_server_instructions",
        &crate::server_instructions::mcp_server_instructions(),
    );
}

#[test]
fn setup_wizard_agent_rules_name_only_the_corrected_staff_contract() {
    assert_guidance_contract(
        "agent_memory_rules_block",
        &crate::bootstrap::setup_wizard::agent_rules::agent_memory_rules_block(),
    );
}

#[test]
fn intake_instruction_names_only_the_corrected_staff_contract() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "owner/repo".to_string(),
        number: 1,
        title: "guidance discriminator".to_string(),
        body: None,
        labels: Vec::new(),
        state: None,
        url: "https://github.com/owner/repo/issues/1".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    crate::task_lifecycle::utils::write_intake_instruction(
        tmp.path(),
        "flow-guidance-1",
        "prove generated guidance uses the corrected staff contract",
        &issue,
        &json!({"status": "ready", "dispatch_allowed": true}),
    )
    .expect("write intake instruction");
    let guidance =
        std::fs::read_to_string(tmp.path().join("instruction.md")).expect("read instruction.md");
    assert_guidance_contract("write_intake_instruction", &guidance);
}

#[test]
fn ux_matrix_names_only_the_corrected_staff_contract() {
    let params: crate::tool_params::TachiTaskParams = serde_json::from_value(json!({
        "action": "ux_matrix",
        "task": "prove ux guidance uses the corrected staff contract",
    }))
    .expect("ux_matrix params");
    let matrix = crate::task_lifecycle::release_ux::handle_task_ux_matrix(&params)
        .expect("render ux matrix");
    assert_guidance_contract("handle_task_ux_matrix", &matrix);
}

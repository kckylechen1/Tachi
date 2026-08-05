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

/// Scans `s` (the text immediately after an opening `(` that is already
/// consumed) for the matching close paren, tracking nesting depth so a paren
/// inside the call's own arguments (e.g. a task string mentioning `(P1)`)
/// doesn't truncate the example early. Also tracks quote state for both `'`
/// and `"` (the two quote styles the extractor below recognizes for
/// `action='start'` / `action="start"`): a `)` inside an open quote (e.g.
/// `task='review ) edge'`) does not count toward depth, since it isn't
/// closing the call — it's part of the string literal. Returns the byte
/// offset of the matching `)`, or `None` if depth never reaches zero (an
/// unclosed paren) OR a quote opened inside the call is never closed before
/// EOF (an unclosed quote is just as malformed as an unclosed paren, and
/// must not be silently swallowed as "the rest of the doc isn't part of the
/// example").
fn find_balanced_close(s: &str) -> Option<usize> {
    let mut depth: i32 = 1;
    let mut in_quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = in_quote {
            if c == q {
                in_quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => in_quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every `tachi_staff(...)` occurrence in `guidance` whose action is `start`,
/// extracted as the balanced parenthesized example. An unclosed
/// `tachi_staff(` is a hard test failure (not a silent fallback to
/// end-of-document) — swallowing the rest of the guidance as "the example"
/// would let unrelated `task=`/`staffing_reason=` text elsewhere in the doc
/// paper over a genuinely malformed generated call (a false GREEN).
fn staff_start_examples(guidance: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = guidance;
    while let Some(start) = rest.find("tachi_staff(") {
        let after = &rest[start + "tachi_staff(".len()..];
        let end = find_balanced_close(after).unwrap_or_else(|| {
            panic!(
                "unclosed tachi_staff(...) call in generated guidance (no matching ')'); \
                 near: {:?}",
                &after[..after.len().min(200)]
            )
        });
        let example = format!("tachi_staff({})", &after[..=end]);
        if example.contains("action='start'") || example.contains("action=\"start\"") {
            out.push(example);
        }
        rest = &after[end + 1..];
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

/// #1319 cross-vendor review repro: a `)` inside a quoted `task=` value (a
/// review comment quoting "review ) edge") must not be mistaken for the
/// call's closing paren — that would truncate the example before
/// `staffing_reason` is ever reached, silently dropping the field the
/// contract check exists to catch.
#[test]
fn staff_start_example_survives_paren_inside_quoted_task_value() {
    let guidance = "Call tachi_staff(action='start', task='review ) edge', \
                     staffing_reason='bounded_implementation') to begin.";
    let examples = staff_start_examples(guidance);
    assert_eq!(examples.len(), 1, "expected exactly one extracted example");
    assert_eq!(
        examples[0],
        "tachi_staff(action='start', task='review ) edge', \
         staffing_reason='bounded_implementation')",
        "extraction must reach the call's real closing paren (not the one \
         inside the quoted task value) and must include staffing_reason"
    );
}

/// An unclosed quote inside `tachi_staff(...)` is exactly as malformed as an
/// unclosed paren — both must hard-fail instead of silently treating the
/// rest of the document as part of (or not part of) the example.
#[test]
#[should_panic(expected = "unclosed tachi_staff(...) call")]
fn staff_start_examples_hard_fails_on_unclosed_quote() {
    let guidance = "Call tachi_staff(action='start', task='never closed) to begin.";
    let _ = staff_start_examples(guidance);
}

//! Active `tachi_memory` examples and retired-token discrimination for #1689.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;
use tachi_params::{TachiMemoryParams, TACHI_MEMORY_ACTIONS};

const FIXTURE: &str = include_str!(
    "../../../../../docs/engineering/architecture/memory-action-contract-v1.fixture.json"
);

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).expect("memory action fixture parses")
}

fn active_contract_teaches_memory_action(body: &str, token: &str) -> bool {
    let double_quoted = format!("action=\"{token}\"");
    let single_quoted = format!("action='{token}'");
    let json_action = format!("\"action\":\"{token}\"");
    let rust_stdio_action = format!("(\"tachi_memory\",Some(\"{token}\"))");
    let dotted_action = format!("tachi_memory.{token}");
    let compact: String = body.chars().filter(|ch| !ch.is_whitespace()).collect();
    let backticked_action_list = body.lines().any(|line| {
        line.split_once("`tachi_memory` ")
            .and_then(|(_, suffix)| suffix.split_whitespace().next())
            .filter(|list| list.contains('/'))
            .is_some_and(|list| list.split('/').any(|action| action == token))
    });

    body.contains(&double_quoted)
        || body.contains(&single_quoted)
        || compact.contains(&json_action)
        || compact.contains(&rust_stdio_action)
        || body.contains(&dotted_action)
        || backticked_action_list
}

#[test]
fn active_memory_examples_deserialize_and_cover_exact_final_set() {
    let fixture = fixture();
    assert_eq!(fixture["schema_version"], "memory_action_contract.v1");
    let examples = fixture["active_examples"]
        .as_array()
        .expect("active_examples array");
    let actions: Vec<_> = examples
        .iter()
        .map(|example| {
            serde_json::from_value::<TachiMemoryParams>(example.clone())
                .expect("active example deserializes")
                .action
        })
        .collect();
    assert_eq!(
        actions, TACHI_MEMORY_ACTIONS,
        "active examples must cover the exact final nine-action Memory contract in order"
    );
}

#[test]
fn active_contract_corpus_includes_all_retirement_surfaces_and_pins_nine_action_doc() {
    let fixture = fixture();
    let active_files = fixture["active_contract_files"]
        .as_array()
        .expect("active_contract_files array")
        .iter()
        .map(|path| path.as_str().expect("active contract path"))
        .collect::<BTreeSet<_>>();
    for required in [
        "docs/engineering/architecture/downstream-sync-surface.md",
        "docs/engineering/architecture/facade-granularity-and-profile-alignment.md",
        "docs/engineering/architecture/kernel-surface-v1.fixture.json",
        "crates/tachi-server/src/bootstrap/serve/stdio/tests.rs",
    ] {
        assert!(
            active_files.contains(required),
            "active Memory contract corpus must include {required}",
        );
    }

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let facade_doc = std::fs::read_to_string(
        repo.join("docs/engineering/architecture/facade-granularity-and-profile-alignment.md"),
    )
    .expect("read facade granularity contract");
    let memory_row = facade_doc
        .lines()
        .find(|line| line.starts_with("| `tachi_memory` |"))
        .expect("tachi_memory facade inventory row");
    let documented_count = memory_row
        .split('|')
        .nth(2)
        .expect("Memory action-count column")
        .trim();
    assert_eq!(
        documented_count, "9",
        "active facade inventory must advertise the final nine-action Memory contract",
    );
}

#[test]
fn active_contract_scanner_detects_backticked_memory_action_lists_without_prose_false_positive() {
    let prior_active_row = "| MCP memory actions | `tachi_memory` search/save/briefing/readiness (or HyperMemory aliases) |";
    for listed_action in ["search", "save", "briefing", "readiness"] {
        assert!(
            active_contract_teaches_memory_action(prior_active_row, listed_action),
            "the exact prior comma/slash-list form must parse listed action {listed_action}",
        );
    }

    let prose = "`tachi_memory` readiness moved to the canonical status owner.";
    assert!(
        !active_contract_teaches_memory_action(prose, "readiness"),
        "a prose mention without an action list must not be classified as an active route",
    );
}

#[test]
fn retired_memory_actions_are_rejected_and_absent_from_declared_active_contract_files() {
    let fixture = fixture();
    let retired = fixture["retired_actions"]
        .as_array()
        .expect("retired_actions array");
    assert_eq!(
        retired.len(),
        8,
        "the frozen C2b retirement set has eight actions"
    );
    let mut unique = BTreeSet::new();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let active_files = fixture["active_contract_files"]
        .as_array()
        .expect("active_contract_files array");

    for token in retired {
        let token = token.as_str().expect("retired action token");
        assert!(unique.insert(token), "duplicate retired action {token}");
        for relative in active_files {
            let relative = relative.as_str().expect("active contract path");
            let body = std::fs::read_to_string(repo.join(relative))
                .unwrap_or_else(|error| panic!("read active contract file {relative}: {error}"));
            assert!(
                !active_contract_teaches_memory_action(&body, token),
                "active contract file {relative} still teaches retired Memory action {token}"
            );
        }

        serde_json::from_value::<TachiMemoryParams>(serde_json::json!({"action": token}))
            .expect_err("retired action must be rejected by the public parameter parser");
    }

    let exclusions = fixture["historical_exclusions"]
        .as_array()
        .expect("historical_exclusions array");
    assert!(
        exclusions.len() >= 6,
        "historical/non-contract exclusions must stay explicit rather than silently escaping the scanner"
    );
}

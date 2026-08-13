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
        let double_quoted = format!("action=\"{token}\"");
        let single_quoted = format!("action='{token}'");
        let json_action = format!("\"action\":\"{token}\"");
        for relative in active_files {
            let relative = relative.as_str().expect("active contract path");
            let body = std::fs::read_to_string(repo.join(relative))
                .unwrap_or_else(|error| panic!("read active contract file {relative}: {error}"));
            let compact: String = body.chars().filter(|ch| !ch.is_whitespace()).collect();
            assert!(
                !body.contains(&double_quoted)
                    && !body.contains(&single_quoted)
                    && !compact.contains(&json_action),
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

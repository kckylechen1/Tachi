//! Active `tachi_task` examples and retired-token discrimination for #1687 C1c.
//!
//! The fixture is deliberately a small set of real wire-shaped parameter
//! objects. It is parsed through `TachiTaskParams`, not through a wrapper or a
//! copied enum, so adding/removing an advertised action requires refreshing the
//! actual active examples and the typed inventory together.

use serde_json::Value;
use std::collections::BTreeSet;
use tachi_params::{TachiTaskAction, TachiTaskParams};

const FIXTURE: &str = include_str!(
    "../../../../../docs/engineering/architecture/task-action-contract-v1.fixture.json"
);

#[test]
fn active_task_examples_deserialize_and_cover_exact_primary_set() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("task action fixture parses");
    assert_eq!(
        fixture["schema_version"], "task_action_contract.v1",
        "active task examples must use the frozen fixture schema"
    );
    let examples = fixture["active_examples"]
        .as_array()
        .expect("active_examples array");
    let mut actions = Vec::new();
    for example in examples {
        let params: TachiTaskParams =
            serde_json::from_value(example.clone()).expect("active example deserializes");
        actions.push(params.action.as_str());
    }
    assert_eq!(
        actions,
        TachiTaskAction::primary_wire_strings(),
        "active examples must cover the exact ten-action Task contract in order"
    );
}

#[test]
fn all_c1a_c1b_c1c_retired_tokens_are_rejected_and_absent_from_active_examples() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("task action fixture parses");
    let active =
        serde_json::to_string(&fixture["active_examples"]).expect("active examples serialize");
    let retired = tachi_params::TACHI_TASK_RETIRED_ACTIONS;
    let mut unique = BTreeSet::new();
    for token in retired {
        assert!(
            unique.insert(*token),
            "retired inventory contains duplicate {token}"
        );
        assert!(
            !active.contains(token),
            "active Task examples must not contain retired token {token}"
        );
        let wire = serde_json::json!({"action": token});
        let err = serde_json::from_value::<TachiTaskParams>(wire)
            .expect_err("retired token must not deserialize as TachiTaskParams");
        assert!(
            err.to_string().contains("unknown variant"),
            "retired token {token} should be rejected by the wire enum: {err}"
        );
        assert!(
            token.parse::<TachiTaskAction>().is_err(),
            "retired token {token} must be rejected by typed FromStr"
        );
    }
}

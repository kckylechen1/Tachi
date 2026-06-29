use super::*;

#[test]
fn tachi_memory_action_schema_declares_enum_values() {
    let schema = rmcp::schemars::schema_for!(TachiMemoryParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("briefing")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("get")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("readiness")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("recall_simulate")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("recall_proposals")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("review_recall_proposal")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("apply_recall_proposals")));
}

#[test]
fn tachi_event_action_schema_declares_enum_values() {
    let schema = rmcp::schemars::schema_for!(TachiEventParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("emit")));
    assert!(values.contains(&json!("query")));
    assert!(values.contains(&json!("metrics")));
    assert!(values.contains(&json!("project")));
    assert!(values.contains(&json!("promote")));
    assert!(values.contains(&json!("context")));
    assert!(values.contains(&json!("a2a")));
    assert!(values.contains(&json!("label_eval")));
}

#[test]
fn tachi_profile_action_schema_declares_enum_values() {
    let schema = rmcp::schemars::schema_for!(TachiProfileParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("import")));
    assert!(values.contains(&json!("render")));
    assert!(values.contains(&json!("context")));
}

#[test]
fn tachi_skill_action_schema_declares_bundle_and_loadout() {
    let schema = rmcp::schemars::schema_for!(TachiSkillParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("discover")));
    assert!(values.contains(&json!("run")));
    assert!(values.contains(&json!("bundle")));
    assert!(values.contains(&json!("from_pattern")));
    assert!(values.contains(&json!("loadout")));
}

#[test]
fn tachi_task_action_schema_declares_feature_briefing() {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("briefing")));
    assert!(values.contains(&json!("doc_index")));
    assert!(values.contains(&json!("plan")));
    assert!(values.contains(&json!("dispatch")));
    assert!(values.contains(&json!("complete")));
    assert!(values.contains(&json!("recommend")));
    assert!(values.contains(&json!("route_simulate")));
    assert!(values.contains(&json!("proposals")));
    assert!(values.contains(&json!("review_proposal")));
    assert!(values.contains(&json!("apply_proposals")));
    assert!(values.contains(&json!("status")));
    assert!(values.contains(&json!("cancel")));
    assert!(values.contains(&json!("intake")));
    assert!(values.contains(&json!("link_pr")));
    assert!(values.contains(&json!("pr_status")));
    assert!(values.contains(&json!("cycle_plan")));
    assert!(values.contains(&json!("pr_handoff")));
    assert!(values.contains(&json!("release_note")));
    assert!(values.contains(&json!("ux_matrix")));
    assert!(values.contains(&json!("build_references")));
    assert!(values.contains(&json!("close_loop")));
}

#[test]
fn tachi_gh_action_schema_mentions_lifecycle_actions() {
    let schema = rmcp::schemars::schema_for!(crate::tool_params::TachiGhParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];
    let description = action["description"].as_str().expect("action description");

    for expected in ["link_pr", "pr_status", "pr_handoff", "release_note"] {
        assert!(
            description.contains(expected),
            "tachi_gh action schema should mention {expected}: {description}"
        );
    }
}

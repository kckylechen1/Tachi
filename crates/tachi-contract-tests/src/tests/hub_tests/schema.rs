use tachi_params::TachiTaskParams;

#[test]
fn tachi_task_schema_advertises_survivors_not_retired_c1c_actions() {
    let schema = rmcp::handler::server::tool::schema_for_type::<TachiTaskParams>();
    let action_description = schema["properties"]["action"]["description"]
        .as_str()
        .expect("action description");

    assert!(
        action_description.contains("status"),
        "tachi_task.action schema must advertise lifecycle status: {action_description}"
    );
    for retired in tachi_params::TACHI_TASK_RETIRED_ACTIONS {
        assert!(
            !action_description.contains(retired),
            "tachi_task.action schema must not advertise retired C1a/C1b/C1c action {retired}: {action_description}"
        );
    }
}

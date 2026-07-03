use super::*;

#[tokio::test]
async fn distill_trajectory_creates_permanent_snapshot_and_skill() {
    let server = make_server();
    const DISTILLED_MARKDOWN: &str =
        "# 适用场景\n- recurring task\n\n# 核心步骤\n- step\n\n# 踩坑记录\n- none\n\n# 验证标准\n- tests pass\n\n# 适用域标签\n- coding";

    server
        .with_global_store(|store| {
            let mut cap = store
                .hub_get("skill:trajectory-distiller")
                .map_err(|e| e.to_string())?
                .expect("trajectory distiller should exist");
            let mut def: Value =
                serde_json::from_str(&cap.definition).map_err(|e| e.to_string())?;
            def["mock_response"] = json!(DISTILLED_MARKDOWN);
            cap.definition = serde_json::to_string(&def).map_err(|e| e.to_string())?;
            store.hub_register(&cap).map_err(|e| e.to_string())
        })
        .expect("inject mock response");

    let response = server
        .distill_trajectory(Parameters(DistillTrajectoryParams {
            task_description: "Fix a flaky test".to_string(),
            execution_trace: vec![json!({"step":"reproduced"}), json!({"step":"fixed"})],
            final_outcome: json!({"success": true, "score": 0.92}),
            agent_id: "codex".to_string(),
            skill_path: "/skills/coding/flaky-test-fix".to_string(),
            skill_id: Some("skill:flaky-test-fix".to_string()),
            importance: Some(0.9),
            domain: Some("coding".to_string()),
            project: None,
            scope: "global".to_string(),
        }))
        .await
        .expect("distill_trajectory should succeed");
    let response_json: Value = serde_json::from_str(&response).expect("distill response json");

    let snapshot_id = response_json["snapshot_id"]
        .as_str()
        .expect("snapshot id")
        .to_string();
    let snapshot = server
        .with_global_store_read(|store| store.get(&snapshot_id).map_err(|e| e.to_string()))
        .expect("load distilled snapshot")
        .expect("snapshot should exist");
    assert_eq!(snapshot.retention_policy.as_deref(), Some("permanent"));
    assert_eq!(snapshot.text, DISTILLED_MARKDOWN);
    assert!(!snapshot
        .text
        .contains(crate::hub_ops::SIMULATED_SKILL_OUTPUT_MARKER));
    assert!(
        serde_json::from_str::<Value>(&snapshot.text).is_err(),
        "distilled snapshot should remain raw markdown, not a JSON envelope"
    );

    let distilled_cap = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:flaky-test-fix")
                .map_err(|e| e.to_string())
        })
        .expect("load distilled cap")
        .expect("distilled skill should exist");
    let distilled_def: Value =
        serde_json::from_str(&distilled_cap.definition).expect("distilled definition json");
    assert_eq!(distilled_def["retention_policy"], "permanent");
    assert_eq!(distilled_def["content"], json!(DISTILLED_MARKDOWN));
    assert!(!distilled_def["content"]
        .as_str()
        .expect("distilled content")
        .contains(crate::hub_ops::SIMULATED_SKILL_OUTPUT_MARKER));
    assert_eq!(distilled_cap.avg_rating, 0.5);
}

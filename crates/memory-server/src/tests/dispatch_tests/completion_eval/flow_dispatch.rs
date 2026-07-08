use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_links_eval_to_flow_dispatch_card_and_ux_matrix() {
    let (server, _temp_home) = make_server_with_temp_home();
    let flow_id = "flow_20260609T000002Z_complete_link_test";
    let dispatch_id = "20260609T000002Z-custom-complete-link";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "glm_impl",
            "task": "implementation",
        }),
    )
    .expect("mark dispatch");

    let mut complete_params = task_params("complete");
    complete_params.task = Some("Implement dispatch completion linkage".to_string());
    complete_params.agent = Some("glm".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-link-002".to_string());
    complete_params.task_type = Some("fix_request".to_string());
    complete_params.profile = Some("glm_impl".to_string());
    complete_params.risk = Some("medium".to_string());
    complete_params.duration_ms = Some(1200);
    complete_params.skills_used = vec!["skill:superpowers-executing-plans".to_string()];
    complete_params.cost_tokens = Some(123);
    complete_params.quality_score = Some(0.88);
    complete_params.notes = Some("Linked eval back to dispatch card.".to_string());
    complete_params.diff = Some("diff --git a/x b/x\n+y\n".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.flow_id = Some(flow_id.to_string());
    complete_params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    complete_params.evidence_refs = vec!["crates/memory-server/src/complete_ops.rs".to_string()];
    complete_params.tests_run = vec!["cargo test -p memory-server dispatch_tests".to_string()];
    complete_params.scope = Some("project".to_string());
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should succeed");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");
    assert_eq!(
        bundle["pipeline"]["dispatch_completion_link"],
        json!(true),
        "{bundle:#}"
    );

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["completed_dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("eval"));
    assert_eq!(status["state"], json!("dispatch_completed"));
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["completion"]["task_id"], json!("eval-link-002"));
    assert_eq!(card["completion"]["outcome"], json!("success"));
    assert_eq!(
        card["completion"]["verification_present"],
        json!(true),
        "{card:#}"
    );

    let mut ux_params = task_params("ux_matrix");
    ux_params.flow_id = Some(flow_id.to_string());
    ux_params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    let ux_raw = server
        .tachi_task(Parameters(ux_params))
        .await
        .expect("ux_matrix should succeed");
    let ux: Value = serde_json::from_str(&ux_raw).expect("ux JSON");
    assert!(
        ux["matrix"].as_array().is_some_and(|steps| {
            steps.iter().any(|step| {
                step["id"] == json!("complete_eval")
                    && step["status"] == json!("passed")
                    && step["tool"]
                        == json!("tachi_task(action='complete', dispatch_id=..., flow_id=...)")
            })
        }),
        "{ux:#}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_infers_task_agent_and_profile_from_dispatch_card() {
    let (server, _temp_home) = make_server_with_temp_home();
    let flow_id = "flow_20260609T000004Z_complete_defaults_test";
    let dispatch_id = "20260609T000004Z-custom-defaults";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "glm_impl",
            "task": "implementation from card",
        }),
    )
    .expect("mark dispatch");

    let mut complete_params = task_params("complete");
    complete_params.format = Some("full".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-link-004".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.flow_id = Some(flow_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should infer dispatch defaults");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");
    assert_eq!(bundle["recorded"], json!(true), "{bundle:#}");
    assert_eq!(bundle["agent"], json!("custom"), "{bundle:#}");
    assert_eq!(
        bundle["task"],
        json!("implementation from card"),
        "{bundle:#}"
    );
    assert_eq!(bundle["dispatch_id"], json!(dispatch_id), "{bundle:#}");
    assert_eq!(bundle["profile"], json!("glm_impl"), "{bundle:#}");
    assert_eq!(
        bundle["pipeline"]["dispatch_completion_link"]["recorded"],
        json!(true),
        "{bundle:#}"
    );

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["agent"],
        json!("custom"),
        "{status:#}"
    );
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["task"],
        json!("implementation from card"),
        "{status:#}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_surfaces_warning_when_kanban_card_is_missing() {
    let (server, _temp_home) = make_server_with_temp_home();
    let dispatch_id = "20260615T000008Z-kanban-warning";

    let mut complete_params = task_params("complete");
    complete_params.format = Some("full".to_string());
    complete_params.task = Some("Write completion while kanban is stale".to_string());
    complete_params.agent = Some("codex".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-kanban-warning".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should still succeed");

    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");
    let warning = bundle["warning"]
        .as_str()
        .expect("kanban update warning should be surfaced");
    assert!(warning.contains("kanban card missing"), "{warning}");
    assert!(warning.contains(dispatch_id), "{warning}");
    assert!(warning.contains("eval-kanban-warning"), "{warning}");
    assert!(
        warning.contains("Write completion while kanban is stale"),
        "{warning}"
    );
    assert_eq!(
        bundle["pipeline"]["kanban_update"]["status"],
        json!("missing")
    );
    assert_eq!(
        bundle["pipeline"]["kanban_update"]["dispatch_id"],
        json!(dispatch_id)
    );
}

use super::*;

#[tokio::test]
async fn tachi_task_recommend_falls_back_to_builtin_profiles_without_eval_rows() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a low-risk documentation update".to_string());
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed without eval rows");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert!(rec["recommended_profile"].as_str().is_some());
    assert!(
        rec.as_object()
            .is_some_and(|obj| obj.contains_key("recommended_model")),
        "recommend should surface model choice: {rec:#}"
    );
    assert!(rec["recommended_transport"].as_str().is_some());
    assert_eq!(rec["live_eval"]["row_count"], serde_json::json!(0));
    assert!(
        rec["evidence_note"]
            .as_str()
            .is_some_and(|note| note.contains("low_sample_fallback")),
        "expected fallback note: {rec:#}"
    );
    assert!(rec["mbit_card"].is_object());
}

#[tokio::test]
async fn tachi_task_recommend_surfaces_kimi_ux_for_agent_experience_tasks() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some(
        "Run an agent-facing UX 大满贯 test for tachi_arena and summarize tool surface friction"
            .to_string(),
    );
    params.limit = Some(20);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    let candidates = rec["candidates"].as_array().expect("candidates");
    let kimi_ux = candidates
        .iter()
        .find(|candidate| candidate["profile"] == serde_json::json!("kimi_ux"))
        .expect("kimi_ux candidate");
    assert!(
        kimi_ux["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| {
                reason
                    .as_str()
                    .is_some_and(|s| s.contains("role_matches_agent_facing_ux"))
            })),
        "kimi_ux should explain UX routing fit: {kimi_ux:#}"
    );
    assert!(
        candidates
            .iter()
            .take(3)
            .any(|candidate| candidate["profile"] == serde_json::json!("kimi_ux")),
        "kimi_ux should be near the top for agent-facing UX tasks: {rec:#}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_routes_low_risk_review_to_fast_checker_without_live_rows() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("review low-risk docs wording".to_string());
    params.risk = Some("low".to_string());
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(rec["task_type"], json!("review_request"));
    assert_eq!(rec["risk"], json!("low"));
    assert_eq!(rec["recommended_profile"], json!("codex_53_fast"));
    assert!(rec["required_profiles"]
        .as_array()
        .expect("required profiles")
        .contains(&json!("codex_53_fast")));
    assert!(rec["blocked_profiles"]
        .as_array()
        .expect("blocked profiles")
        .is_empty());
}

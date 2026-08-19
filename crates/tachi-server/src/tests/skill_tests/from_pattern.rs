use super::*;

fn skill_params(action: &str) -> TachiSkillParams {
    TachiSkillParams {
        action: action.to_string(),
        query: Some("continuity-first".to_string()),
        cap_type: None,
        enabled_only: None,
        limit: Some(10),
        skill_id: None,
        args: Some(json!({
            "skill_id": "skill:pattern-continuity-test",
            "name": "continuity-test-pattern",
            "description": "Candidate from continuity pattern"
        })),
    }
}

#[tokio::test]
async fn tachi_skill_facade_rejects_retired_actions() {
    let server = make_server();

    let error = server
        .tachi_skill(Parameters(skill_params("from_pattern")))
        .await
        .expect_err("tachi_skill must reject retired from_pattern action");
    assert!(error.contains("Invalid action 'from_pattern'. Use 'discover' or 'run'."));

    let bundle_error = server
        .tachi_skill(Parameters(skill_params("bundle")))
        .await
        .expect_err("tachi_skill must reject retired bundle action");
    assert!(bundle_error.contains("Invalid action 'bundle'. Use 'discover' or 'run'."));
}

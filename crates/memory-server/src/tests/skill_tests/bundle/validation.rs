use super::*;

#[tokio::test]
async fn tachi_skill_bundle_requires_query() {
    let server = make_server();

    let err = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "bundle".to_string(),
            query: None,
            cap_type: None,
            enabled_only: None,
            limit: None,
            skill_id: None,
            args: None,
            profile: None,
            host: Some("codex".to_string()),
            skill_limit: None,
            capability_limit: None,
            include_section: None,
        }))
        .await
        .expect_err("missing bundle query should fail");

    assert!(
        err.contains("query is required when action='bundle'"),
        "{err}"
    );
}

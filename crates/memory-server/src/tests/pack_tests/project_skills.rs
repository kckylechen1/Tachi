use super::*;

#[tokio::test(flavor = "current_thread")]
async fn test_pack_project_with_skill_files() {
    let _temp_home = TempHomeGuard::new();
    let server = make_server();

    // Create a temp directory with SKILL.md files to act as a pack source
    let pack_dir = std::env::temp_dir().join(format!("tachi-test-pack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&pack_dir).unwrap();

    // Root SKILL.md
    std::fs::write(
        pack_dir.join("SKILL.md"),
        "# Root Skill\nThis is the root skill.",
    )
    .unwrap();

    // Subdirectory skill
    let sub_skill = pack_dir.join("code-review");
    std::fs::create_dir_all(&sub_skill).unwrap();
    std::fs::write(
        sub_skill.join("SKILL.md"),
        "# Code Review\nReview code carefully.",
    )
    .unwrap();

    // Register the pack with local_path
    server
        .pack_register(Parameters(PackRegisterParams {
            id: "test/skillpack".to_string(),
            name: Some("Skill Pack".to_string()),
            source: Some("local".to_string()),
            version: Some("1.0.0".to_string()),
            description: None,
            local_path: Some(pack_dir.display().to_string()),
            metadata: None,
        }))
        .await
        .expect("register skill pack");

    // Project to generic agent.
    let result = server
        .pack_project(Parameters(PackProjectParams {
            pack_id: "test/skillpack".to_string(),
            agents: vec!["generic".to_string()],
        }))
        .await
        .expect("pack_project should succeed");

    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["pack_id"], "test/skillpack");
    let projections = json["projections"].as_array().unwrap();
    assert_eq!(projections.len(), 1);
    assert_eq!(projections[0]["agent"], "generic");
    assert_eq!(projections[0]["status"], "projected");

    let skill_count = projections[0]["skill_count"].as_u64().unwrap();
    assert_eq!(
        skill_count, 2,
        "expected exactly 2 skills to be projected, but got {skill_count}"
    );

    // Verify projection_list records it
    let result = server
        .projection_list(Parameters(ProjectionListParams {
            agent: None,
            pack_id: Some("test/skillpack".to_string()),
        }))
        .await
        .expect("projection_list should succeed");
    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["count"], 1);

    // Clean up
    let projected_path = projections[0]["path"].as_str().unwrap_or("");
    if !projected_path.is_empty() {
        let _ = std::fs::remove_dir_all(projected_path);
    }
    let _ = std::fs::remove_dir_all(&pack_dir);
}

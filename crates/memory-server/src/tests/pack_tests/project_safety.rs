use super::*;

#[tokio::test(flavor = "current_thread")]
async fn test_pack_project_sanitizes_pack_target_directory() {
    let _temp_home = TempHomeGuard::new();
    let server = make_server();
    let pack_dir =
        std::env::temp_dir().join(format!("tachi-test-pack-sanitize-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&pack_dir).unwrap();
    std::fs::write(pack_dir.join("SKILL.md"), "# Root Skill").unwrap();

    server
        .pack_register(Parameters(PackRegisterParams {
            id: "test/..".to_string(),
            name: Some("Sanitized Pack".to_string()),
            source: Some("local".to_string()),
            version: Some("1.0.0".to_string()),
            description: None,
            local_path: Some(pack_dir.display().to_string()),
            metadata: None,
        }))
        .await
        .expect("register sanitized pack");

    let result = server
        .pack_project(Parameters(PackProjectParams {
            pack_id: "test/..".to_string(),
            agents: vec!["generic".to_string()],
        }))
        .await
        .expect("pack_project should succeed");
    let json: Value = serde_json::from_str(&result).unwrap();
    let projections = json["projections"].as_array().unwrap();
    let projected_path = projections[0]["path"].as_str().unwrap_or("");

    let projected_name = std::path::Path::new(projected_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    assert_eq!(projected_name, "unnamed");

    if !projected_path.is_empty() {
        let _ = std::fs::remove_dir_all(projected_path);
    }
    let _ = std::fs::remove_dir_all(&pack_dir);
}

#[tokio::test]
async fn test_pack_project_invalid_agent() {
    let server = make_server();

    // Register a pack with a dummy path
    let pack_dir = std::env::temp_dir().join(format!("tachi-test-empty-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&pack_dir).unwrap();
    std::fs::write(pack_dir.join("SKILL.md"), "# Test").unwrap();

    server
        .pack_register(Parameters(PackRegisterParams {
            id: "test/invalidagent".to_string(),
            name: None,
            source: None,
            version: None,
            description: None,
            local_path: Some(pack_dir.display().to_string()),
            metadata: None,
        }))
        .await
        .expect("register");

    // Project with invalid agent name
    let err = server
        .pack_project(Parameters(PackProjectParams {
            pack_id: "test/invalidagent".to_string(),
            agents: vec!["not_an_agent".to_string()],
        }))
        .await;
    assert!(err.is_err(), "should fail with invalid agent kind");

    let _ = std::fs::remove_dir_all(&pack_dir);
}

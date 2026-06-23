use super::*;

#[tokio::test(flavor = "current_thread")]
async fn test_projection_list_filter_by_agent() {
    let _temp_home = TempHomeGuard::new();
    let server = make_server();

    // Create pack source
    let pack_dir =
        std::env::temp_dir().join(format!("tachi-test-proj-filter-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&pack_dir).unwrap();
    std::fs::write(pack_dir.join("SKILL.md"), "# Filter Test").unwrap();

    server
        .pack_register(Parameters(PackRegisterParams {
            id: "test/filterpack".to_string(),
            name: None,
            source: None,
            version: None,
            description: None,
            local_path: Some(pack_dir.display().to_string()),
            metadata: None,
        }))
        .await
        .expect("register");

    // Project to generic.
    server
        .pack_project(Parameters(PackProjectParams {
            pack_id: "test/filterpack".to_string(),
            agents: vec!["generic".to_string()],
        }))
        .await
        .expect("project");

    // Filter by agent=generic should return 1
    let result = server
        .projection_list(Parameters(ProjectionListParams {
            agent: Some("generic".to_string()),
            pack_id: None,
        }))
        .await
        .expect("projection_list by agent");
    let json: Value = serde_json::from_str(&result).unwrap();
    assert!(json["count"].as_u64().unwrap() >= 1);

    // Filter by agent=cursor should return 0 (we didn't project there)
    let result = server
        .projection_list(Parameters(ProjectionListParams {
            agent: Some("cursor".to_string()),
            pack_id: None,
        }))
        .await
        .expect("projection_list by cursor");
    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["count"], 0);

    // Clean up projected files
    let result = server
        .projection_list(Parameters(ProjectionListParams {
            agent: Some("generic".to_string()),
            pack_id: Some("test/filterpack".to_string()),
        }))
        .await
        .expect("get projection path");
    let json: Value = serde_json::from_str(&result).unwrap();
    if let Some(projs) = json["projections"].as_array() {
        for p in projs {
            if let Some(path) = p["projected_path"].as_str() {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&pack_dir);
}

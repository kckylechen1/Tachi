use super::*;

#[tokio::test]
async fn test_pack_register_and_get() {
    let server = make_server();

    // Register a pack
    let result = server
        .pack_register(Parameters(PackRegisterParams {
            id: "test/mypack".to_string(),
            name: Some("My Test Pack".to_string()),
            source: Some("github:test/mypack".to_string()),
            version: Some("1.0.0".to_string()),
            description: Some("A test pack".to_string()),
            local_path: None,
            metadata: Some(json!({"author": "tester"})),
        }))
        .await
        .expect("pack_register should succeed");

    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["status"], "registered");
    assert_eq!(json["pack_id"], "test/mypack");

    // Get the pack
    let result = server
        .pack_get(Parameters(PackGetParams {
            id: "test/mypack".to_string(),
        }))
        .await
        .expect("pack_get should succeed");

    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["id"], "test/mypack");
    assert_eq!(json["name"], "My Test Pack");
    assert_eq!(json["version"], "1.0.0");
    assert_eq!(json["source"], "github:test/mypack");
    assert!(json["enabled"].as_bool().unwrap());
}

#[tokio::test]
async fn test_pack_list_empty_and_filled() {
    let server = make_server();

    // List should be empty initially
    let result = server
        .pack_list(Parameters(PackListParams { enabled_only: None }))
        .await
        .expect("pack_list should succeed");
    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["count"], 0);

    // Register two packs
    server
        .pack_register(Parameters(PackRegisterParams {
            id: "pack-a".to_string(),
            name: Some("Pack A".to_string()),
            source: None,
            version: None,
            description: None,
            local_path: None,
            metadata: None,
        }))
        .await
        .expect("register pack-a");

    server
        .pack_register(Parameters(PackRegisterParams {
            id: "pack-b".to_string(),
            name: Some("Pack B".to_string()),
            source: None,
            version: None,
            description: None,
            local_path: None,
            metadata: None,
        }))
        .await
        .expect("register pack-b");

    // List should have 2
    let result = server
        .pack_list(Parameters(PackListParams { enabled_only: None }))
        .await
        .expect("pack_list should succeed");
    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["count"], 2);
}

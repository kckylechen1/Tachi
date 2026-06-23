use super::*;

// ─── Pack System Tests ────────────────────────────────────────────────────────

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

#[tokio::test]
async fn test_pack_register_uses_tachi_pack_manifest_metadata() {
    let server = make_server();
    let pack_dir =
        std::env::temp_dir().join(format!("tachi-test-manifest-pack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(pack_dir.join("skills").join("review")).unwrap();
    std::fs::write(
        pack_dir.join("skills").join("review").join("SKILL.md"),
        "# Review\nManifest-backed skill",
    )
    .unwrap();
    std::fs::write(
        pack_dir.join("tachi-pack.json"),
        json!({
            "schema_version": "1",
            "pack": {
                "name": "Manifest Pack",
                "version": "2.3.4",
                "description": "Pack metadata should come from manifest",
                "source": "github:test/manifest-pack"
            },
            "services": ["memory"]
        })
        .to_string(),
    )
    .unwrap();

    let result = server
        .pack_register(Parameters(PackRegisterParams {
            id: "test/manifest-pack".to_string(),
            name: None,
            source: None,
            version: None,
            description: None,
            local_path: Some(pack_dir.display().to_string()),
            metadata: None,
        }))
        .await
        .expect("pack_register with manifest should succeed");

    let registered: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(registered["skill_count"], 1);
    assert!(registered["manifest_path"]
        .as_str()
        .unwrap_or("")
        .ends_with("tachi-pack.json"));

    let result = server
        .pack_get(Parameters(PackGetParams {
            id: "test/manifest-pack".to_string(),
        }))
        .await
        .expect("pack_get should succeed");
    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["name"], "Manifest Pack");
    assert_eq!(json["version"], "2.3.4");
    assert_eq!(
        json["description"],
        "Pack metadata should come from manifest"
    );
    assert_eq!(json["source"], "github:test/manifest-pack");

    let metadata: Value =
        serde_json::from_str(json["metadata"].as_str().unwrap_or("{}")).expect("metadata json");
    assert_eq!(metadata["pack_manifest"]["services"][0], "memory");
    assert_eq!(metadata["projection"]["discovered"]["skill_count"], 1);

    let _ = std::fs::remove_dir_all(&pack_dir);
}

#[tokio::test]
async fn test_pack_remove() {
    let server = make_server();

    // Register then remove
    server
        .pack_register(Parameters(PackRegisterParams {
            id: "removable".to_string(),
            name: Some("Removable Pack".to_string()),
            source: None,
            version: None,
            description: None,
            local_path: None,
            metadata: None,
        }))
        .await
        .expect("register removable");

    let result = server
        .pack_remove(Parameters(PackRemoveParams {
            id: "removable".to_string(),
            clean_files: Some(false),
        }))
        .await
        .expect("pack_remove should succeed");

    let json: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["status"], "removed");

    // pack_get should fail
    let err = server
        .pack_get(Parameters(PackGetParams {
            id: "removable".to_string(),
        }))
        .await;
    assert!(err.is_err(), "pack_get after remove should fail");
}

#[tokio::test]
async fn test_pack_get_not_found() {
    let server = make_server();

    let err = server
        .pack_get(Parameters(PackGetParams {
            id: "nonexistent/pack".to_string(),
        }))
        .await;
    assert!(err.is_err(), "pack_get for nonexistent should fail");
    assert!(
        err.unwrap_err().contains("not found"),
        "error should mention not found"
    );
}

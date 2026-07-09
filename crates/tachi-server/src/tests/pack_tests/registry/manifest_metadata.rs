use super::*;

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

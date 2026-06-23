use super::*;

#[tokio::test(flavor = "current_thread")]
async fn test_pack_project_openclaw_writes_projection_manifest() {
    let _temp_home = TempHomeGuard::new();
    let server = make_server();

    let pack_dir =
        std::env::temp_dir().join(format!("tachi-test-openclaw-pack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(pack_dir.join("skills").join("brainstorm")).unwrap();
    std::fs::create_dir_all(pack_dir.join("workflows")).unwrap();
    std::fs::create_dir_all(pack_dir.join("commands")).unwrap();
    std::fs::create_dir_all(pack_dir.join("hooks")).unwrap();
    std::fs::create_dir_all(pack_dir.join("openclaw")).unwrap();
    std::fs::create_dir_all(pack_dir.join("runtime")).unwrap();

    std::fs::write(
        pack_dir.join("skills").join("brainstorm").join("SKILL.md"),
        "# Brainstorm\nAsk clarifying questions first.",
    )
    .unwrap();
    std::fs::write(
        pack_dir.join("workflows").join("intake.md"),
        "# Intake Workflow\nCollect choices before execution.",
    )
    .unwrap();
    std::fs::write(
        pack_dir.join("commands").join("plan.md"),
        "/plan\nProduce a plan.",
    )
    .unwrap();
    std::fs::write(
        pack_dir.join("hooks").join("hooks.json"),
        r#"{"SessionStart":{"command":"echo hello"}}"#,
    )
    .unwrap();
    std::fs::write(
        pack_dir.join("openclaw").join("plugin.json"),
        r#"{"plugin":"pack-openclaw"}"#,
    )
    .unwrap();
    std::fs::write(
        pack_dir.join("runtime").join("runner.js"),
        "export function run() { return 'ok'; }",
    )
    .unwrap();
    std::fs::write(
        pack_dir.join("tachi-pack.json"),
        json!({
            "schema_version": "1",
            "pack": {
                "name": "OpenClaw Pack",
                "version": "1.0.0"
            },
            "services": ["memory"],
            "workflows": [
                { "path": "workflows/intake.md", "target": "intake.md", "kind": "workflow" }
            ],
            "runtime": [
                { "path": "runtime/runner.js", "target": "runner.js", "kind": "node" }
            ],
            "overlays": {
                "openclaw": {
                    "files": [
                        { "path": "openclaw/plugin.json", "target": "plugin.json", "kind": "manifest" }
                    ],
                    "manifest": {
                        "hooks": {
                            "before_agent_start": {
                                "type": "skill-injection"
                            }
                        }
                    }
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    server
        .pack_register(Parameters(PackRegisterParams {
            id: "test/openclaw-pack".to_string(),
            name: None,
            source: Some("local".to_string()),
            version: None,
            description: None,
            local_path: Some(pack_dir.display().to_string()),
            metadata: None,
        }))
        .await
        .expect("register openclaw pack");

    let result = server
        .pack_project(Parameters(PackProjectParams {
            pack_id: "test/openclaw-pack".to_string(),
            agents: vec!["openclaw".to_string()],
        }))
        .await
        .expect("pack_project openclaw should succeed");
    let json: Value = serde_json::from_str(&result).unwrap();
    let projections = json["projections"].as_array().unwrap();
    assert_eq!(projections.len(), 1);
    assert_eq!(projections[0]["agent"], "openclaw");
    assert_eq!(projections[0]["status"], "projected");
    assert_eq!(projections[0]["skill_count"], 1);
    assert_eq!(projections[0]["workflow_count"], 1);
    assert!(projections[0]["overlay_count"].as_u64().unwrap_or(0) >= 3);
    assert_eq!(projections[0]["runtime_count"], 1);

    let projected_path = projections[0]["path"].as_str().unwrap_or("");
    let projection_manifest = std::path::Path::new(projected_path).join("tachi-projection.json");
    assert!(
        projection_manifest.exists(),
        "projection manifest should exist"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&projection_manifest)
            .expect("projection manifest metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "projection manifest should be owner-only");
    }

    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(&projection_manifest).expect("read projection manifest"),
    )
    .expect("projection manifest json");
    assert_eq!(manifest["agent"], "openclaw");
    assert_eq!(manifest["counts"]["skills"], 1);
    assert_eq!(manifest["counts"]["workflows"], 1);
    assert_eq!(manifest["counts"]["runtime"], 1);
    assert_eq!(
        manifest["overlay_manifest"]["hooks"]["before_agent_start"]["type"],
        "skill-injection"
    );

    assert!(
        std::path::Path::new(projected_path)
            .join("_overlay")
            .join("openclaw")
            .join("plugin.json")
            .exists(),
        "openclaw plugin overlay should be copied"
    );
    let overlay_manifest = std::path::Path::new(projected_path)
        .join("_overlay")
        .join("openclaw")
        .join("overlay-manifest.json");
    assert!(overlay_manifest.exists(), "overlay manifest should exist");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&overlay_manifest)
            .expect("overlay manifest metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "overlay manifest should be owner-only");
    }

    let _ = std::fs::remove_dir_all(&pack_dir);
}

use super::{make_server, TempHomeGuard};
use crate::tool_params::{
    PackGetParams, PackListParams, PackProjectParams, PackRegisterParams, PackRemoveParams,
    ProjectionListParams,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

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

use super::*;

// ─── Export Skills Tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn openclaw_export_preserves_document_text_and_legacy_prompt_precedence() {
    let server = make_server();
    let export_dir = tempfile::tempdir().expect("export directory");
    let cases = [
        (
            "skill:export-document-content",
            json!({"execution": "document", "content": "# Workflow\nKeep {{input}} literal."}),
            "# Workflow\nKeep {{input}} literal.",
        ),
        (
            "skill:export-document-precedence",
            json!({"execution": "document", "content": "Workflow body", "prompt": "Old wrapper", "template": "Old template"}),
            "Workflow body",
        ),
        (
            "skill:export-document-prompt",
            json!({"execution": "document", "prompt": "Prompt document"}),
            "Prompt document",
        ),
        (
            "skill:export-document-template",
            json!({"execution": "document", "template": "Template document"}),
            "Template document",
        ),
        (
            "skill:export-legacy-prompt",
            json!({"content": "Legacy content", "prompt": "Legacy prompt", "template": "Legacy template"}),
            "Legacy prompt",
        ),
        (
            "skill:export-legacy-template",
            json!({"content": "Legacy content", "template": "Legacy template"}),
            "Legacy template",
        ),
    ];
    for (id, definition, _) in &cases {
        let mut cap = crate::tests::make_skill_capability(id, id, "export fixture", "listed");
        cap.definition = definition.to_string();
        server
            .with_global_store(|store| store.hub_register(&cap).map_err(|error| error.to_string()))
            .expect("seed export skill");
    }
    let response = server
        .hub_export_skills(Parameters(ExportSkillsParams {
            agent: "openclaw".to_string(),
            skill_ids: Some(cases.iter().map(|(id, _, _)| id.to_string()).collect()),
            visibility: "all".to_string(),
            output_dir: Some(export_dir.path().display().to_string()),
            clean: false,
        }))
        .await
        .expect("export document and legacy assets");
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["exported"], json!(cases.len()));
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(export_dir.path().join("tachi-skills.json")).unwrap(),
    )
    .unwrap();
    let entries = manifest["skills"].as_array().expect("exported skills");
    for (id, _, expected) in &cases {
        let entry = entries.iter().find(|entry| entry["id"] == *id).unwrap();
        assert_eq!(
            entry["prompt"],
            json!(expected),
            "exported content for {id}"
        );
    }
}

#[tokio::test]
async fn hub_export_skills_returns_empty_when_no_skills() {
    let server = make_server();

    let result = server
        .hub_export_skills(Parameters(ExportSkillsParams {
            agent: "generic".to_string(),
            skill_ids: Some(vec!["skill:does-not-exist".to_string()]),
            visibility: "all".to_string(),
            output_dir: Some(
                std::env::temp_dir()
                    .join(format!("tachi-export-{}", uuid::Uuid::new_v4()))
                    .display()
                    .to_string(),
            ),
            clean: false,
        }))
        .await
        .expect("hub_export_skills should succeed");
    let json: serde_json::Value = serde_json::from_str(&result).expect("should be JSON");
    assert_eq!(json["exported"], json!(0));
}

#[tokio::test]
async fn hub_export_skills_rejects_unknown_agent() {
    let server = make_server();

    let result = server
        .hub_export_skills(Parameters(ExportSkillsParams {
            agent: "unknown-agent".to_string(),
            skill_ids: Some(vec!["skill:does-not-exist".to_string()]),
            visibility: "all".to_string(),
            output_dir: None,
            clean: false,
        }))
        .await;
    assert!(result.is_ok(), "should not crash when no skills match");
}

#[tokio::test]
async fn hub_export_skills_generic_writes_files() {
    let server = make_server();
    let export_dir =
        crate::utils::test_fixture_path(format!("tachi-export-{}", uuid::Uuid::new_v4()));

    // Register a skill
    server
        .hub_register(Parameters(HubRegisterParams {
            id: "skill:test-export".to_string(),
            cap_type: "skill".to_string(),
            name: "test-export".to_string(),
            description: "export test skill".to_string(),
            definition: json!({
                "prompt": "You are a helpful assistant that reviews code.",
                "content": "# Test Export Skill\n\nReview code carefully.",
                "inputSchema": {"type": "object"},
            })
            .to_string(),
            version: 1,
            scope: "global".to_string(),
        }))
        .await
        .expect("register skill for export");

    let result = server
        .hub_export_skills(Parameters(ExportSkillsParams {
            agent: "generic".to_string(),
            skill_ids: None,
            visibility: "all".to_string(),
            output_dir: Some(export_dir.display().to_string()),
            clean: false,
        }))
        .await
        .expect("hub_export_skills generic should succeed");
    let json: serde_json::Value = serde_json::from_str(&result).expect("should be JSON");
    assert!(
        json["exported"].as_u64().unwrap_or(0) >= 1,
        "expected at least 1 skill exported, got: {json}"
    );

    // Verify file was created
    let skill_file = export_dir.join("test-export.md");
    assert!(
        skill_file.exists(),
        "expected skill file at {}",
        skill_file.display()
    );

    let _ = std::fs::remove_dir_all(&export_dir);
}

#[tokio::test]
async fn hub_export_skills_sanitizes_skill_file_names() {
    let server = make_server();
    let export_dir =
        crate::utils::test_fixture_path(format!("tachi-export-sanitize-{}", uuid::Uuid::new_v4()));

    server
        .hub_register(Parameters(HubRegisterParams {
            id: "skill:..".to_string(),
            cap_type: "skill".to_string(),
            name: "dot-skill".to_string(),
            description: "sanitized export skill".to_string(),
            definition: json!({
                "prompt": "Export safely.",
                "content": "# Sanitized Skill",
                "inputSchema": {"type": "object"},
            })
            .to_string(),
            version: 1,
            scope: "global".to_string(),
        }))
        .await
        .expect("register sanitized skill");

    let result = server
        .hub_export_skills(Parameters(ExportSkillsParams {
            agent: "generic".to_string(),
            skill_ids: Some(vec!["skill:..".to_string()]),
            visibility: "all".to_string(),
            output_dir: Some(export_dir.display().to_string()),
            clean: false,
        }))
        .await
        .expect("hub_export_skills generic should succeed");
    let json: serde_json::Value = serde_json::from_str(&result).expect("should be JSON");

    assert!(export_dir.join("unnamed.md").exists());
    assert_eq!(json["skills"][0]["name"], json!("unnamed"));
    assert_eq!(
        json["skills"][0]["file"],
        json!(export_dir.join("unnamed.md"))
    );

    let _ = std::fs::remove_dir_all(&export_dir);
}

#[tokio::test]
async fn hub_export_omits_retired_trajectory_distiller_from_historical_stores() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("retired-trajectory-export");
    let global = crate::tests::make_skill_capability(
        crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID,
        "trajectory-distiller",
        "Historical global trajectory writer",
        "listed",
    );
    let mut project = global.clone();
    project.description = "Historical project trajectory writer".to_string();
    server
        .with_global_store(|store| {
            store
                .hub_register(&global)
                .map_err(|error| error.to_string())
        })
        .expect("inject historical global export row");
    server
        .with_project_store(|store| {
            store
                .hub_register(&project)
                .map_err(|error| error.to_string())
        })
        .expect("inject historical project export row");
    let export_dir =
        crate::utils::test_fixture_path(format!("tachi-export-retired-{}", uuid::Uuid::new_v4()));

    let result = server
        .hub_export_skills(Parameters(ExportSkillsParams {
            agent: "generic".to_string(),
            skill_ids: Some(vec![
                crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID.to_string()
            ]),
            visibility: "all".to_string(),
            output_dir: Some(export_dir.display().to_string()),
            clean: false,
        }))
        .await
        .expect("retired-only export should return an empty result");
    let json: Value = serde_json::from_str(&result).expect("export JSON");
    assert_eq!(json["exported"], json!(0));
    assert!(!export_dir.join("trajectory-distiller.md").exists());

    let _ = std::fs::remove_dir_all(&export_dir);
}

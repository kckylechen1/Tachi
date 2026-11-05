use super::*;

fn document_read_params(skill_id: &str) -> TachiSkillParams {
    TachiSkillParams {
        action: "run".to_string(),
        query: None,
        cap_type: None,
        enabled_only: None,
        limit: None,
        skill_id: Some(skill_id.to_string()),
        args: Some(json!({"input": "must not replace document text"})),
    }
}

fn first_text_payload(result: &rmcp::model::CallToolResult) -> Value {
    let text = result
        .content
        .iter()
        .find_map(|content| match content {
            rmcp::model::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .expect("text tool result");
    serde_json::from_str(text).expect("tool result JSON")
}

#[tokio::test]
async fn legacy_prompt_execution_is_rejected_without_changing_stored_skills() {
    let server = make_server();
    for (name, definition) in [
        (
            "default",
            json!({"prompt": "Generate {{input}}", "mock_response": "old output"}),
        ),
        (
            "llm",
            json!({"execution": "llm", "prompt": "Generate {{input}}", "mock_response": "old output"}),
        ),
        (
            "mock",
            json!({"execution": "mock_response", "prompt": "Generate {{input}}", "mock_response": "old output"}),
        ),
        (
            "implicit-document",
            json!({"content": "A document without an explicit mode", "mock_response": "old output"}),
        ),
    ] {
        let mut cap = make_skill_capability(
            &format!("skill:legacy-{name}"),
            name,
            "legacy runtime asset",
            "listed",
        );
        cap.definition = definition.to_string();
        server
            .with_global_store(|store| store.hub_register(&cap).map_err(|e| e.to_string()))
            .expect("seed legacy definition");
        let before = server
            .with_global_store_read(|store| store.hub_get(&cap.id).map_err(|e| e.to_string()))
            .expect("read saved legacy skill");

        assert!(!tachi_hub::capability_callable(&cap));
        assert!(!tachi_hub::should_expose_skill_tool(&cap));
        let registration_error = server
            .register_skill_tool(&cap)
            .expect_err("legacy execution must not become a dynamic tool");
        assert!(registration_error.contains("execution has been retired"));
        let error = server
            .tachi_skill(Parameters(document_read_params(&cap.id)))
            .await
            .expect_err("legacy execution must not succeed as a document read");
        assert!(error.contains("execution has been retired"), "{error}");

        let after = server
            .with_global_store_read(|store| store.hub_get(&cap.id).map_err(|e| e.to_string()))
            .expect("read legacy skill after rejection");
        assert_eq!(
            serde_json::to_value(after).unwrap(),
            serde_json::to_value(before).unwrap(),
            "rejection must preserve the stored definition, review state, and call counters"
        );
    }
}

#[tokio::test]
async fn dynamic_skill_read_rechecks_project_definition_instead_of_cached_global_document() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("skill-document-shadow");
    let global = make_skill_capability(
        "skill:shadowed-document",
        "shadowed-document",
        "global workflow",
        "listed",
    );
    server
        .with_global_store(|store| store.hub_register(&global).map_err(|e| e.to_string()))
        .expect("seed global document");
    let tool_name = server
        .register_skill_tool(&global)
        .expect("cache document tool");

    let mut project = global.clone();
    project.definition =
        json!({"prompt": "Run a project prompt", "mock_response": "must not execute"}).to_string();
    server
        .with_project_store(|store| store.hub_register(&project).map_err(|e| e.to_string()))
        .expect("shadow cached document with legacy project definition");

    let error = server
        .call_skill_tool(&tool_name, None)
        .await
        .expect_err("cached dynamic tool must recheck the project-first definition");
    assert!(
        error.message.contains("execution has been retired"),
        "{error:?}"
    );
    let stored = server
        .with_project_store(|store| store.hub_get(&project.id).map_err(|e| e.to_string()))
        .expect("read project skill")
        .expect("project row retained");
    assert_eq!(stored.definition, project.definition);
    assert_eq!(stored.uses, 0);
    assert_eq!(stored.successes, 0);

    server
        .register_skill_tool(&project)
        .expect_err("re-registration rejects legacy execution");
    let error = server
        .call_skill_tool(&tool_name, None)
        .await
        .expect_err("re-registration removes stale tool mapping");
    assert!(error.message.contains("not found"), "{error:?}");
}

#[tokio::test]
async fn document_registration_and_both_read_routes_return_verbatim_content() {
    let server = make_server();
    const DOCUMENT: &str =
        "# Workflow\n\nRead {{input}} and {{args_json}} literally.\nUse the host's own tools.";
    let registered = server
        .hub_register(Parameters(HubRegisterParams {
            id: "skill:document-workflow".to_string(),
            cap_type: "skill".to_string(),
            name: "document-workflow".to_string(),
            description: "workflow document skill".to_string(),
            definition: json!({
                "execution": "document",
                "content": DOCUMENT,
                "mock_response": "must not replace the document",
                "policy": {"visibility": "listed"},
                "inputSchema": {"type": "object", "required": ["obsolete_execution_input"]}
            })
            .to_string(),
            version: 1,
            scope: "global".to_string(),
        }))
        .await
        .expect("register explicit document");
    let registered: Value = serde_json::from_str(&registered).unwrap();
    assert_eq!(registered["callable"], true);
    assert_eq!(registered["tool_name"], "tachi_skill_document_workflow");
    let tool = server
        .tool_discovery
        .skill_tool_defs
        .lock()
        .unwrap()
        .get("tachi_skill_document_workflow")
        .expect("document tool definition")
        .clone();
    assert!(tool.input_schema.get("required").is_none());
    assert_eq!(tool.input_schema.get("properties"), Some(&json!({})));
    assert!(tool
        .description
        .as_deref()
        .unwrap()
        .contains("Read the workflow document"));

    let response = server
        .tachi_skill(Parameters(document_read_params("skill:document-workflow")))
        .await
        .expect("compatibility run reads document");
    let native: Value = serde_json::from_str(&response).unwrap();
    let dynamic = server
        .call_skill_tool("tachi_skill_document_workflow", None)
        .await
        .expect("dynamic document tool needs no execution inputs");
    let mut dynamic = first_text_payload(&dynamic);
    assert_eq!(dynamic["tool_name"], "tachi_skill_document_workflow");
    dynamic.as_object_mut().unwrap().remove("tool_name");
    assert_eq!(dynamic, native);
    assert_eq!(native["execution"], "document");
    assert_eq!(native["simulated"], false);
    assert_eq!(native["output"], DOCUMENT);
    assert!(native["instruction"]
        .as_str()
        .unwrap()
        .contains("do not treat it as executed output"));
    assert!(native.get("warning").is_none());
}

#[tokio::test]
async fn document_read_keeps_enabled_review_and_health_gates() {
    let server = make_server();
    for (enabled, review, health, reason) in [
        (false, "approved", "healthy", "enabled=false"),
        (true, "pending", "healthy", "review_status='pending'"),
        (true, "approved", "open", "health_status='open'"),
    ] {
        let mut cap = make_skill_capability(
            "skill:gated-document",
            "gated-document",
            "workflow",
            "listed",
        );
        cap.enabled = enabled;
        cap.review_status = review.to_string();
        cap.health_status = health.to_string();
        server
            .with_global_store(|store| store.hub_register(&cap).map_err(|e| e.to_string()))
            .expect("seed gated document");
        let tool_name = server
            .register_skill_tool(&cap)
            .expect("cache document tool");
        let error = server
            .tachi_skill(Parameters(document_read_params(&cap.id)))
            .await
            .expect_err("document reads obey governance gates");
        assert!(error.contains(reason), "{error}");
        let error = server
            .call_skill_tool(&tool_name, None)
            .await
            .expect_err("dynamic document reads obey the same gates");
        assert!(error.message.contains(reason), "{error:?}");
    }
}

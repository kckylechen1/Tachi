use super::{call_tool_on_server, make_server};

#[tokio::test]
async fn ordinary_profiles_can_discover_connections_but_cannot_read_secrets_or_execute() {
    for profile in [
        tachi_hub::ToolProfile::standard(),
        tachi_hub::ToolProfile::delegate(),
    ] {
        let server = make_server();
        server.set_tool_profile(Some(profile));
        let tools = server.tachi_tools().await.unwrap();
        assert!(tools.contains("`vault_connections`"));
        for raw in [
            "`vault_get`",
            "`vault_list`",
            "`vault_lease_api_key`",
            "`vault_unlock`",
        ] {
            assert!(!tools.contains(raw));
        }
        let listed = call_tool_on_server(
            server.clone(),
            "vault_connections",
            Some(
                serde_json::json!({"action":"list"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
        assert_ne!(listed.is_error, Some(true));
        let text = listed
            .content
            .first()
            .and_then(|content| content.as_text())
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&text.text).unwrap();
        assert_eq!(value["execution"], "not_started");
        assert_eq!(value["connections"], serde_json::json!([]));
        for arguments in [
            serde_json::json!({"action":"run","name":"host"}),
            serde_json::json!({"action":"set","name":"host"}),
            serde_json::json!({"action":"list","agent_id":"another-agent"}),
            serde_json::json!({"action":"list","project":"another-project"}),
        ] {
            let denied = call_tool_on_server(
                server.clone(),
                "vault_connections",
                Some(arguments.as_object().unwrap().clone()),
            )
            .await;
            match denied {
                Ok(result) => assert_eq!(result.is_error, Some(true)),
                Err(error) => {
                    assert!(arguments.get("project").is_some());
                    assert_eq!(error.code.0, -32602);
                    assert!(error.message.contains("not bound to a project"));
                }
            }
        }
    }
}

#[tokio::test]
async fn connection_scope_uses_current_session_and_canonicalizes_aliases_without_writes() {
    use crate::vault_ops::connections::{
        put_connection, Connection, ConnectionQuery, ConnectionTarget,
    };
    use rmcp::handler::server::wrapper::Parameters;

    let (server, project_db) = crate::tests::make_server_with_project_fixture("project");
    let root = project_db.parent().unwrap().parent().unwrap();
    let canonical = crate::path_utils::plan_c_dir_name_from_root(root).unwrap();
    put_connection(
        &server.global_db_path_buf(),
        Connection {
            name: "scoped-host".into(),
            purpose: "Scoped synthetic host".into(),
            enabled: true,
            allowed_agents: Vec::new(),
            allowed_projects: vec![canonical.clone()],
            executor: None,
            target: ConnectionTarget::Ssh {
                alias: "synthetic-host".into(),
                remote_cwd: None,
            },
        },
        false,
    )
    .unwrap();
    let manifest = server.tachi_home_dir().join("manifest.json");
    let before = std::fs::read(&manifest).unwrap();
    let alias_path =
        crate::path_utils::plan_c_alias_db_for_root_in_home(root, &server.tachi_home_dir())
            .unwrap();
    let alias_before = std::fs::symlink_metadata(&alias_path).is_ok();
    for session_name in [
        None,
        Some("unregistered"),
        Some("project"),
        Some(canonical.as_str()),
    ] {
        server.set_session_identity(
            None,
            session_name.map(str::to_string),
            Some(tachi_hub::ToolProfile::standard()),
        );
        let query: ConnectionQuery =
            serde_json::from_value(serde_json::json!({"action":"list"})).unwrap();
        let response: serde_json::Value =
            serde_json::from_str(&server.vault_connections(Parameters(query)).await.unwrap())
                .unwrap();
        let has_scope =
            matches!(session_name, Some(name) if name == "project" || name == canonical);
        assert_eq!(
            response["connections"].as_array().unwrap().len(),
            usize::from(has_scope)
        );
        if has_scope {
            assert_eq!(response["current_project"], canonical);
        } else {
            assert!(response["current_project"].is_null());
        }
    }
    assert_eq!(std::fs::read(manifest).unwrap(), before);
    assert_eq!(std::fs::symlink_metadata(alias_path).is_ok(), alias_before);
}

#[cfg(unix)]
#[tokio::test]
async fn connection_scope_rejects_conflicting_project_alias() {
    use crate::vault_ops::connections::ConnectionQuery;
    use rmcp::handler::server::wrapper::Parameters;
    let (server, project_db) = crate::tests::make_server_with_project_fixture("project");
    let root = project_db.parent().unwrap().parent().unwrap();
    let alias = crate::path_utils::plan_c_alias_db_for_root_in_home(root, &server.tachi_home_dir())
        .unwrap();
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    assert!(!alias.exists());
    std::os::unix::fs::symlink(server.global_db_path_buf(), alias).unwrap();
    server.set_session_identity(
        None,
        Some("project".into()),
        Some(tachi_hub::ToolProfile::standard()),
    );
    let query: ConnectionQuery =
        serde_json::from_value(serde_json::json!({"action":"list"})).unwrap();
    assert_eq!(
        server
            .vault_connections(Parameters(query))
            .await
            .unwrap_err(),
        "Cannot establish the current connection project scope"
    );
}

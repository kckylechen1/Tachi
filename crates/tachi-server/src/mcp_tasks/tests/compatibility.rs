use super::*;

#[test]
fn global_run_binding_is_principal_and_physical_store_scoped() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    let named_owner = current_owner(&server).unwrap();
    std::fs::remove_file(run.join("mcp-tasks.json")).unwrap();
    server.set_session_identity(
        Some("global-host".into()),
        None,
        Some(tachi_hub::ToolProfile::standard()),
    );
    crate::claims_ops::admit_agent_connection(&server, Some("agent.owner".into()), true).unwrap();
    prepare_run_binding(&server, &run, DISPATCH, None).unwrap();
    let owner = current_owner(&server).unwrap();
    assert!(owner.canonical_project.is_empty());
    assert_ne!(owner.source_fingerprint, named_owner.source_fingerprint);
    assert!(
        owned_project_for_dispatch(&server, DISPATCH).is_none(),
        "global scope must not be resolved as a named project"
    );
    let handle = start(&server);
    assert_eq!(
        get(&server, &handle).unwrap().task.status(),
        TaskStatus::Working
    );
    write_status(&run, "TASK_STATE_COMPLETED", 2);
    std::fs::write(run.join("result.md"), "global report\n").unwrap();
    assert!(matches!(
        begin_compatible_result(&server, &request()).unwrap(),
        CallToolResponse::Complete(_)
    ));
    let clone = server.clone_for_mcp_session();
    crate::claims_ops::admit_agent_connection(&clone, Some("agent.intruder".into()), true).unwrap();
    assert_eq!(
        begin_compatible_result(&clone, &request()).unwrap_err(),
        unavailable()
    );
    bind_identity(&clone, "agent.owner");
    assert_eq!(
        begin_compatible_result(&clone, &request()).unwrap_err(),
        unavailable()
    );
    let other = crate::tests::make_server();
    other.set_session_identity(None, None, Some(tachi_hub::ToolProfile::standard()));
    crate::claims_ops::admit_agent_connection(&other, Some("agent.owner".into()), true).unwrap();
    assert_ne!(
        current_owner(&other).unwrap().source_fingerprint,
        owner.source_fingerprint
    );
    assert_eq!(
        begin_compatible_result(&other, &request()).unwrap_err(),
        unavailable()
    );
}

#[test]
fn historical_terminal_result_does_not_create_grant_or_bypass_existing_binding() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    write_status(&run, "TASK_STATE_COMPLETED", 2);
    std::fs::write(run.join("result.md"), "retained historical report\n").unwrap();
    let original = std::fs::read(run.join("mcp-tasks.json")).unwrap();
    let clone = server.clone_for_mcp_session();
    bind_identity(&clone, "agent.intruder");
    assert_eq!(
        begin_compatible_result(&clone, &request()).unwrap_err(),
        unavailable()
    );
    clone.set_session_identity(None, None, Some(tachi_hub::ToolProfile::standard()));
    assert_eq!(
        begin_compatible_result(&clone, &request()).unwrap_err(),
        unavailable()
    );
    for invalid in [b"{broken".as_slice(), b"null".as_slice()] {
        std::fs::write(run.join("mcp-tasks.json"), invalid).unwrap();
        assert_eq!(
            begin_compatible_result(&server, &request()).unwrap_err(),
            unavailable()
        );
    }
    std::fs::remove_file(run.join("mcp-tasks.json")).unwrap();
    #[cfg(unix)]
    {
        let target = run.join("private-binding");
        std::fs::write(&target, &original).unwrap();
        std::os::unix::fs::symlink(&target, run.join("mcp-tasks.json")).unwrap();
        assert_eq!(
            begin_compatible_result(&server, &request()).unwrap_err(),
            unavailable()
        );
        std::fs::remove_file(run.join("mcp-tasks.json")).unwrap();
    }
    // Pre-revision historical receipts are still eligible for a synchronous read.
    std::fs::write(
        run.join("status.json"),
        serde_json::to_vec(&json!({"dispatch_id":DISPATCH, "state":"TASK_STATE_COMPLETED"}))
            .unwrap(),
    )
    .unwrap();
    let CallToolResponse::Complete(result) = begin_compatible_result(&clone, &request()).unwrap()
    else {
        panic!("historical read must be complete")
    };
    let value = serde_json::to_value(result).unwrap();
    let body: Value = serde_json::from_str(value["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["dispatch_id"], DISPATCH);
    assert_eq!(body["state"], "TASK_STATE_COMPLETED");
    assert_eq!(body["result"]["body"], "retained historical report\n");
    assert_eq!(body["result"]["truncated"], false);
    assert!(!run.join("mcp-tasks.json").exists());
    assert_eq!(
        begin_result(&clone, &request()).unwrap_err(),
        unavailable(),
        "strict Tasks API stays unchanged"
    );
    write_status(&run, "TASK_STATE_WORKING", 3);
    assert_eq!(
        begin_compatible_result(&clone, &request()).unwrap_err(),
        unavailable(),
        "an unbound live run gains no Tasks handle"
    );
}

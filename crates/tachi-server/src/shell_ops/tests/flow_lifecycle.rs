use super::*;

#[test]
fn flow_id_is_well_formed() {
    let id = new_flow_id(Some("Refactor Tachi Shell"), None);
    assert!(id.starts_with("flow_"));
    assert!(id.contains("refactor-tachi-shell"));
}

#[test]
fn resolve_or_create_flow_creates_dir() {
    let _root = temp_runs_root();
    let p = TachiShellParams {
        action: "dispatch".into(),
        format: None,
        flow_id: None,
        task: Some("hello".into()),
        title: Some("hello".into()),
        agent: None,
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        async_dispatch: false,
        dispatch_reason: None,
        project: None,
        limit: None,
        notes: None,
        validation: vec![],
        allowed_scope: vec![],
    };
    let (fid, dir, created) = resolve_or_create_flow(&p, "hello").unwrap();
    assert!(created);
    assert!(dir.exists());
    assert!(fid.starts_with("flow_"));
}

#[test]
fn advance_stage_writes_status_and_events() {
    let _root = temp_runs_root();
    let p = TachiShellParams {
        action: "dispatch".into(),
        format: None,
        flow_id: None,
        task: Some("t".into()),
        title: Some("t".into()),
        agent: None,
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        async_dispatch: false,
        dispatch_reason: None,
        project: None,
        limit: None,
        notes: None,
        validation: vec![],
        allowed_scope: vec![],
    };
    let (fid, dir, _created) = resolve_or_create_flow(&p, "t").unwrap();
    let inj = InjectionResult {
        required: true,
        rel_path: Some("skill/x".into()),
        source_path: None,
        injected_path: None,
        content_hash: None,
        loaded: false,
        warning: Some("missing".into()),
        failure_class: Some("missing_source_roots"),
    };
    advance_stage(&dir, &fid, "dispatch", "t", &inj, true).unwrap();
    let status = read_status(&dir);
    assert_eq!(
        status.get("stage").and_then(|v| v.as_str()),
        Some("dispatch")
    );
    assert_eq!(
        status.get("state").and_then(|v| v.as_str()),
        Some("dispatch_ready")
    );
    let events = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(events.contains("flow_created"));
}

#[tokio::test]
async fn async_shell_dispatch_requires_native_first_exception_before_artifacts() {
    let runs_root = temp_runs_root();
    let db_dir = tempfile::tempdir().expect("temp DB directory");
    let server =
        crate::MemoryServer::new(db_dir.path().join("memory.db"), None).expect("test server");
    let mut params = TachiShellParams {
        action: "dispatch".into(),
        format: None,
        flow_id: None,
        task: Some("ordinary local delegation".into()),
        title: Some("native first".into()),
        agent: Some("claude".into()),
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        async_dispatch: true,
        dispatch_reason: None,
        project: None,
        limit: None,
        notes: None,
        validation: vec![],
        allowed_scope: vec![],
    };

    let error = handle_tachi_shell(&server, params.clone())
        .await
        .expect_err("bare async dispatch must fail closed");
    assert!(error.contains("native subagent"));
    assert_eq!(
        std::fs::read_dir(&*runs_root)
            .expect("read runs root")
            .count(),
        0,
        "refusal must precede flow/run artifact creation"
    );

    params.dispatch_reason = Some(tachi_params::TachiDispatchReason::ExplicitUserRequest);
    params.async_dispatch = false;
    handle_tachi_shell(&server, params)
        .await
        .expect("packet-only shell dispatch remains available");
}

#[tokio::test]
async fn status_action_returns_not_found_for_missing_flow() {
    let _root = temp_runs_root();
    let p = TachiShellParams {
        action: "status".into(),
        format: None,
        flow_id: Some("flow_does_not_exist".into()),
        task: None,
        title: None,
        agent: None,
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        async_dispatch: false,
        dispatch_reason: None,
        project: None,
        limit: None,
        notes: None,
        validation: vec![],
        allowed_scope: vec![],
    };
    let out = handle_status_action(p).await.unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.get("found").and_then(|x| x.as_bool()), Some(false));
}

#[tokio::test]
async fn status_action_lists_known_flows() {
    let root = temp_runs_root();
    // Pre-create a flow dir + status.json
    let fdir = root.join("flow_20260505T000000Z_demo");
    std::fs::create_dir_all(&fdir).unwrap();
    std::fs::write(
            fdir.join("status.json"),
            r#"{"flow_id":"flow_20260505T000000Z_demo","stage":"dispatch","state":"dispatch_ready","updated_at":"2026-05-05T00:00:00Z"}"#,
        )
        .unwrap();
    let p = TachiShellParams {
        action: "status".into(),
        format: None,
        flow_id: None,
        task: None,
        title: None,
        agent: None,
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        async_dispatch: false,
        dispatch_reason: None,
        project: None,
        limit: Some(10),
        notes: None,
        validation: vec![],
        allowed_scope: vec![],
    };
    let out = handle_status_action(p).await.unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let flows = v
        .get("flows")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        flows.iter().any(
            |f| f.get("flow_id").and_then(|x| x.as_str()) == Some("flow_20260505T000000Z_demo")
        ),
        "expected demo flow in listing, got {v}"
    );
}

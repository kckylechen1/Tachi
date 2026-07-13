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
        action: "plan".into(),
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
        project: None,
        state_filter: None,
        limit: None,
        notes: None,
        validation: vec![],
        allowed_scope: vec![],
        slices: vec![],
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
        action: "plan".into(),
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
        project: None,
        state_filter: None,
        limit: None,
        notes: None,
        validation: vec![],
        allowed_scope: vec![],
        slices: vec![],
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
    advance_stage(&dir, &fid, "plan", "t", &inj, true).unwrap();
    let status = read_status(&dir);
    assert_eq!(status.get("stage").and_then(|v| v.as_str()), Some("plan"));
    assert_eq!(
        status.get("state").and_then(|v| v.as_str()),
        Some("instruction_ready")
    );
    let events = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(events.contains("flow_created"));
}

#[test]
fn flow_id_rejects_path_traversal() {
    for invalid in [
        "../../etc",
        "flow_../../etc",
        "/tmp/evil",
        "flow_/tmp/evil",
        "flow_..",
        "flow_bad/name",
        "flow_bad\\name",
        "notflow_20260505",
    ] {
        assert!(
            validate_flow_id(invalid).is_err(),
            "expected invalid flow_id to be rejected: {invalid}"
        );
    }
    assert!(validate_flow_id("flow_20260505T000000Z_demo-1").is_ok());
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
        project: None,
        state_filter: None,
        limit: None,
        notes: None,
        validation: vec![],
        allowed_scope: vec![],
        slices: vec![],
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
            r#"{"flow_id":"flow_20260505T000000Z_demo","stage":"plan","state":"instruction_ready","updated_at":"2026-05-05T00:00:00Z"}"#,
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
        project: None,
        state_filter: None,
        limit: Some(10),
        notes: None,
        validation: vec![],
        allowed_scope: vec![],
        slices: vec![],
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

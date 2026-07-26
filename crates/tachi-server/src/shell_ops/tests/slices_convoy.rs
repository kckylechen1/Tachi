use super::*;

#[test]
fn resolve_slice_id_uses_explicit_id() {
    let slice = TachiShellDispatchSliceParams {
        id: Some("my-slice".into()),
        task: Some("do thing".into()),
        title: Some("My Slice".into()),
        agent: None,
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        notes: None,
        validation: Vec::new(),
        allowed_scope: Vec::new(),
    };
    assert_eq!(resolve_slice_id(0, &slice).unwrap(), "my-slice");
}

#[test]
fn resolve_slice_id_falls_back_to_title() {
    let slice = TachiShellDispatchSliceParams {
        id: None,
        task: Some("do thing".into()),
        title: Some("Hello World".into()),
        agent: None,
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        notes: None,
        validation: Vec::new(),
        allowed_scope: Vec::new(),
    };
    assert_eq!(resolve_slice_id(0, &slice).unwrap(), "hello-world");
}

#[test]
fn resolve_slice_id_falls_back_to_task() {
    let slice = TachiShellDispatchSliceParams {
        id: None,
        task: Some("Refactor Core".into()),
        title: None,
        agent: None,
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        notes: None,
        validation: Vec::new(),
        allowed_scope: Vec::new(),
    };
    assert_eq!(resolve_slice_id(0, &slice).unwrap(), "refactor-core");
}

#[test]
fn resolve_slice_id_falls_back_to_index() {
    let slice = TachiShellDispatchSliceParams {
        id: None,
        task: None,
        title: None,
        agent: None,
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        notes: None,
        validation: Vec::new(),
        allowed_scope: Vec::new(),
    };
    assert_eq!(resolve_slice_id(3, &slice).unwrap(), "slice-4");
}

#[test]
fn resolve_slice_id_rejects_traversal() {
    for invalid in ["../../etc", "slice/evil", "slice\\bad", "slice..whoops"] {
        assert!(
            validate_slice_id(invalid).is_err(),
            "expected invalid slice id to be rejected: {invalid}"
        );
    }
    assert!(validate_slice_id("alpha-1").is_ok());
}

#[tokio::test]
async fn convoy_dispatch_creates_slice_dirs_and_status() {
    let _root = temp_runs_root();
    let server = {
        let db_path = crate::utils::test_fixture_path(format!(
            "memory-server-convoy-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        crate::MemoryServer::new(db_path, None).expect("test server")
    };
    let params = TachiShellParams {
        action: "dispatch".into(),
        format: None,
        flow_id: None,
        task: Some("parent task: review GitHub PRs and issues".into()),
        title: Some("convoy test".into()),
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
        slices: vec![
            TachiShellDispatchSliceParams {
                id: Some("alpha".into()),
                task: Some("slice alpha task: inspect the new PR".into()),
                title: Some("Alpha Slice".into()),
                agent: None,
                profile: None,
                cwd: None,
                tool_profile: None,
                mcp_access: None,
                allowed_mcp_servers: Vec::new(),
                notes: None,
                validation: Vec::new(),
                allowed_scope: Vec::new(),
            },
            TachiShellDispatchSliceParams {
                id: Some("beta".into()),
                task: Some("slice beta task".into()),
                title: None,
                agent: None,
                profile: None,
                cwd: None,
                tool_profile: None,
                mcp_access: None,
                allowed_mcp_servers: Vec::new(),
                notes: None,
                validation: Vec::new(),
                allowed_scope: Vec::new(),
            },
        ],
    };
    let out = handle_tachi_shell(&server, params).await.unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.get("convoy").and_then(|x| x.as_bool()), Some(true));
    assert_eq!(v.get("async").and_then(|x| x.as_bool()), Some(false));
    assert_eq!(
        v.get("dispatch_ids")
            .and_then(|x| x.as_array())
            .map(|a| a.len()),
        Some(0)
    );
    let slices = v
        .get("slices")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(slices.len(), 2);

    let run_dir = PathBuf::from(v.get("run_dir").unwrap().as_str().unwrap());
    assert!(run_dir.join("slices/alpha/instruction.md").exists());
    assert!(run_dir.join("slices/beta/instruction.md").exists());
    let parent_instruction = std::fs::read_to_string(run_dir.join("instruction.md")).unwrap();
    assert!(
        parent_instruction.contains("skill:superpowers-subagent-driven-development"),
        "{parent_instruction}"
    );
    assert!(
        parent_instruction.contains("## Worker Factory Contract"),
        "{parent_instruction}"
    );
    let alpha_instruction =
        std::fs::read_to_string(run_dir.join("slices/alpha/instruction.md")).unwrap();
    assert!(
        alpha_instruction.contains("skill:waza-check"),
        "{alpha_instruction}"
    );
    assert!(
        alpha_instruction.contains("## Worker Report-Back Contract"),
        "{alpha_instruction}"
    );

    let status = read_status(&run_dir);
    let convoy = status.get("convoy").unwrap();
    assert_eq!(
        convoy.get("mode").and_then(|x| x.as_str()),
        Some("parallel")
    );
    assert_eq!(convoy.get("slice_count").and_then(|x| x.as_u64()), Some(2));
    assert_eq!(
        convoy
            .get("native_skill_policy")
            .and_then(|x| x.get("policy"))
            .and_then(|x| x.as_str()),
        Some("native")
    );

    let events_raw = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    let prepared_count = events_raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("event").and_then(|x| x.as_str()) == Some("convoy_slice_prepared"))
        .count();
    assert_eq!(prepared_count, 2);
}

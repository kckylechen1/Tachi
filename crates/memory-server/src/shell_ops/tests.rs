use super::*;

fn runs_env_lock() -> &'static std::sync::Mutex<()> {
    tachi_run_root_env_lock()
}

struct RunsRootGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    path: PathBuf,
}

impl std::ops::Deref for RunsRootGuard {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

fn temp_runs_root() -> RunsRootGuard {
    let guard = runs_env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let d = std::env::temp_dir().join(format!(
        "tachi-shell-test-{}",
        Utc::now().format("%Y%m%dT%H%M%S%fZ")
    ));
    std::fs::create_dir_all(&d).unwrap();
    // SAFETY: `set_var` is unsafe on edition 2021 because it can race with
    // other threads reading the same env key. This call is safe because:
    //   1. The `runs_env_lock` mutex is held for the entire lifetime of
    //      `RunsRootGuard`, serialising all `temp_runs_root()` callers.
    //   2. The `Drop` impl restores the original value under the same lock.
    //   3. No other code path mutates `TACHI_RUN_ROOT`.
    unsafe {
        std::env::set_var("TACHI_RUN_ROOT", &d);
    }
    RunsRootGuard {
        _guard: guard,
        path: d,
    }
}

#[test]
fn slugify_basic() {
    assert_eq!(slugify("Hello, World!"), "hello-world");
    assert_eq!(slugify("   "), "flow");
    assert_eq!(slugify("已经-OK_test"), "ok-test");
}

#[test]
fn meta_skill_mapping_is_complete() {
    for stage in &["brainstorm", "plan", "dispatch", "review", "ship"] {
        assert!(meta_skill_for_stage(stage).is_some(), "stage {stage}");
    }
    assert!(meta_skill_for_stage("kanban").is_none());
    assert!(meta_skill_for_stage("status").is_none());
}

#[test]
fn superpowers_meta_skills_resolve_for_all_shell_stages() {
    for stage in STAGE_ACTIONS {
        let rel = meta_skill_for_stage(stage).expect("mapped stage");
        let resolved = resolve_meta_skill(rel)
            .unwrap_or_else(|| panic!("superpowers skill not found for stage {stage} at {rel}"));
        assert!(
            resolved.ends_with("SKILL.md"),
            "stage {stage} should resolve to SKILL.md, got {}",
            resolved.display()
        );
        let content = std::fs::read_to_string(&resolved)
            .unwrap_or_else(|e| panic!("read superpowers skill for {stage}: {e}"));
        assert!(
            content.contains("name:") || content.starts_with("# "),
            "stage {stage} skill should look like a SKILL.md front matter or heading"
        );
    }
}

#[test]
fn build_instruction_includes_required_sections() {
    let inj = InjectionResult {
        required: true,
        rel_path: Some("skill/x/SKILL.md".into()),
        source_path: Some("/abs/skill/x/SKILL.md".into()),
        injected_path: Some(".tachi/runs/flow_x/injected/superpowers-plan.md".into()),
        content_hash: Some("a".repeat(16)),
        loaded: true,
        warning: None,
    };
    let s = build_instruction_md(
        "flow_x",
        "plan",
        "do the thing",
        &inj,
        Some("be careful"),
        &["cargo test".to_string()],
        &["crates/memory-server/**".to_string()],
    );
    assert!(s.contains("flow_x"));
    assert!(s.contains("Stage: **plan**"));
    assert!(s.contains("do the thing"));
    assert!(s.contains("superpowers-plan.md"));
    assert!(s.contains("## Native Lifecycle Policy"));
    assert!(s.contains("skill:superpowers-writing-plans"));
    assert!(s.contains("skill:waza-think"));
    assert!(s.contains("max 6 concurrent workers"));
    assert!(s.contains("cargo test"));
    assert!(s.contains("crates/memory-server/**"));
    assert!(s.contains("be careful"));
}

#[test]
fn ship_instruction_includes_pr_first_release_flow() {
    let inj = InjectionResult {
        required: true,
        rel_path: Some("skill/x/SKILL.md".into()),
        source_path: None,
        injected_path: Some(".tachi/runs/flow_x/injected/superpowers-ship.md".into()),
        content_hash: Some("b".repeat(16)),
        loaded: true,
        warning: None,
    };
    let s = build_instruction_md("flow_x", "ship", "ship it", &inj, None, &[], &[]);
    assert!(s.contains("## Release Flow"));
    assert!(s.contains("Push the feature branch"));
    assert!(s.contains("Open a PR"));
    assert!(s.contains("Pass the PR gate"));
    assert!(s.contains("CI checks"));
    assert!(s.contains("Merge the PR"));
    assert!(!s.contains("direct push to the protected branch:\n\n1. Merge"));
}

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
        let db_path = std::env::temp_dir().join(format!(
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
        project: None,
        state_filter: None,
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

// ─── GitHub status / events helpers ──────────────────────────────────

fn read_events_jsonl(run_dir: &Path) -> Vec<Value> {
    let raw = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("event line is JSON"))
        .collect()
}

fn read_status_obj(run_dir: &Path) -> Value {
    let raw = std::fs::read_to_string(run_dir.join("status.json")).unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or(json!({}))
}

#[test]
fn merge_github_status_creates_block_when_absent() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-create");
    std::fs::create_dir_all(&run_dir).unwrap();

    let merged = merge_github_status(
        &run_dir,
        json!({
            "repo": "kckylec/sigil",
            "issue_number": 42,
            "issue_url": "https://github.com/kckylec/sigil/issues/42",
        }),
    )
    .expect("merge should succeed on empty status");

    assert_eq!(merged["repo"], json!("kckylec/sigil"));
    assert_eq!(merged["issue_number"], json!(42));

    let on_disk = read_status_obj(&run_dir);
    assert_eq!(on_disk["github"]["repo"], json!("kckylec/sigil"));
    assert!(
        on_disk["updated_at"].is_string(),
        "merge_github_status must stamp top-level updated_at"
    );
}

#[test]
fn merge_github_status_deep_merges_partial_patches() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-merge");
    std::fs::create_dir_all(&run_dir).unwrap();

    // Seed: PR created with full checks block.
    merge_github_status(
        &run_dir,
        json!({
            "repo": "owner/repo",
            "pr_number": 7,
            "pr_url": "https://github.com/owner/repo/pull/7",
            "merge_state": "pending",
            "checks": { "state": "pending", "updated_at": "T0" },
        }),
    )
    .unwrap();

    // Patch: only the checks.state changes — checks.updated_at must
    // survive (deep merge), and pr_number / repo must be untouched.
    let merged = merge_github_status(
        &run_dir,
        json!({
            "checks": { "state": "success", "updated_at": "T1" },
        }),
    )
    .unwrap();

    assert_eq!(merged["repo"], json!("owner/repo"));
    assert_eq!(merged["pr_number"], json!(7));
    assert_eq!(merged["checks"]["state"], json!("success"));
    assert_eq!(merged["checks"]["updated_at"], json!("T1"));
    assert_eq!(merged["merge_state"], json!("pending"));

    // Patch: advance merge_state without touching anything else.
    let merged = merge_github_status(&run_dir, json!({ "merge_state": "ready" })).unwrap();
    assert_eq!(merged["merge_state"], json!("ready"));
    assert_eq!(merged["pr_number"], json!(7), "pr_number must persist");
}

#[test]
fn merge_github_status_null_value_clears_field() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-clear");
    std::fs::create_dir_all(&run_dir).unwrap();
    merge_github_status(&run_dir, json!({ "issue_number": 1, "pr_number": 2 })).unwrap();
    let merged = merge_github_status(&run_dir, json!({ "issue_number": null })).unwrap();
    assert!(
        merged.get("issue_number").is_none(),
        "null patch value must remove the key, got: {merged}"
    );
    assert_eq!(merged["pr_number"], json!(2), "pr_number must persist");
}

#[test]
fn merge_github_status_rejects_non_object_patch() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-bad-shape");
    std::fs::create_dir_all(&run_dir).unwrap();
    let err = merge_github_status(&run_dir, json!("not-an-object"))
        .expect_err("string patch must be rejected");
    assert!(err.contains("must be a JSON object"), "got: {err}");
    let err = merge_github_status(&run_dir, json!(null)).expect_err("null patch must be rejected");
    assert!(err.contains("must be a JSON object"), "got: {err}");
}

#[test]
fn merge_github_status_rejects_invalid_merge_state() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-bad-state");
    std::fs::create_dir_all(&run_dir).unwrap();
    let err = merge_github_status(&run_dir, json!({ "merge_state": "exploded" }))
        .expect_err("invalid merge_state must be rejected");
    assert!(err.contains("invalid merge_state"), "got: {err}");
    // No status.json should have been written.
    assert!(
        !run_dir.join("status.json").exists(),
        "rejected patch must not partially write status.json"
    );
}

#[test]
fn append_github_event_writes_typed_event_with_framing() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-event");
    std::fs::create_dir_all(&run_dir).unwrap();

    append_github_event(
        &run_dir,
        "flow-abc",
        "github_pr_created",
        json!({ "pr_number": 99, "pr_url": "https://github.com/o/r/pull/99" }),
    )
    .unwrap();
    append_github_event(
        &run_dir,
        "flow-abc",
        "github_checks_polled",
        json!({ "state": "pending" }),
    )
    .unwrap();

    let events = read_events_jsonl(&run_dir);
    assert_eq!(events.len(), 2, "two events expected, got: {events:?}");

    assert_eq!(events[0]["event"], json!("github_pr_created"));
    assert_eq!(events[0]["flow_id"], json!("flow-abc"));
    assert_eq!(events[0]["pr_number"], json!(99));
    assert!(
        events[0]["timestamp"].is_string(),
        "event must carry an RFC3339 timestamp"
    );

    assert_eq!(events[1]["event"], json!("github_checks_polled"));
    assert_eq!(events[1]["state"], json!("pending"));
}

#[test]
fn append_github_event_rejects_unknown_kind() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-bad-kind");
    std::fs::create_dir_all(&run_dir).unwrap();
    let err = append_github_event(&run_dir, "flow", "github_nukes_launched", json!({}))
        .expect_err("unknown kind must be rejected");
    assert!(err.contains("unknown kind"), "got: {err}");
    assert!(
        !run_dir.join("events.jsonl").exists(),
        "rejected event must not be partially written"
    );
}

#[test]
fn append_github_event_reserved_keys_cannot_be_overridden() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-reserved");
    std::fs::create_dir_all(&run_dir).unwrap();
    append_github_event(
        &run_dir,
        "real-flow",
        "github_pr_merged",
        json!({
            "event": "spoofed",
            "flow_id": "spoofed",
            "timestamp": "spoofed",
            "merge_sha": "deadbeef",
        }),
    )
    .unwrap();
    let events = read_events_jsonl(&run_dir);
    assert_eq!(events[0]["event"], json!("github_pr_merged"));
    assert_eq!(events[0]["flow_id"], json!("real-flow"));
    assert_ne!(events[0]["timestamp"], json!("spoofed"));
    assert_eq!(events[0]["merge_sha"], json!("deadbeef"));
}

#[test]
fn github_block_coexists_with_existing_status_fields() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-coexist");
    std::fs::create_dir_all(&run_dir).unwrap();
    // Pre-seed a status.json that mimics a flow already in `dispatch`.
    crate::utils::write_run_status_file(
        &run_dir,
        &json!({
            "flow_id": "flow-coexist",
            "stage": "dispatch",
            "state": "dispatch_ready",
            "history": [{"stage": "dispatch", "from": "plan", "at": "T0"}],
        }),
    )
    .unwrap();

    merge_github_status(
        &run_dir,
        json!({ "repo": "o/r", "pr_number": 1, "merge_state": "pending" }),
    )
    .unwrap();

    let on_disk = read_status_obj(&run_dir);
    // Pre-existing fields must survive.
    assert_eq!(on_disk["flow_id"], json!("flow-coexist"));
    assert_eq!(on_disk["stage"], json!("dispatch"));
    assert_eq!(on_disk["history"][0]["stage"], json!("dispatch"));
    // New github block was added.
    assert_eq!(on_disk["github"]["repo"], json!("o/r"));
    assert_eq!(on_disk["github"]["pr_number"], json!(1));
}

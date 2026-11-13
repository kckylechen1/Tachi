use super::*;
use memcore::anchored_fs::AnchoredDirectory;
use tachi_params::{ExecutionGrant, ResolvedStaffAssignment, StaffAssignmentRequest};

fn grant(cwd: &Path) -> ExecutionGrant {
    ExecutionGrant {
        grant_id: "cwd-boundary".into(),
        env_id: Some("cwd-boundary-env".into()),
        unmanaged_cwd_allowed: false,
        allowed_cwd: Some(cwd.into()),
        credential_profiles: Vec::new(),
        mcp_access: None,
        allowed_tools: Vec::new(),
        permission_profile: Some("default".into()),
        sandbox: None,
        max_turns: None,
        timeout_secs: 5,
    }
}

fn prepare(
    run: &Path,
    assignment: &ResolvedStaffAssignment,
    grant: &ExecutionGrant,
    command: &[String],
    transport: &str,
) -> PreparedDispatchBackend {
    let request = StaffAssignmentRequest::new(
        tachi_params::TachiDispatchReason::ExplicitUserRequest,
        "observe actual backend cwd",
    );
    let launch = (assignment.selected_backend == "custom").then(|| {
        mint_subprocess_launch_spec(assignment, grant, command, "cwd task", transport, &None)
            .unwrap()
    });
    prepare_dispatch_backend(DispatchBackendContext {
        server: &crate::tests::make_server(),
        trajectory_path: &run.join("trajectory.jsonl"),
        workspace_dir: run,
        dispatch_id: "cwd-boundary",
        request: &request,
        assignment,
        grant,
        command,
        prompt: "observe actual backend cwd",
        custom_launch_spec: launch.as_ref(),
        mcp_config_path: None,
        v2: false,
        plan_generated_at: None,
        plan_duration_ms: None,
        harness_transport: transport,
        harness_server_url: &None,
        timeout_secs_for_status: grant.timeout_secs,
    })
    .unwrap()
}

#[tokio::test]
async fn descriptor_bound_backend_cannot_follow_a_replaced_directory() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let captured = root.path().join("captured");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("identity"), "admitted").unwrap();
    let authority = AnchoredDirectory::open_absolute(&workspace.canonicalize().unwrap()).unwrap();
    let grant = carrier_execution_grant(&grant(&workspace), true);
    let assignment = ResolvedStaffAssignment::new(
        "cwd-boundary",
        tachi_params::TachiDispatchReason::ExplicitUserRequest,
        "custom",
        "custom",
    );
    let mut backend = prepare(
        root.path(),
        &assignment,
        &grant,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "cat identity; touch executed".into(),
        ],
        "cli",
    );
    finalize_dispatch_backend(&mut backend.execution, None, &grant, Some(&authority)).unwrap();
    std::fs::rename(&workspace, &captured).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("identity"), "replacement").unwrap();
    let DispatchExecution::Subprocess(mut command) = backend.execution else {
        panic!("custom subprocess")
    };
    let output = command.output().await.unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "admitted");
    assert!(captured.join("executed").exists());
    assert!(!workspace.join("executed").exists());
}

#[tokio::test]
async fn unmanaged_backend_keeps_its_requested_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut grant = carrier_execution_grant(&grant(&workspace), false);
    grant.env_id = None;
    grant.unmanaged_cwd_allowed = true;
    let assignment = ResolvedStaffAssignment::new(
        "cwd-boundary",
        tachi_params::TachiDispatchReason::ExplicitUserRequest,
        "custom",
        "custom",
    );
    let mut backend = prepare(
        root.path(),
        &assignment,
        &grant,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "pwd -P; touch executed".into(),
        ],
        "cli",
    );
    finalize_dispatch_backend(&mut backend.execution, None, &grant, None).unwrap();
    let DispatchExecution::Subprocess(mut command) = backend.execution else {
        panic!("custom subprocess")
    };
    let output = command.output().await.unwrap();
    assert!(output.status.success());
    assert_eq!(
        Path::new(String::from_utf8(output.stdout).unwrap().trim()),
        workspace.canonicalize().unwrap()
    );
    assert!(workspace.join("executed").exists());
}

/// Interpreter for the ACP peer spawned inside the native sandbox.
/// `/usr/bin/python3` on macOS runner images is the xcrun shim: resolving it
/// spawns `xcodebuild` and writes `$TMPDIR` cache files, both denied by the
/// sandbox profile (`Operation not permitted`). Prefer the first real
/// interpreter on the test PATH; the shim is only a last-resort fallback
/// (it works on hosts whose Command Line Tools resolve without cached lookups).
fn sandbox_capable_python3() -> String {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir == std::path::Path::new("/usr/bin") {
                continue;
            }
            let candidate = dir.join("python3");
            if candidate.is_file() {
                if let Some(found) = candidate.to_str() {
                    return found.to_string();
                }
            }
        }
    }
    "/usr/bin/python3".to_string()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn native_backend_protocol_and_sandbox_use_the_opened_directory() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    assert!(tachi_dispatch::native_sandbox_available());
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let run = root.join("run");
    let auth = root.join("account");
    for path in [&workspace, &run, &auth] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::create_dir(auth.join("opencode")).unwrap();
    std::fs::write(
        auth.join("opencode/auth.json"),
        r#"{"fixture":"no real account"}"#,
    )
    .unwrap();
    let adapter = root.join("peer.py");
    std::fs::write(&adapter, r#"
import json, os, pathlib, sys
options = [{'id':'model','category':'model','type':'select','currentValue':'unselected','options':[{'value':'openai/gpt-6.1-sol','name':'sol'}]}]
observation = {'process_cwd':os.getcwd()}
for line in sys.stdin:
    request = json.loads(line)
    method, params = request['method'], request.get('params', {})
    if method == 'initialize': result = {'protocolVersion':1}
    elif method == 'session/new':
        observation['protocol_cwd'] = params['cwd']
        result = {'sessionId':'cwd-session','configOptions':options}
    elif method == 'session/set_config_option':
        for option in options:
            if option['id'] == params['configId']: option['currentValue'] = params['value']
        result = {'configOptions':options}
    elif method == 'session/prompt':
        pathlib.Path(os.environ['HOME'], 'cwd-observation.json').write_text(json.dumps(observation))
        print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'cwd-session','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'cwd verified'}}}}), flush=True)
        result = {'stopReason':'end_turn'}
    else: raise RuntimeError(method)
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)
"#).unwrap();
    let _env = [
        EnvRestore::set("TACHI_OPENCODE_ACP_COMMAND", &sandbox_capable_python3()),
        EnvRestore::set(
            "TACHI_OPENCODE_ACP_ARGS",
            // `-B` keeps the sandboxed peer from writing bytecode caches.
            &serde_json::to_string(&["-B".to_string(), adapter.to_string_lossy().into_owned()])
                .unwrap(),
        ),
        EnvRestore::set_path("XDG_DATA_HOME", &auth),
    ];
    let mut grant = grant(&workspace);
    grant.sandbox = Some("read-only".into());
    grant.max_turns = Some(4);
    grant.timeout_secs = 1200;
    let assignment = ResolvedStaffAssignment::new(
        "cwd-boundary",
        tachi_params::TachiDispatchReason::ExplicitUserRequest,
        "opencode",
        "opencode",
    )
    .with_profile("opencode_sol_review")
    .with_model("openai/gpt-6.1-sol");
    let authority = AnchoredDirectory::open_absolute(&workspace).unwrap();
    let mut backend = prepare(&run, &assignment, &grant, &[], "acp_native");
    finalize_dispatch_backend(
        &mut backend.execution,
        None,
        &carrier_execution_grant(&grant, true),
        Some(&authority),
    )
    .unwrap();
    let DispatchExecution::NativeAcp(spec) = backend.execution else {
        panic!("native ACP backend")
    };
    assert_eq!(spec.cwd, workspace);
    assert_eq!(spec.sandbox_cwd.as_deref(), Some(workspace.as_path()));
    assert!(spec
        .cwd_authority
        .as_ref()
        .unwrap()
        .matches_absolute_path(&spec.cwd)
        .unwrap());
    let result = crate::dispatch_ops::acp_native::run_native_acp_dispatch(
        *spec,
        &run,
        &run.join("trajectory.jsonl"),
        "cwd-boundary",
        "opencode",
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(result.output, "cwd verified");
    let observed: Value = serde_json::from_slice(
        &std::fs::read(run.join("runtime/home/cwd-observation.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        observed["process_cwd"],
        workspace.to_string_lossy().as_ref()
    );
    assert_eq!(observed["protocol_cwd"], observed["process_cwd"]);
}

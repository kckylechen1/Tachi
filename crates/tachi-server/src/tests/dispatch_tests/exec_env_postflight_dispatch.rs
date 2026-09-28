//! Integration tests for ExecEnv postflight write-contract verification and terminal receipt (#894 S2e, #1322).

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::super::make_server;
use super::{dispatch_params, wait_for_dispatch_status};
use crate::test_support::EnvRestore;

fn seed_managed_lease(
    server: &crate::server_state::MemoryServer,
    lease_path: &std::path::Path,
    suffix: &str,
) -> (String, String) {
    let env_id = format!("env-postflight-{suffix}");
    let resource_id = format!("res-postflight-{suffix}");
    server
        .with_global_store(|store| {
            memcore::insert_exec_env(
                store.connection_mut(),
                &memcore::NewExecEnvLease {
                    env_id: env_id.clone(),
                    kind: "worktree".to_string(),
                    path: lease_path.to_string_lossy().to_string(),
                    repo_root: lease_path.to_string_lossy().to_string(),
                    branch: format!("test/{suffix}"),
                    base_sha: "test-base".to_string(),
                    dispatch_id: None,
                    env_class: memcore::EnvClass::EditOnly,
                    created_at: String::new(),
                },
            )
            .map_err(|error| error.to_string())?;
            memcore::insert_resource(
                store.connection_mut(),
                &memcore::NewExecEnvResource {
                    resource_id: resource_id.clone(),
                    kind: memcore::ResourceKind::Worktree,
                    path: lease_path.to_string_lossy().to_string(),
                    bytes: None,
                    created_at: String::new(),
                },
            )
            .map_err(|error| error.to_string())?;
            memcore::bind_resource(store.connection_mut(), &env_id, &resource_id)
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("seed managed postflight lease");
    (env_id, resource_id)
}

fn resource_state(
    server: &crate::server_state::MemoryServer,
    resource_id: &str,
) -> memcore::ResourceState {
    server
        .with_global_store_read(|store| {
            memcore::get_resource(store.connection(), resource_id)
                .map_err(|error| error.to_string())
        })
        .expect("read managed resource")
        .expect("managed resource exists")
        .state
}

fn lease_state(server: &crate::server_state::MemoryServer, env_id: &str) -> memcore::ExecEnvState {
    server
        .with_global_store_read(|store| {
            memcore::get_exec_env(store.connection(), env_id).map_err(|error| error.to_string())
        })
        .expect("read managed lease")
        .expect("managed lease exists")
        .state
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn required_postflight_rejects_unmanaged_and_default_bindings_before_spawn() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let server = make_server();
    let unmanaged = TempDir::new().expect("unmanaged cwd");

    for (label, cwd, unmanaged_cwd) in [
        (
            "unmanaged",
            Some(unmanaged.path().to_string_lossy().to_string()),
            Some(true),
        ),
        ("default", None, None),
    ] {
        let marker = temp_home.path().join(format!("{label}-spawned"));
        let mut params = dispatch_params(Some("custom"), "required postflight pre-spawn fence");
        params.profile = Some("glm_impl".to_string());
        params.cwd = cwd;
        params.unmanaged_cwd = unmanaged_cwd;
        params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
        params.command = vec![
            "python3".to_string(),
            "-c".to_string(),
            format!("open({marker:?}, 'w').write('spawned')"),
        ];

        let error = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
            .await
            .expect_err("required postflight must reject a non-managed binding");
        assert!(error.contains("requires a managed env_id"), "{error}");
        assert!(!marker.exists(), "{label} worker must not spawn");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn postflight_dispatch_with_declared_scope_accepts_in_scope_write() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");

    let server = make_server();
    let lease_dir = TempDir::new().expect("lease dir");
    let lease_path = lease_dir.path();
    fs::write(lease_path.join("allowed.txt"), b"initial allowed\n").expect("write allowed");
    fs::write(lease_path.join("forbidden.txt"), b"initial forbidden\n").expect("write forbidden");
    let (env_id, resource_id) = seed_managed_lease(&server, lease_path, "clean");

    let mut params = dispatch_params(Some("custom"), "declared scope dispatch");
    params.profile = Some("glm_impl".to_string());
    params.env_id = Some(env_id.clone());
    params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "open('allowed.txt', 'w').write('updated allowed')".to_string(),
    ];

    let result = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch start");
    let response: serde_json::Value = serde_json::from_str(&result).expect("dispatch json");
    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run_dir"));

    let terminal_status = wait_for_dispatch_status(&run_dir).await;
    assert!(matches!(
        terminal_status["state"].as_str(),
        Some("TASK_STATE_COMPLETED" | "TASK_STATE_CLOSED")
    ));
    assert_eq!(terminal_status["result_written"], true);

    let postflight = &terminal_status["exec_env_postflight"];
    assert_eq!(postflight["gate"], "exec_env_postflight");
    assert_eq!(postflight["verdict"], "clean");
    assert_eq!(postflight["artifacts"], "released");
    assert_eq!(postflight["lease_action"], "none");
    assert_eq!(
        resource_state(&server, &resource_id),
        memcore::ResourceState::Active
    );
    assert_eq!(lease_state(&server, &env_id), memcore::ExecEnvState::Active);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn postflight_dispatch_rejects_and_withholds_when_worker_mutates_out_of_scope() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");

    let server = make_server();
    let lease_dir = TempDir::new().expect("lease dir");
    let lease_path = lease_dir.path();
    fs::write(lease_path.join("allowed.txt"), b"initial allowed\n").expect("write allowed");
    fs::write(lease_path.join("forbidden.txt"), b"initial forbidden\n").expect("write forbidden");
    let (env_id, resource_id) = seed_managed_lease(&server, lease_path, "mutated");

    let mut params = dispatch_params(Some("custom"), "mutating out of scope dispatch");
    params.profile = Some("glm_impl".to_string());
    params.env_id = Some(env_id.clone());
    params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "open('forbidden.txt', 'w').write('bad mutation'); print('SECRET_POSTFLIGHT_OUTPUT')"
            .to_string(),
    ];

    let result = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch start");
    let response: serde_json::Value = serde_json::from_str(&result).expect("dispatch json");
    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run_dir"));

    let terminal_status = wait_for_dispatch_status(&run_dir).await;
    assert_eq!(terminal_status["state"], "TASK_STATE_FAILED");
    assert_eq!(terminal_status["result_written"], false);
    assert!(terminal_status["result_persist_error"].is_string());

    let postflight = &terminal_status["exec_env_postflight"];
    assert_eq!(postflight["gate"], "exec_env_postflight");
    assert_eq!(postflight["verdict"], "rejected");
    assert_eq!(postflight["artifacts"], "withheld");
    assert_eq!(postflight["lease_action"], "quarantined");
    assert_eq!(
        resource_state(&server, &resource_id),
        memcore::ResourceState::Quarantined
    );
    assert_eq!(lease_state(&server, &env_id), memcore::ExecEnvState::Active);
    let trajectory = fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory");
    assert!(
        !trajectory.contains("SECRET_POSTFLIGHT_OUTPUT"),
        "rejected carrier output must remain parent-staged and unpublished"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn postflight_dispatch_rejects_and_withholds_when_untracked_file_created_outside_scope() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");

    let server = make_server();
    let lease_dir = TempDir::new().expect("lease dir");
    let lease_path = lease_dir.path();
    fs::write(lease_path.join("allowed.txt"), b"initial allowed\n").expect("write allowed");
    let (env_id, resource_id) = seed_managed_lease(&server, lease_path, "created");

    let mut params = dispatch_params(Some("custom"), "unauthorized creation dispatch");
    params.profile = Some("glm_impl".to_string());
    params.env_id = Some(env_id.clone());
    params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "open('untracked_secret.txt', 'w').write('unauthorized creation')".to_string(),
    ];

    let result = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch start");
    let response: serde_json::Value = serde_json::from_str(&result).expect("dispatch json");
    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run_dir"));

    let terminal_status = wait_for_dispatch_status(&run_dir).await;
    assert_eq!(terminal_status["state"], "TASK_STATE_FAILED");
    assert_eq!(terminal_status["result_written"], false);
    assert!(terminal_status["result_persist_error"].is_string());

    let postflight = &terminal_status["exec_env_postflight"];
    assert_eq!(postflight["gate"], "exec_env_postflight");
    assert_eq!(postflight["verdict"], "rejected");
    assert_eq!(postflight["artifacts"], "withheld");
    assert_eq!(postflight["lease_action"], "quarantined");
    assert_eq!(
        resource_state(&server, &resource_id),
        memcore::ResourceState::Quarantined
    );
    assert_eq!(lease_state(&server, &env_id), memcore::ExecEnvState::Active);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admitted_declared_scope_is_visible_on_the_automatic_presence_claim() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");

    let server = make_server();
    let lease_dir = TempDir::new().expect("lease dir");
    fs::write(lease_dir.path().join("allowed.txt"), b"initial\n").expect("write allowed");
    let (env_id, _) = seed_managed_lease(&server, lease_dir.path(), "claim-scope");

    let mut params = dispatch_params(Some("custom"), "observable declared scope dispatch");
    params.profile = Some("glm_impl".to_string());
    params.env_id = Some(env_id);
    params.issue_ref = Some("#1322-claim-scope".to_string());
    params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import time; time.sleep(0.5)".to_string(),
    ];

    let result = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch start");
    let response: serde_json::Value = serde_json::from_str(&result).expect("dispatch json");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
    let observed_scope: String = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT declared_file_scope FROM session_claims WHERE dispatch_id = ?1 AND state = 'active'",
                    rusqlite::params![dispatch_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("read automatic presence claim");
    assert_eq!(observed_scope, r#"["allowed.txt"]"#);

    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let _ = wait_for_dispatch_status(&run_dir).await;
}

fn reclaim_reason(server: &crate::server_state::MemoryServer, resource_id: &str) -> String {
    server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT reclaim_reason FROM exec_env_resources WHERE resource_id = ?1",
                    rusqlite::params![resource_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("read fenced resource reason")
}

fn flow_dispatch_slot_dir(flow_id: &str) -> PathBuf {
    crate::task_lifecycle::run_dir_for_flow_id(flow_id)
        .expect("valid flow run directory")
        .join(".dispatch-dedupe")
}

fn flow_dispatch_slot_files(flow_id: &str) -> Vec<String> {
    let dir = flow_dispatch_slot_dir(flow_id);
    if !dir.exists() {
        return Vec::new();
    }
    std::fs::read_dir(&dir)
        .expect("read flow dispatch slot dir")
        .filter_map(|entry| {
            entry
                .ok()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
        })
        .collect()
}

/// #2022 reviewer-confirmed regression discriminator: aborting the detached
/// background dispatch while its required postflight gate is paused must NOT
/// release the armed early-exit cleanup (credential materializations, flow
/// dispatch slot, exclusive postflight lease) before the gate resolves, and
/// the resolved fence plus terminal trajectory event must still be persisted.
///
/// A `spawn_blocking(..).await` at the gate handoff (the shape this test was
/// written against) drops the dispatch future at that await point: the Drop of
/// `BackgroundEarlyExitCleanup` fences the resource with the *abandonment*
/// reason, reopens the lease and deletes the credentials/slot while the
/// detached gate is still paused, and the gate's verdict is then discarded —
/// no fence with delta evidence, no `exec_env_postflight` trajectory event.
/// The non-cancellable `run -> apply_verdict -> trajectory` section keeps
/// aborts from taking effect until the fence is persisted.
///
/// The pause/resume is a deterministic barrier at the top of the real
/// `PostflightGate::run` (test-only seam keyed by the isolated home/run
/// root); the only bounded wait is the negative proof that the aborted handle
/// does not finish while the gate is paused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn aborting_a_paused_postflight_gate_cannot_release_cleanup_before_resolution() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = TempDir::new().expect("temp home");
    let temp_runs = TempDir::new().expect("temp run root");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvRestore::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let _v2 = EnvRestore::set("DISPATCH_V2_ENABLED", "false");

    let server = make_server();
    let lease_dir = TempDir::new().expect("lease dir");
    let lease_path = lease_dir.path();
    fs::write(lease_path.join("allowed.txt"), b"initial allowed\n").expect("write allowed");
    fs::write(lease_path.join("forbidden.txt"), b"initial forbidden\n").expect("write forbidden");
    // Run-scoped credential materialization: a file under the dispatch run
    // dir, so the armed early-exit cleanup owns a real ephemeral target.
    let credentials_profile_dir = lease_path.join(".tachi").join("credentials");
    fs::create_dir_all(&credentials_profile_dir).expect("create credential profile dir");
    fs::write(
        credentials_profile_dir.join("postflight-abort.json"),
        r#"{
          "credential_profiles": {
            "postflight_abort_file": {
              "entries": { "abort_secret": "POSTFLIGHT_ABORT_SECRET" },
              "allowed_consumers": { "agents": ["custom"] },
              "materializers": [
                { "type": "file_copy", "source": "abort_secret", "target": "{credentials_dir}/postflight-abort.json" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");
    server
        .vault_init(rmcp::handler::server::wrapper::Parameters(
            crate::vault_ops::VaultInitParams {
                password: "paused-gate abort regression vault".to_string(),
            },
        ))
        .await
        .expect("vault_init");
    server
        .vault_set(rmcp::handler::server::wrapper::Parameters(
            crate::vault_ops::VaultSetParams {
                name: "POSTFLIGHT_ABORT_SECRET".to_string(),
                value: "postflight-abort-secret-value".to_string(),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "run-scoped materialization for the paused-gate abort regression"
                    .to_string(),
                allowed_agents: Some(vec!["custom".to_string()]),
                enable_rotation: false,
                rotation_strategy: None,
                rebind: false,
            },
        ))
        .await
        .expect("vault_set");
    let (env_id, resource_id) = seed_managed_lease(&server, lease_path, "gate-abort");

    // Deterministic boundaries: hold the real gate at the top of its run, and
    // capture the real detached dispatch task's JoinHandle.
    let (_gate_barrier, gate_entered, gate_release) =
        crate::exec_env_postflight::gate_run_barrier::install_postflight_gate_run_barrier(
            temp_home.path(),
            temp_runs.path(),
        );
    let (_capture_guard, background_rx) =
        crate::dispatch_ops::install_background_dispatch_abort_capture(
            temp_home.path(),
            temp_runs.path(),
        );

    const FLOW_ID: &str = "flow_2022_paused_gate_abort";
    let mut params = dispatch_params(Some("custom"), "paused postflight gate abort regression");
    params.profile = Some("glm_impl".to_string());
    params.env_id = Some(env_id.clone());
    params.flow_id = Some(FLOW_ID.to_string());
    params.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
    params.credential_profiles = vec!["postflight_abort_file".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "open('forbidden.txt', 'w').write('paused-gate abort regression mutation')".to_string(),
    ];

    let result = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch start");
    let response: serde_json::Value = serde_json::from_str(&result).expect("dispatch json");
    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let materialized_credential = run_dir.join("credentials/postflight-abort.json");

    // The gate is provably paused: it entered its run and is blocked on the
    // barrier. The dispatch task is inside its synchronous gate section.
    tokio::task::spawn_blocking(move || {
        gate_entered
            .recv()
            .expect("postflight gate entered its run")
    })
    .await
    .expect("gate entered join");

    // The handle was handed over at spawn time, so it is already buffered;
    // take it off the sync channel from a blocking thread.
    let mut background = tokio::task::spawn_blocking(move || {
        background_rx
            .recv()
            .expect("captured background dispatch handle")
    })
    .await
    .expect("background handle capture join");

    // Pre-abort invariants: the exclusive admission and the armed cleanup are
    // all held while the gate is unresolved.
    assert_eq!(
        lease_state(&server, &env_id),
        memcore::ExecEnvState::Dispatching
    );
    assert_eq!(
        resource_state(&server, &resource_id),
        memcore::ResourceState::Active
    );
    assert!(
        materialized_credential.exists(),
        "the run must hold a real credential materialization for the armed cleanup to own"
    );
    assert_eq!(
        flow_dispatch_slot_files(FLOW_ID).len(),
        1,
        "the flow dispatch slot must be reserved for the active run"
    );

    // Abort the detached dispatch owner while the gate is paused.
    background.abort();

    // Negative proof (bounded): the handle must NOT finish while the gate is
    // paused. With an `.await` at the gate handoff the abort drops the
    // dispatch future immediately — the handle finishes here and every
    // assertion below flips.
    let dropped_during_pause =
        tokio::time::timeout(std::time::Duration::from_millis(1200), &mut background).await;
    assert!(
        dropped_during_pause.is_err(),
        "the aborted background dispatch must not be dropped while the postflight gate is \
         still unresolved; join result: {dropped_during_pause:?}"
    );

    // Cleanup cannot precede resolution: credentials, slot and exclusive
    // lease admission are all still held by the paused dispatch.
    assert_eq!(
        lease_state(&server, &env_id),
        memcore::ExecEnvState::Dispatching,
        "the exclusive postflight lease admission must outlive the abort until the gate resolves"
    );
    assert_eq!(
        resource_state(&server, &resource_id),
        memcore::ResourceState::Active,
        "the lease resource must not be fenced by the abandonment path while the gate is \
         still running"
    );
    assert!(
        materialized_credential.exists(),
        "the ephemeral credential materialization must not be cleaned up before the gate \
         resolves"
    );
    assert_eq!(
        flow_dispatch_slot_files(FLOW_ID).len(),
        1,
        "the flow dispatch slot must stay held until the gate resolves"
    );

    // Release the gate: run -> apply_verdict -> trajectory completes in the
    // aborted task's synchronous section before the abort can take effect.
    gate_release
        .send(())
        .expect("release the paused postflight gate");

    let joined = tokio::time::timeout(std::time::Duration::from_secs(15), &mut background)
        .await
        .expect("aborted background dispatch resolves after gate release");
    assert!(
        matches!(&joined, Err(join_error) if join_error.is_cancelled()),
        "the abort must land at the first await after the gate section, not before it: \
         {joined:?}"
    );

    // Fencing was persisted by the gate's own verdict — with the delta
    // evidence, not the lease-guard abandonment reason.
    assert_eq!(
        resource_state(&server, &resource_id),
        memcore::ResourceState::Quarantined,
        "the resolved rejection must fence the lease resource"
    );
    let reason = reclaim_reason(&server, &resource_id);
    assert!(
        reason.contains("forbidden.txt"),
        "the fence reason must carry the gate's delta evidence: {reason}"
    );
    assert!(
        !reason.contains("dispatch aborted before postflight ownership completed"),
        "the fence must not be attributed to the abandonment path: {reason}"
    );
    assert_eq!(
        lease_state(&server, &env_id),
        memcore::ExecEnvState::Active,
        "release_after_fence must reopen the lease once the fence is persisted"
    );

    // The terminal trajectory event survived the abort.
    let trajectory = fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory");
    let postflight_events: Vec<serde_json::Value> = trajectory
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| {
            event.get("event").and_then(serde_json::Value::as_str) == Some("exec_env_postflight")
        })
        .collect();
    assert_eq!(
        postflight_events.len(),
        1,
        "exactly one resolved postflight receipt must be appended to the trajectory"
    );
    let receipt = &postflight_events[0];
    assert_eq!(receipt["verdict"], "rejected");
    assert_eq!(receipt["lease_action"], "quarantined");
    assert!(
        receipt["prohibited_deltas"]
            .as_array()
            .is_some_and(|deltas| deltas.iter().any(|delta| delta["path"]
                .as_str()
                .is_some_and(|path| path.contains("forbidden.txt")))),
        "the persisted receipt must name the out-of-scope mutation: {receipt}"
    );

    // Cleanup did happen — after resolution, via the armed Drop.
    assert!(
        !materialized_credential.exists(),
        "the early-exit cleanup must remove the credential materialization after the gate \
         resolved and the abort landed"
    );
    assert!(
        flow_dispatch_slot_files(FLOW_ID).is_empty(),
        "the early-exit cleanup must release the flow dispatch slot after resolution"
    );
}

use super::*;
use std::os::unix::fs::PermissionsExt;

/// Exercise the actual Staff entry, prepared backend, managed runner and
/// terminal writer. A fake executable is a protocol discriminator, never a
/// claim that a live model or its native sandbox was validated.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dsh_staff_completed_failed_turn_and_nonzero_exit_are_distinct() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("isolated Tachi home");
    let runs = tempfile::tempdir().expect("isolated runs");
    let bins = tempfile::tempdir().expect("fake DSH bin");
    let _home = EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let _host = EnvRestore::set("TACHI_HOST_PROFILE", "development");
    let _polls = EnvRestore::set("TACHI_DISPATCH_WATCHDOG_POLLS", "0");
    let mut entries = vec![bins.path().to_path_buf()];
    entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(entries).expect("fixture PATH");
    let _path = EnvRestore::set_os("PATH", &path);
    let fake = bins.path().join("dsh");
    let server = crate::tests::make_server();
    for (reason, exit, expected_state) in [
        ("completed", 0, "TASK_STATE_COMPLETED"),
        ("error", 0, "TASK_STATE_FAILED"),
        ("completed", 1, "TASK_STATE_FAILED"),
    ] {
        let script = format!(
            r#"#!/bin/sh
test "$1" = --profile && test "$2" = headless && test "$3" = --json && test "$4" = -- && test "$#" = 5 || exit 9
printf '%s\n' '{{"type":"session","sessionId":"fixture-session","cwd":"fixture"}}' '{{"type":"status","phase":"turn_end","reason":{{"kind":"{reason}"}}}}' '{{"type":"final","text":"3973"}}'
exit {exit}
"#
        );
        std::fs::write(&fake, script).expect("write fake DSH");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700))
            .expect("executable fake DSH");
        let mut request = tachi_params::StaffAssignmentRequest::new(
            tachi_params::TachiDispatchReason::ExplicitUserRequest,
            "compute 29 * 137",
        );
        request.profile = Some("dsh_executor".to_string());
        request.execution_level = Some(tachi_params::ExecutionLevel::L0);
        let (raw, assignment, _) = launch_staff_assignment(&server, request)
            .await
            .expect("Staff starts native DSH");
        let response: Value = serde_json::from_str(&raw).expect("start JSON");
        assert_eq!(assignment.selected_backend, "dsh");
        assert_eq!(
            response["authority"]["workspace_authority"],
            "workspace-write"
        );
        assert_eq!(response["authority"]["enforcement"]["mode"], "advisory");
        assert_eq!(response["execution_backend"], "dsh_headless");
        let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run directory"));
        let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
        let terminal = wait_for_terminal_status(&run_dir).await;
        assert_eq!(terminal["state"], expected_state, "{terminal}");
        assert_eq!(terminal["execution_backend"], "dsh_headless", "{terminal}");
        let output = std::fs::read_to_string(run_dir.join("result.md")).expect("collected result");
        if expected_state == "TASK_STATE_COMPLETED" {
            assert_eq!(output, "3973");
        }
        let events =
            std::fs::read_to_string(run_dir.join("dsh-events.jsonl")).expect("raw events retained");
        assert!(events.contains("fixture-session"));
        assert!(events.contains("\"type\":\"final\""));
        wait_for_dsh_cleanup(dispatch_id).await;
    }
}

async fn wait_for_dsh_cleanup(dispatch_id: &str) {
    for _ in 0..120 {
        if background_dispatch_cleanup_complete(dispatch_id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(background_dispatch_cleanup_complete(dispatch_id));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dsh_staff_stderr_only_failure_preserves_channel_identity() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("isolated Tachi home");
    let runs = tempfile::tempdir().expect("isolated runs");
    let bins = tempfile::tempdir().expect("fake DSH bin");
    let _home = EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let _host = EnvRestore::set("TACHI_HOST_PROFILE", "development");
    let _polls = EnvRestore::set("TACHI_DISPATCH_WATCHDOG_POLLS", "0");
    let mut entries = vec![bins.path().to_path_buf()];
    entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(entries).expect("fixture PATH");
    let _path = EnvRestore::set_os("PATH", &path);
    let fake = bins.path().join("dsh");
    std::fs::write(&fake, "#!/bin/sh\nprintf 'loader failure\\n' >&2\nexit 1\n")
        .expect("write fake DSH loader failure");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700))
        .expect("executable fake DSH");
    let server = crate::tests::make_server();
    let mut request = tachi_params::StaffAssignmentRequest::new(
        tachi_params::TachiDispatchReason::ExplicitUserRequest,
        "loader failure discriminator",
    );
    request.profile = Some("dsh_executor".to_string());
    request.execution_level = Some(tachi_params::ExecutionLevel::L0);
    let (raw, _, _) = launch_staff_assignment(&server, request)
        .await
        .expect("Staff starts fake DSH");
    let response: Value = serde_json::from_str(&raw).expect("start JSON");
    let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run directory"));
    let terminal = wait_for_terminal_status(&run_dir).await;
    assert_eq!(terminal["state"], "TASK_STATE_FAILED", "{terminal}");
    assert_eq!(
        std::fs::read_to_string(run_dir.join("dsh-events.jsonl")).expect("stdout evidence"),
        "",
        "stderr must not be falsely published as stdout JSONL"
    );
    assert_eq!(
        std::fs::read_to_string(run_dir.join("dsh-stderr.log")).expect("stderr evidence"),
        "loader failure\n"
    );
    assert!(std::fs::read_to_string(run_dir.join("result.md"))
        .expect("failure result")
        .contains("loader failure"));
    wait_for_dsh_cleanup(response["dispatch_id"].as_str().expect("dispatch id")).await;
}

#[cfg(any(
    target_os = "macos",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn dsh_required_postflight_publication_failure_fences_the_lease() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("isolated Tachi home");
    let runs = tempfile::tempdir().expect("isolated runs");
    let bins = tempfile::tempdir().expect("fake DSH bin");
    let control = tempfile::tempdir().expect("out-of-workspace worker barrier");
    let _home = EnvRestore::set_path("TACHI_HOME", home.path());
    let _runs = EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    let _host = EnvRestore::set("TACHI_HOST_PROFILE", "development");
    let _polls = EnvRestore::set("TACHI_DISPATCH_WATCHDOG_POLLS", "0");
    let mut entries = vec![bins.path().to_path_buf()];
    entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(entries).expect("fixture PATH");
    let _path = EnvRestore::set_os("PATH", &path);
    let fake = bins.path().join("dsh");
    let server = crate::tests::make_server();
    for fail_publication in [false, true] {
        let managed = tempfile::tempdir().expect("managed workspace");
        let managed_path = std::fs::canonicalize(managed.path()).expect("canonical workspace");
        let env_id = format!("dsh-publication-{fail_publication}");
        let resource_id = format!("dsh-resource-{fail_publication}");
        server
            .with_global_store(|store| {
                memcore::insert_exec_env(
                    store.connection(),
                    &memcore::NewExecEnvLease {
                        env_id: env_id.clone(),
                        kind: "worktree".to_string(),
                        path: managed_path.to_string_lossy().to_string(),
                        repo_root: managed_path.to_string_lossy().to_string(),
                        branch: env_id.clone(),
                        base_sha: "fixture".to_string(),
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
                        path: managed_path.to_string_lossy().to_string(),
                        bytes: None,
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                memcore::bind_resource(store.connection_mut(), &env_id, &resource_id)
                    .map_err(|error| error.to_string())
            })
            .expect("seed bound workspace and object identity");
        let release = control.path().join(format!("release-{fail_publication}"));
        let script = format!(
            r#"#!/bin/sh
n=0
while test ! -f '{}'; do
    n=$((n + 1))
    test "$n" -lt 400 || exit 30
    sleep 0.01
done
printf '%s\n' '{{"type":"session","sessionId":"publication-fixture","cwd":"fixture"}}' '{{"type":"status","phase":"turn_end","reason":{{"kind":"completed"}}}}' '{{"type":"final","text":"3973"}}'
"#,
            release.display()
        );
        std::fs::write(&fake, script).expect("write barrier fake DSH");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700))
            .expect("executable fake DSH");
        let mut request = tachi_params::StaffAssignmentRequest::new(
            tachi_params::TachiDispatchReason::ExplicitUserRequest,
            "compute without changing workspace",
        );
        request.profile = Some("dsh_executor".to_string());
        request.execution_level = Some(tachi_params::ExecutionLevel::L0);
        request.declared_file_scope = Some(vec!["allowed.txt".to_string()]);
        let mut start = resolve_staff_dispatch_start(
            &server,
            request,
            Utc::now(),
            tachi_params::ExecutionLevel::L0,
        )
        .expect("Staff profile admission");
        // The server fixture supplies the existing managed lease; Staff still
        // has no caller cwd/env knob. Grants are minted by the real kernel.
        start.mechanics.env_id = Some(env_id.clone());
        start.mechanics.timeout_secs = 5;
        let raw = launch_canonical_dispatch(&server, start, ManagedControlOrigin::StaffFacade)
            .await
            .expect("launch required postflight DSH");
        let response: Value = serde_json::from_str(&raw).expect("start JSON");
        let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run directory"));
        if fail_publication {
            // Deterministic owner-atomic rename failure, outside the measured
            // workspace and after the worker has been admitted.
            std::fs::create_dir(run_dir.join("dsh-events.jsonl"))
                .expect("block sidecar publication with a directory");
        }
        std::fs::write(&release, "release").expect("release fake worker");
        let terminal = wait_for_terminal_status(&run_dir).await;
        let (lease_state, resource_state) = server
            .with_global_store_read(|store| {
                let lease_state: String = store
                    .connection()
                    .query_row(
                        "SELECT state FROM exec_envs WHERE env_id = ?1",
                        [&env_id],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                let resource_state: String = store
                    .connection()
                    .query_row(
                        "SELECT state FROM exec_env_resources WHERE resource_id = ?1",
                        [&resource_id],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                Ok((lease_state, resource_state))
            })
            .expect("read canonical lease/resource ledger");
        assert_eq!(lease_state, "active", "{terminal}");
        if fail_publication {
            assert_eq!(terminal["state"], "TASK_STATE_FAILED", "{terminal}");
            assert_eq!(
                terminal["exec_env_postflight"]["verdict"], "error",
                "{terminal}"
            );
            assert_eq!(
                terminal["exec_env_postflight"]["artifacts"], "withheld",
                "{terminal}"
            );
            assert_eq!(
                terminal["exec_env_postflight"]["lease_action"], "quarantined",
                "{terminal}"
            );
            assert!(terminal["exec_env_postflight"]["error"]
                .as_str()
                .expect("publication error")
                .contains("DSH artifact publication failed"));
            assert!(!run_dir.join("result.md").exists());
            assert_eq!(resource_state, "quarantined");
            assert!(server
                .resolve_dispatch_env_binding(Some(&env_id), None, false)
                .is_err());
        } else {
            assert_eq!(terminal["state"], "TASK_STATE_COMPLETED", "{terminal}");
            assert_eq!(
                terminal["exec_env_postflight"]["verdict"], "clean",
                "{terminal}"
            );
            assert_eq!(
                terminal["exec_env_postflight"]["artifacts"], "released",
                "{terminal}"
            );
            assert_eq!(resource_state, "active");
            assert_eq!(
                std::fs::read_to_string(run_dir.join("result.md")).expect("released result"),
                "3973"
            );
            assert!(run_dir.join("dsh-events.jsonl").is_file());
            server
                .resolve_dispatch_env_binding(Some(&env_id), None, false)
                .expect("successful publication returns reusable lease");
        }
        wait_for_dsh_cleanup(response["dispatch_id"].as_str().expect("dispatch id")).await;
    }
}

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
        let run_dir = PathBuf::from(response["run_dir"].as_str().expect("run directory"));
        let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
        let terminal = wait_for_terminal_status(&run_dir).await;
        assert_eq!(terminal["state"], expected_state, "{terminal}");
        let output = std::fs::read_to_string(run_dir.join("result.md")).expect("collected result");
        if expected_state == "TASK_STATE_COMPLETED" {
            assert_eq!(output, "3973");
        }
        let events =
            std::fs::read_to_string(run_dir.join("dsh-events.jsonl")).expect("raw events retained");
        assert!(events.contains("fixture-session"));
        assert!(events.contains("\"type\":\"final\""));
        for _ in 0..120 {
            if background_dispatch_cleanup_complete(dispatch_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(background_dispatch_cleanup_complete(dispatch_id));
    }
}

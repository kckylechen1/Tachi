use super::*;
use crate::dispatch_ops::acp_native::{
    NativeAcpRunMode, NativeAcpRunSpec, run_native_acp_dispatch_with_liveness,
};
use crate::exec_env_postflight::RunnerLivenessEvidence;

#[tokio::test]
async fn required_postflight_macos_refuses_all_runners_before_launch() {
    for runner in ["agent", "managed", "opencode", "acp"] {
        let temp = tempfile::tempdir().expect("runner fixture");
        let marker = temp.path().join("child-launched");
        // The ordinary control proves the exact same command really executes
        // its side effect. Required must refuse without launching it at all.
        for required in [true, false] {
            let args = vec![
                "-c".to_string(),
                "printf launched > \"$1\"".to_string(),
                "probe".to_string(),
                marker.to_string_lossy().into_owned(),
            ];
            let mut cmd = Command::new("/bin/sh");
            cmd.args(&args);
            let timeout = Duration::from_secs(10);
            let outcome = match runner {
                "agent" => {
                    run_agent_subprocess_with_liveness(cmd, timeout, required, None).await
                }
                "opencode" => {
                    run_opencode_sop_subprocess_with_liveness(
                        cmd, timeout, "probe", required, None,
                    )
                    .await
                }
                "managed" => {
                    let (_sender, receiver) = mpsc::channel(1);
                    let outcome = run_managed_custom_subprocess_outcome(
                        cmd,
                        timeout,
                        receiver,
                        temp.path(),
                        required,
                        None,
                    )
                    .await;
                    assert!(outcome.cancellation.is_none());
                    assert!(outcome.termination_proof.is_none());
                    DispatchRunOutcome {
                        result: outcome.result,
                        liveness: outcome.liveness,
                        deferred_native_acp: None,
                    }
                }
                "acp" => {
                    run_native_acp_dispatch_with_liveness(
                        NativeAcpRunSpec {
                            command: "/bin/sh".to_string(),
                            args,
                            cwd: temp.path().to_path_buf(),
                            cwd_authority: None,
                            prompt: "probe".to_string(),
                            mode: NativeAcpRunMode::OneShot,
                            permission_label: "approve-reads".to_string(),
                            session: None,
                            session_record_path: None,
                            session_distill_path: None,
                            metadata: serde_json::json!({}),
                            env: Default::default(),
                            env_remove: Default::default(),
                        },
                        temp.path(),
                        &temp.path().join("trajectory.jsonl"),
                        "probe",
                        "probe",
                        timeout,
                        required,
                    )
                    .await
                }
                _ => unreachable!(),
            };
            if required {
                assert!(outcome.deferred_native_acp.is_none());
                assert_eq!(
                    outcome.result.unwrap_err(),
                    REQUIRED_POSTFLIGHT_UNSUPPORTED,
                    "{runner}"
                );
                assert!(matches!(
                    outcome.liveness,
                    RunnerLivenessEvidence::NoWorkerSpawned
                ));
                assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0, "{runner}");
            } else {
                // This shell is deliberately not an ACP protocol adapter;
                // ACP may reject its response, but must allow it to launch.
                if runner != "acp" {
                    assert_eq!(outcome.result.unwrap().exit_code, Some(0), "{runner}");
                }
                assert_eq!(
                    std::fs::read_to_string(&marker).unwrap(),
                    "launched",
                    "{runner}"
                );
            }
        }
    }
}

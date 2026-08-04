mod control;
mod events;
mod spec;
mod types;

pub(crate) use control::run_acpx_control_from_status;
pub(super) use events::persist_acpx_events_and_map;
pub(super) use spec::{
    build_acpx_command, build_acpx_command_spec, is_acpx_transport, prepare_acpx_prompt,
};

#[cfg(test)]
mod tests {
    use super::types::ACPX_EVENTS_FILE;
    use super::*;
    use crate::test_support::EnvRestore;
    use crate::tool_params::TachiDispatchParams;
    use serde_json::json;
    use std::path::Path;

    fn params() -> TachiDispatchParams {
        TachiDispatchParams {
            staffing_reason: tachi_params::TachiDispatchReason::ExplicitUserRequest,
            agent: Some("codex".to_string()),
            profile: None,
            task: "noop".to_string(),
            execution_level: None,
            cwd: Some("/tmp/project".to_string()),
            env_id: None,
            unmanaged_cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            completion_predicate: None,
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: Some("acpx".to_string()),
            harness_server_url: None,
            project: None,
            stage: None,
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            verbose: None,
            inject_card: None,
        }
    }

    #[test]
    fn acpx_command_spec_uses_file_prompt_and_read_approved_posture() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "python3");
        let _args = EnvRestore::set("TACHI_ACPX_ARGS", "-m acpx");
        let _agent = EnvRestore::remove("TACHI_ACPX_AGENT");
        let _mode = EnvRestore::remove("TACHI_ACPX_RUN_MODE");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");
        let params = params();
        let spec = build_acpx_command_spec(&params, "codex", Path::new("/tmp/run/prompt.md"))
            .expect("spec should build");

        assert_eq!(spec.command, "python3");
        assert!(spec.args.windows(2).any(|pair| pair == ["-m", "acpx"]));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--cwd", "/tmp/project"]));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--format", "json"]));
        assert!(spec.args.iter().any(|arg| arg == "--json-strict"));
        assert!(spec.args.iter().any(|arg| arg == "--approve-reads"));
        assert!(spec.args.windows(2).any(|pair| pair == ["codex", "exec"]));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--file", "/tmp/run/prompt.md"]));
        assert_eq!(spec.metadata["permissions"], json!("approve-reads"));
        assert_eq!(spec.metadata["mode"], json!("exec"));
        assert_eq!(spec.metadata["session"], json!(null));
        assert_eq!(spec.metadata["readiness"]["node"]["checked"], json!(false));
        assert_eq!(
            spec.metadata["controls"]["status"]["supported"],
            json!(false)
        );
    }

    #[test]
    fn acpx_session_mode_maps_card_hint_to_named_session_and_controls() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "python3");
        let _args = EnvRestore::set("TACHI_ACPX_ARGS", "-m acpx");
        let _agent = EnvRestore::remove("TACHI_ACPX_AGENT");
        let _mode = EnvRestore::set("TACHI_ACPX_RUN_MODE", "session");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");
        let mut params = params();
        params.stage = Some("review".to_string());
        let spec = build_acpx_command_spec(&params, "codex", Path::new("/tmp/run/prompt.md"))
            .expect("spec should build");

        assert!(spec.args.windows(2).any(|pair| pair == ["-s", "raven"]));
        assert!(!spec.args.windows(2).any(|pair| pair == ["codex", "exec"]));
        assert_eq!(spec.metadata["mode"], json!("session"));
        assert_eq!(spec.metadata["session"], json!("raven"));
        assert_eq!(spec.metadata["session_source"], json!("stage"));
        assert_eq!(
            spec.metadata["controls"]["status"]["supported"],
            json!(true)
        );
        assert!(spec.metadata["controls"]["status"]["argv"]
            .as_array()
            .expect("status argv")
            .iter()
            .any(|arg| arg.as_str() == Some("status")));
        assert!(spec.metadata["controls"]["cancel"]["argv"]
            .as_array()
            .expect("cancel argv")
            .iter()
            .any(|arg| arg.as_str() == Some("cancel")));
    }

    /// The other input `derive_acpx_session` accepts: the dispatch **profile**.
    /// `codex_55_review` -> `raven`, sourced `dispatch_profile`.
    ///
    /// This used to be covered only end-to-end, by the acpx-session dispatch test.
    /// Since #894 S2d that lane cannot be dispatched at all (a read-only profile
    /// over acpx — no sandbox primitive, unattended shell, nobody enforcing the
    /// read-only claim — is refused pre-spawn by authority invariant 4), so
    /// `dispatch_acpx_session_mode_derives_raven_for_a_review_lane` now declares
    /// its review lane with `stage`. Session derivation from a profile is a pure
    /// function and needs no dispatch: it is pinned here rather than quietly
    /// dropped.
    #[test]
    fn acpx_session_derives_raven_from_a_review_dispatch_profile() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "python3");
        let _args = EnvRestore::set("TACHI_ACPX_ARGS", "-m acpx");
        let _agent = EnvRestore::remove("TACHI_ACPX_AGENT");
        let _mode = EnvRestore::set("TACHI_ACPX_RUN_MODE", "session");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");
        let mut params = params();
        params.profile = Some("codex_55_review".to_string());
        let spec = build_acpx_command_spec(&params, "codex", Path::new("/tmp/run/prompt.md"))
            .expect("spec should build");

        assert!(spec.args.windows(2).any(|pair| pair == ["-s", "raven"]));
        assert_eq!(spec.metadata["session"], json!("raven"));
        assert_eq!(spec.metadata["session_source"], json!("dispatch_profile"));
    }

    #[test]
    fn acpx_rejects_full_permission_profile() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "python3");
        let _mode = EnvRestore::remove("TACHI_ACPX_RUN_MODE");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");
        let mut params = params();
        params.permission_profile = Some("full".to_string());
        let _allow = EnvRestore::set("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE", "true");
        let err = build_acpx_command_spec(&params, "codex", Path::new("/tmp/run/prompt.md"))
            .expect_err("full should not map to acpx approve-all");
        assert!(err.contains("never maps acpx to --approve-all"), "{err}");
    }

    #[test]
    fn acpx_fails_closed_on_sandbox_request() {
        // acpx has no `--sandbox`-equivalent knob; a caller-supplied sandbox
        // request must fail closed with a receipt naming the backend and the
        // requested level, never be silently dropped (#894 S0).
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "python3");
        let _args = EnvRestore::set("TACHI_ACPX_ARGS", "-m acpx");
        let _agent = EnvRestore::remove("TACHI_ACPX_AGENT");
        let _mode = EnvRestore::remove("TACHI_ACPX_RUN_MODE");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");
        let mut params = params();
        params.sandbox = Some("workspace-write".to_string());

        let err = build_acpx_command_spec(&params, "codex", Path::new("/tmp/run/prompt.md"))
            .expect_err("acpx has no sandbox concept and must fail closed");
        assert!(
            err.contains("acpx") && err.contains("workspace-write"),
            "receipt must name backend + requested level: {err}"
        );
        assert!(err.contains("fail-closed"), "{err}");
    }

    /// #894 S0 round 2: the sandbox rejection must run BEFORE the node
    /// readiness preflight (a real subprocess spawn), not after. The prior
    /// version of this test used `TACHI_ACPX_COMMAND=python3`, which the node
    /// check skips entirely (`should_check_node_for_acpx` only fires for
    /// `acpx`/`npx`/`node`) — so it never actually exercised the ordering.
    /// This version points at a real `node`-named binary that, if invoked,
    /// writes a marker file; asserting the marker is absent after the call
    /// proves the preflight subprocess never ran.
    #[cfg(unix)]
    #[test]
    fn acpx_sandbox_rejection_runs_before_node_preflight_spawn() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_path = tempfile::tempdir().expect("fake node PATH dir");
        let marker_path = temp_path.path().join("node-was-invoked.marker");
        let node_path = temp_path.path().join("node");
        // If this script ever runs, it proves the node preflight fired before
        // the sandbox check — which would be the regression this test guards
        // against.
        std::fs::write(
            &node_path,
            format!(
                "#!/bin/sh\ntouch '{}'\nprintf 'v22.13.0\\n'\n",
                marker_path.display()
            ),
        )
        .expect("fake node");
        let mut perms = std::fs::metadata(&node_path)
            .expect("fake node metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&node_path, perms).expect("fake node executable");

        let path_value = temp_path.path().to_string_lossy().to_string();
        let _path = EnvRestore::set("PATH", &path_value);
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "node");
        let _args = EnvRestore::remove("TACHI_ACPX_ARGS");
        let _agent = EnvRestore::remove("TACHI_ACPX_AGENT");
        let _mode = EnvRestore::remove("TACHI_ACPX_RUN_MODE");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");
        let mut params = params();
        params.sandbox = Some("workspace-write".to_string());

        let err = build_acpx_command_spec(&params, "codex", Path::new("/tmp/run/prompt.md"))
            .expect_err("acpx has no sandbox concept and must fail closed");

        assert!(
            err.contains("acpx") && err.contains("workspace-write"),
            "receipt must name backend + requested level: {err}"
        );
        assert!(
            !marker_path.exists(),
            "node preflight must never run when the sandbox check rejects first"
        );
    }

    #[test]
    fn acpx_missing_command_error_is_actionable() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_path = tempfile::tempdir().expect("empty PATH dir");
        let path_value = temp_path.path().to_string_lossy().to_string();
        let _path = EnvRestore::set("PATH", &path_value);
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "acpx");
        let _args = EnvRestore::remove("TACHI_ACPX_ARGS");
        let _mode = EnvRestore::remove("TACHI_ACPX_RUN_MODE");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");

        let err = build_acpx_command_spec(&params(), "codex", Path::new("/tmp/run/prompt.md"))
            .expect_err("missing acpx command should fail before dispatch");

        assert!(
            err.contains("command 'acpx' was not found on PATH"),
            "{err}"
        );
        assert!(err.contains("Install acpx"), "{err}");
        assert!(err.contains("TACHI_ACPX_COMMAND=npx"), "{err}");
        assert!(
            err.contains("TACHI_ACPX_ARGS='-y acpx@<tested-version>'"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn acpx_rejects_unsupported_node_runtime_with_upgrade_guidance() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_path = tempfile::tempdir().expect("fake node PATH dir");
        let node_path = temp_path.path().join("node");
        std::fs::write(&node_path, "#!/bin/sh\nprintf 'v20.12.0\\n'\n").expect("fake node");
        let mut perms = std::fs::metadata(&node_path)
            .expect("fake node metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&node_path, perms).expect("fake node executable");

        let path_value = temp_path.path().to_string_lossy().to_string();
        let _path = EnvRestore::set("PATH", &path_value);
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "node");
        let _args = EnvRestore::remove("TACHI_ACPX_ARGS");
        let _mode = EnvRestore::remove("TACHI_ACPX_RUN_MODE");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");

        let err = build_acpx_command_spec(&params(), "codex", Path::new("/tmp/run/prompt.md"))
            .expect_err("unsupported Node should fail before dispatch");

        assert!(err.contains("requires Node >=22.13.0"), "{err}");
        assert!(err.contains("found v20.12.0"), "{err}");
        assert!(err.contains("Upgrade Node"), "{err}");
        assert!(
            err.contains("set TACHI_ACPX_COMMAND to a compatible tested acpx wrapper"),
            "{err}"
        );
    }

    #[test]
    fn acpx_event_mapping_persists_raw_events_and_extracts_final_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trajectory = dir.path().join("trajectory.jsonl");
        crate::utils::write_owner_only_file_atomic(&trajectory, b"").expect("trajectory");
        let output = r#"{"type":"message","message":"working"}
{"event":"tool_call_start","tool_name":"bash"}
{"event":"end_turn","final_response":"done"}
"#;
        let summary =
            persist_acpx_events_and_map(dir.path(), &trajectory, "dispatch-1", "codex", output)
                .expect("events should persist");
        assert_eq!(summary.mapped_events, 3);
        assert_eq!(summary.final_response.as_deref(), Some("done"));
        assert!(summary.events_file.ends_with(ACPX_EVENTS_FILE));
        let raw = std::fs::read_to_string(summary.events_file).expect("raw events");
        assert!(raw.contains("tool_call_start"));
        let progress =
            std::fs::read_to_string(dir.path().join("progress.jsonl")).expect("progress events");
        assert!(progress.contains("acpx_message"));
        let trajectory = std::fs::read_to_string(trajectory).expect("trajectory events");
        assert!(trajectory.contains("acpx_tool_event"));
        assert!(trajectory.contains("acpx_final_event"));
    }
}

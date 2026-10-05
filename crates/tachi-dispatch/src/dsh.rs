//! DeepSeek Harness one-shot carrier. The DSH profile owns model/auth settings;
//! Tachi owns the executable, arguments and admitted working directory.

use crate::{reject_unsupported_sandbox, DispatchLaunchParams, LaunchCommand};
use std::path::PathBuf;

pub fn build_dsh_launch(
    params: &DispatchLaunchParams,
    prompt: &str,
) -> Result<LaunchCommand, String> {
    reject_unsupported_sandbox("dsh", params.sandbox.as_deref())?;
    if !matches!(params.permission_profile.as_deref(), None | Some("default"))
        || !params.allowed_tools.is_empty()
    {
        return Err("dsh supports only permission_profile='default'; tool allowlists and permission bypasses are not supported".to_string());
    }
    if params.max_turns.is_some() {
        return Err("dsh headless does not support a Tachi max_turns grant".to_string());
    }
    if params.model.is_some() || !params.command.is_empty() {
        return Err("dsh launch command and model are host-owned; command/model overrides are not supported".to_string());
    }
    if params
        .cwd
        .as_deref()
        .is_some_and(|cwd| cwd.trim().is_empty())
    {
        return Err("dsh working directory grant is blank".to_string());
    }
    Ok(LaunchCommand {
        program: "dsh".to_string(),
        // `--` prevents a task beginning with --session-id/--json from becoming
        // carrier options. No shell interpolation or session adoption occurs.
        args: vec![
            "--profile".to_string(),
            "headless".to_string(),
            "--json".to_string(),
            "--".to_string(),
            prompt.to_string(),
        ],
        // None is the canonical Default grant: inherit the server's cwd, just
        // like the existing subprocess carriers. Do not manufacture a path or
        // pretend Default is a managed lease. A present cwd comes from the
        // grant (including '.' when the process cwd is descriptor-bound).
        current_dir: params.cwd.as_ref().map(PathBuf::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> DispatchLaunchParams {
        DispatchLaunchParams {
            cwd: Some("/admitted/workspace".to_string()),
            ..DispatchLaunchParams::default()
        }
    }

    #[test]
    fn dsh_default_grant_preserves_absent_working_directory() {
        let launch = build_dsh_launch(&DispatchLaunchParams::default(), "task")
            .expect("canonical Default grant");
        assert_eq!(launch.current_dir, None);
    }

    #[test]
    fn dsh_launch_is_fixed_and_preserves_task_as_one_positional_argument() {
        let prompt = "--session-id other; $(touch /tmp/never)\nanswer 6 * 7";
        let launch = build_dsh_launch(&params(), prompt).expect("default DSH launch");
        assert_eq!(launch.program, "dsh");
        assert_eq!(
            launch.args,
            ["--profile", "headless", "--json", "--", prompt]
        );
        assert_eq!(launch.current_dir, Some("/admitted/workspace".into()));
    }

    #[test]
    fn dsh_launch_refuses_unrepresentable_authority_and_host_overrides() {
        for sandbox in ["read-only", "workspace-write", "danger-full-access", " "] {
            let mut input = params();
            input.sandbox = Some(sandbox.to_string());
            assert!(build_dsh_launch(&input, "task").is_err());
        }
        for permission in ["allowlist", "full", "verify", "unknown"] {
            let mut input = params();
            input.permission_profile = Some(permission.to_string());
            assert!(build_dsh_launch(&input, "task").is_err());
        }
        for input in [
            DispatchLaunchParams {
                allowed_tools: vec!["Read".to_string()],
                ..params()
            },
            DispatchLaunchParams {
                max_turns: Some(1),
                ..params()
            },
            DispatchLaunchParams {
                model: Some("deepseek-v4-flash".to_string()),
                ..params()
            },
            DispatchLaunchParams {
                command: vec!["sh".to_string()],
                ..params()
            },
            DispatchLaunchParams {
                cwd: Some(" ".to_string()),
                ..params()
            },
        ] {
            assert!(build_dsh_launch(&input, "task").is_err());
        }
    }
}

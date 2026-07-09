use crate::tool_params::TachiDispatchParams;
use std::path::PathBuf;
use tachi_dispatch::{
    build_claude_launch, build_codex_launch, build_custom_launch, build_grok_launch,
    build_kimi_launch, DispatchLaunchParams, LaunchCommand,
};
use tokio::process::Command;

fn launch_params(params: &TachiDispatchParams) -> DispatchLaunchParams {
    DispatchLaunchParams {
        cwd: params.cwd.clone(),
        model: params.model.clone(),
        permission_profile: params.permission_profile.clone(),
        allowed_tools: params.allowed_tools.clone(),
        max_turns: params.max_turns,
        sandbox: params.sandbox.clone(),
        command: params.command.clone(),
    }
}

fn command_from_launch(spec: LaunchCommand) -> Command {
    let mut cmd = Command::new(spec.program);
    cmd.args(spec.args);
    if let Some(cwd) = spec.current_dir {
        cmd.current_dir(cwd);
    }
    cmd
}

pub(super) fn resolve_permission_profile(params: &TachiDispatchParams) -> Result<&str, String> {
    let resolved = tachi_dispatch::resolve_permission_profile(&launch_params(params))?;
    Ok(resolved.as_str())
}

pub(super) fn build_claude_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    build_claude_launch(
        &launch_params(params),
        prompt,
        mcp_config_path.map(PathBuf::as_path),
    )
    .map(command_from_launch)
}

pub(super) fn build_codex_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    build_codex_launch(
        &launch_params(params),
        prompt,
        mcp_config_path.map(PathBuf::as_path),
    )
    .map(command_from_launch)
}

pub(super) fn build_grok_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    build_grok_launch(
        &launch_params(params),
        prompt,
        mcp_config_path.map(PathBuf::as_path),
    )
    .map(command_from_launch)
}

pub(super) fn build_kimi_command(
    params: &TachiDispatchParams,
    prompt: &str,
) -> Result<Command, String> {
    build_kimi_launch(&launch_params(params), prompt).map(command_from_launch)
}

pub(super) fn build_custom_command(
    params: &TachiDispatchParams,
    prompt: &str,
) -> Result<Command, String> {
    build_custom_launch(&launch_params(params), prompt).map(command_from_launch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch_params(agent: &str) -> TachiDispatchParams {
        TachiDispatchParams {
            agent: Some(agent.to_string()),
            profile: None,
            task: "noop".to_string(),
            cwd: None,
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
            harness_transport: None,
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
        }
    }

    fn command_args(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn claude_adapter_preserves_allowlist_and_mcp_config_order() {
        let mut params = dispatch_params("claude");
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string(), "Bash(git status*)".to_string()];
        params.max_turns = Some(3);
        params.model = Some("sonnet".to_string());
        params.cwd = Some("/work/repo".to_string());
        let mcp_path = PathBuf::from("/tmp/tachi-mcp.json");

        let cmd =
            build_claude_command(&params, "inspect", Some(&mcp_path)).expect("claude command");
        let args = command_args(&cmd);

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "Read"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "Bash(git status*)"]));
        assert!(args.windows(2).any(|pair| pair == ["--max-turns", "3"]));
        assert!(args.windows(2).any(|pair| pair == ["--model", "sonnet"]));
        let prompt_pos = args.iter().position(|arg| arg == "inspect").unwrap();
        let mcp_pos = args.iter().position(|arg| arg == "--mcp-config").unwrap();
        assert!(
            prompt_pos < mcp_pos,
            "prompt must precede --mcp-config: {args:?}"
        );
        assert_eq!(
            cmd.as_std().get_current_dir(),
            Some(std::path::Path::new("/work/repo"))
        );
    }

    #[test]
    fn codex_adapter_preserves_sandbox_turns_model_and_cwd_arg() {
        let mut params = dispatch_params("codex");
        params.cwd = Some("/work/repo".to_string());
        params.sandbox = Some("read-only".to_string());
        params.max_turns = Some(4);
        params.model = Some("gpt-5-codex".to_string());

        let cmd = build_codex_command(&params, "fix it", None).expect("codex command");
        let args = command_args(&cmd);

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "read-only"]));
        assert!(args.windows(2).any(|pair| pair == ["-c", "max_turns=4"]));
        assert!(args.windows(2).any(|pair| pair == ["-m", "gpt-5-codex"]));
        assert!(args.windows(2).any(|pair| pair == ["-C", "/work/repo"]));
    }

    #[test]
    fn custom_adapter_uses_dispatch_trust_policy() {
        let mut params = dispatch_params("custom");
        params.command = vec![
            "python3".to_string(),
            "-m".to_string(),
            "worker".to_string(),
        ];

        let cmd = build_custom_command(&params, "run task").expect("custom command");
        assert_eq!(command_args(&cmd), vec!["-m", "worker", "run task"]);

        params.command = vec!["/tmp/python3".to_string()];
        assert!(
            build_custom_command(&params, "run task").is_err(),
            "trusted basenames should not bless untrusted paths"
        );
    }
}

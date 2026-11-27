//! The Claude CLI lane, run against a stand-in `claude` first on `PATH`.

use super::*;
#[cfg(unix)]
use std::path::PathBuf;

/// The stand-in's directory. Dropped when the test ends.
#[cfg(unix)]
struct StandIn(tempfile::TempDir);

#[cfg(unix)]
impl StandIn {
    fn helper_pid(&self) -> PathBuf {
        self.0.path().join("helper.pid")
    }
}

/// Puts a stand-in `claude` first on `PATH`, calls it with `user` under
/// `limit`, and returns what the lane got back.
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn call_stand_in(
    body: &str,
    user: &str,
    limit: Duration,
) -> (Result<String, String>, StandIn) {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("stand-in directory");
    let script = dir.path().join("claude");
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).expect("write the stand-in");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in executable");

    let _guard = crate::test_support::global_test_lock().lock();
    let mut path = dir.path().as_os_str().to_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let _path = EnvRestore::set("PATH", path);

    let result = LlmClient::call_claude_cli_within("system", user, limit).await;
    (result, StandIn(dir))
}

/// Polls until no process answers to `pid`, or gives up after `wait`.
#[cfg(unix)]
async fn process_stays_alive(pid: i32, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        let alive = match std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .status()
        {
            Ok(status) => status.success(),
            Err(_) => false,
        };
        if !alive || Instant::now() >= deadline {
            return alive;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Four hundred KiB is many pipe buffers. The prompt cannot be written until
/// the CLI reads it, and this CLI does not read until its output is drained.
#[cfg(unix)]
#[tokio::test]
async fn a_large_prompt_reaches_a_cli_that_talks_before_it_listens() {
    let user = "p".repeat(400 * 1024);
    let (result, _stand_in) = tokio::time::timeout(
        Duration::from_secs(20),
        call_stand_in(
            "yes x | head -c 200000\ncat > /dev/null\nprintf DONE",
            &user,
            Duration::from_secs(60),
        ),
    )
    .await
    .expect("the prompt write must progress while the CLI's output is drained");
    let text = result.expect("the CLI answers once its prompt is in");
    assert_eq!(text.len(), 200_004, "{text}");
    assert!(text.ends_with("DONE"));
}

/// The deadline covers a CLI that never finishes, and the CLI's helpers do
/// not outlive the call.
#[cfg(unix)]
#[tokio::test]
async fn a_cli_that_never_finishes_is_taken_down_with_its_helpers() {
    let (result, stand_in) = call_stand_in(
        "cat > /dev/null\n\
         sleep 600 &\n\
         echo $! > \"$(dirname \"$0\")/helper.pid\"\n\
         sleep 600",
        "prompt",
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(
        result,
        Err("claude cli timeout after 5 minutes".to_string())
    );
    let helper: i32 = std::fs::read_to_string(stand_in.helper_pid())
        .expect("the stand-in records its helper")
        .trim()
        .parse()
        .expect("a pid");
    let alive = process_stays_alive(helper, Duration::from_secs(10)).await;
    if alive {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-9", &helper.to_string()])
            .status();
    }
    assert!(!alive, "the CLI's helper {helper} outlived the call");
}

#[cfg(unix)]
#[tokio::test]
async fn claude_cli_disables_tools_mcp_and_project_settings_and_preserves_stderr_on_early_exit() {
    let args_dir = tempfile::tempdir().expect("args directory");
    let args_path = args_dir.path().join("args.txt");
    let user = "p".repeat(400 * 1024);
    let body = format!(
        "printf '%s\\n' \"$@\" > \"{}\"\n\
         echo 'auth failed on server' >&2\n\
         exit 23",
        args_path.display()
    );
    let (result, _stand_in) = call_stand_in(&body, &user, Duration::from_secs(10)).await;
    let err = result.expect_err("non-zero CLI exit must fail");
    assert!(
        err.contains("claude cli exited 23: auth failed on server"),
        "expected CLI exit status and stderr rather than BrokenPipe, got: {err}"
    );
    let args: Vec<String> = std::fs::read_to_string(&args_path)
        .expect("stand-in wrote its args")
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        args.windows(2).any(|w| w == ["--tools", ""]),
        "expected --tools \"\" in {args:?}"
    );
    assert!(
        args.iter().any(|arg| arg == "--strict-mcp-config"),
        "expected --strict-mcp-config in {args:?}"
    );
    assert!(
        args.windows(2).any(|w| w == ["--setting-sources", "user"]),
        "expected --setting-sources user in {args:?}"
    );
}

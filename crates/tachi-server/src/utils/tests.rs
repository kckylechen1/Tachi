use super::file::MAX_APPEND_ONLY_JSONL_LINE_BYTES;
use super::*;
use crate::test_support::EnvRestore;
use serde_json::json;
use std::path::{Path, PathBuf};

struct CwdGuard {
    original: PathBuf,
}

impl CwdGuard {
    fn set(path: &Path) -> Self {
        let original = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(path).expect("set cwd");
        Self { original }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original).expect("restore cwd");
    }
}

fn make_git_root(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");
    root
}

fn clear_workspace_root_env() -> Vec<EnvRestore> {
    vec![
        EnvRestore::remove("TACHI_PROJECT_ROOT"),
        EnvRestore::remove("TACHI_WORKSPACE_ROOT"),
        EnvRestore::remove("PROJECT_ROOT"),
        EnvRestore::remove("WORKSPACE_ROOT"),
        EnvRestore::remove("WORKSPACE"),
        EnvRestore::remove("PWD"),
    ]
}

#[test]
fn find_project_git_root_prefers_real_cwd_over_stale_pwd() {
    let _lock = lock_or_recover(global_test_lock(), "global test lock");
    let _env = clear_workspace_root_env();
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd_root = make_git_root(dir.path(), "cwd-repo");
    let pwd_root = make_git_root(dir.path(), "stale-pwd-repo");
    let nested = cwd_root.join("nested");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    let _cwd = CwdGuard::set(&nested);
    let _pwd = EnvRestore::set_path("PWD", &pwd_root);

    let resolved = find_project_git_root().expect("resolve project root");

    assert_eq!(
        resolved,
        cwd_root.canonicalize().expect("canonical cwd root")
    );
}

#[test]
fn find_project_git_root_prefers_explicit_tachi_root_over_cwd_and_pwd() {
    let _lock = lock_or_recover(global_test_lock(), "global test lock");
    let _env = clear_workspace_root_env();
    let dir = tempfile::tempdir().expect("tempdir");
    let explicit_root = make_git_root(dir.path(), "explicit-repo");
    let cwd_root = make_git_root(dir.path(), "cwd-repo");
    let pwd_root = make_git_root(dir.path(), "stale-pwd-repo");
    let _cwd = CwdGuard::set(&cwd_root);
    let _project = EnvRestore::set_path("TACHI_PROJECT_ROOT", &explicit_root);
    let _pwd = EnvRestore::set_path("PWD", &pwd_root);

    let resolved = find_project_git_root().expect("resolve project root");

    assert_eq!(
        resolved,
        explicit_root
            .canonicalize()
            .expect("canonical explicit root")
    );
}

#[test]
fn find_git_root_from_linked_worktree_resolves_primary_checkout_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    let primary = make_git_root(dir.path(), "primary");
    let linked = dir.path().join("linked");
    std::fs::create_dir_all(&linked).expect("create linked worktree");
    let worktree_git_dir = primary.join(".git/worktrees/linked");
    std::fs::create_dir_all(&worktree_git_dir).expect("create worktree git dir");
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", worktree_git_dir.display()),
    )
    .expect("write linked .git file");
    std::fs::write(worktree_git_dir.join("commondir"), "../..").expect("write commondir");

    let resolved = find_git_root_from(&linked).expect("resolve linked worktree root");

    assert_eq!(resolved, primary.canonicalize().expect("canonical primary"));
}

#[test]
fn append_owner_only_jsonl_line_caps_size_and_restricts_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("events.jsonl");

    append_owner_only_jsonl_line(&path, r#"{"event":"ok"}"#).expect("append event");
    let raw = std::fs::read_to_string(&path).expect("read events");
    assert_eq!(raw, "{\"event\":\"ok\"}\n");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let huge = "x".repeat(MAX_APPEND_ONLY_JSONL_LINE_BYTES);
    let err = append_owner_only_jsonl_line(&path, &huge).expect_err("huge line should fail");
    assert!(
        err.contains("byte cap"),
        "expected JSONL line cap error, got: {err}"
    );
}

#[test]
fn compact_text_line_respects_limit_with_ellipsis() {
    assert_eq!(compact_text_line("hello world", 20), "hello world");
    assert_eq!(
        compact_text_line("one two three four five", 10),
        "one two..."
    );
}

#[test]
fn skill_prompt_template_renders_safe_args_only() {
    let args = serde_json::Map::from_iter([
        ("name".to_string(), json!("Ada")),
        ("input".to_string(), json!("review this")),
    ]);

    let rendered =
        render_skill_prompt_template("Name={{name}}\nInput={{input}}\nAll={{args_json}}", &args)
            .expect("render prompt");

    assert!(rendered.contains("Name=Ada"));
    assert!(rendered.contains("Input=review this"));
    assert!(rendered.contains("\"name\":\"Ada\""));
    assert!(!rendered.contains("\n  \"name\""));
}

#[test]
fn skill_prompt_template_ignores_reserved_and_unsafe_arg_keys() {
    let args = serde_json::Map::from_iter([
        ("args_json".to_string(), json!("replace all args")),
        ("input".to_string(), json!("safe input value")),
        ("name}} {{args_json".to_string(), json!("injected")),
        ("unsafe.key".to_string(), json!("dot value")),
    ]);

    let rendered = render_skill_prompt_template(
        "Args={{args_json}}\nInput={{input}}\nBad={{name}} {{args_json}}\nDot={{unsafe.key}}",
        &args,
    )
    .expect("render prompt");

    assert!(rendered.contains("Input=safe input value"));
    assert!(rendered.contains("\"args_json\":\"replace all args\""));
    assert!(rendered.contains("Bad={{name}}"));
    assert!(rendered.contains("Dot={{unsafe.key}}"));
    assert!(
        !rendered.contains("Bad=injected"),
        "unsafe keys must not synthesize template placeholders: {rendered}"
    );
}

#[test]
fn redact_sensitive_value_redacts_secret_keys_and_url_params() {
    let mut value = json!({
        "definition": {
            "url": "https://example.test/mcp?tavilyApiKey=abc123&safe=ok",
            "headers": {
                "Authorization": "Bearer secret-token"
            }
        }
    });

    redact_sensitive_value(&mut value);

    assert_eq!(
        value["definition"]["url"],
        json!("https://example.test/mcp?tavilyApiKey=[REDACTED]&safe=ok")
    );
    assert_eq!(
        value["definition"]["headers"]["Authorization"],
        json!("[REDACTED]")
    );
}

#[test]
fn write_owner_only_file_atomic_replaces_file_without_temp_leftovers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("status.json");
    std::fs::write(&path, b"old").expect("seed old file");

    write_owner_only_file_atomic(&path, br#"{"state":"ok"}"#).expect("atomic write");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read replaced file"),
        r#"{"state":"ok"}"#
    );
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("status.json.tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "atomic write should clean temp files: {leftovers:?}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn write_json_file_owner_only_pretty_prints_and_restricts_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested/overlay/manifest.json");

    write_json_file_owner_only(&path, &json!({"state":"ok"})).expect("write json");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read json"),
        "{\n  \"state\": \"ok\"\n}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn read_to_string_allow_missing_distinguishes_missing_from_io_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.md");
    assert_eq!(
        read_to_string_allow_missing(&missing, "test artifact").expect("missing is ok"),
        None
    );

    let readable = dir.path().join("result.md");
    std::fs::write(&readable, "done").expect("write artifact");
    assert_eq!(
        read_to_string_allow_missing(&readable, "test artifact").expect("readable file"),
        Some("done".to_string())
    );

    let unreadable = dir.path().join("unreadable");
    std::fs::create_dir(&unreadable).expect("create unreadable directory");
    let err = read_to_string_allow_missing(&unreadable, "test artifact").expect_err("dir read");
    assert!(err.contains("test artifact"), "{err}");
    assert!(err.contains("unreadable"), "{err}");
}

#[test]
fn shared_env_name_validator_matches_shell_identifier_rules() {
    assert!(is_shell_env_name("OPENAI_API_KEY"));
    assert!(is_shell_env_name("_TACHI"));
    assert!(!is_shell_env_name("1_BAD"));
    assert!(!is_shell_env_name("BAD-NAME"));
    assert!(!is_shell_env_name(""));
}

#[test]
fn mcp_interpreters_are_not_auto_approved() {
    let test_cmds = [
        "python3",
        "python",
        "node",
        "npx",
        "bun",
        "deno",
        "uv",
        "cargo",
        "rustup",
        "python3.12",
        "node20",
        "bash",
        "sh",
        "zsh",
        "ruby",
        "perl",
        "php",
    ];
    for cmd in test_cmds {
        assert!(
            !is_trusted_mcp_command(cmd),
            "{cmd} should require capability-level approval for MCP"
        );
        assert!(
            !is_trusted_mcp_command(&format!("/usr/local/bin/{cmd}")),
            "absolute {cmd} should also be rejected for MCP"
        );
    }
}

#[test]
fn dispatch_interpreters_remain_trusted_for_caller_supplied_commands() {
    assert!(is_trusted_command("python3"));
    assert!(is_trusted_command("/usr/local/bin/node"));
    assert!(is_trusted_command("acpx"));
}

#[test]
fn mcp_container_and_platform_runtimes_remain_trusted() {
    for cmd in ["docker", "podman", "tachi", "opencode"] {
        assert!(
            is_trusted_mcp_command(cmd),
            "{cmd} should be trusted by basename for MCP"
        );
    }
}

#[test]
fn mcp_trusted_prefixes_still_allow_non_interpreters() {
    assert!(is_trusted_mcp_command("/opt/homebrew/bin/opencode"));
    assert!(!is_trusted_mcp_command("/usr/bin/python3"));
    assert!(!is_trusted_mcp_command("/bin/bash"));
}

use super::binary::{resolve_claude_binary, validate_claude_binary_override};
use super::cleanup::cleanup_runs_dir;
use super::envelope::parse_claude_json_envelope;
use super::fallback::format_pool_prompt;
use super::files::sanitize_label;
use super::*;

#[test]
fn sanitize_label_replaces_unsafe_chars() {
    assert_eq!(sanitize_label("ok-label_1"), "ok-label_1");
    assert_eq!(sanitize_label("a/b c"), "a_b_c");
    assert_eq!(sanitize_label(""), "run");
    // Trim leading/trailing underscores.
    assert_eq!(sanitize_label("///"), "run");
    // Long labels capped at 48 chars.
    let long = "x".repeat(80);
    assert_eq!(sanitize_label(&long).len(), 48);
}

#[test]
fn sanitize_label_distinguishes_long_labels_with_shared_prefix() {
    let common_prefix = "a".repeat(60);
    let label_a = format!("{common_prefix}-suffix-A");
    let label_b = format!("{common_prefix}-suffix-B");

    let safe_a = sanitize_label(&label_a);
    let safe_b = sanitize_label(&label_b);

    assert_eq!(safe_a.len(), 48);
    assert_eq!(safe_b.len(), 48);
    assert_ne!(
        safe_a, safe_b,
        "distinct labels sharing a 48-char prefix must not collide"
    );
    let prefix_len = 39;
    assert_eq!(&safe_a[..prefix_len], &safe_b[..prefix_len]);
    assert_ne!(&safe_a[prefix_len..], &safe_b[prefix_len..]);
}

#[test]
fn sanitize_label_truncates_long_prefix_at_segment_boundary() {
    let safe = sanitize_label("issue-501-architecture-review-backlog-2026-07-05-consolidated-plan");

    assert!(safe.len() <= 48);
    assert!(
        safe.starts_with("issue-501-architecture-review-backlog-"),
        "expected readable whole-segment prefix, got {safe}"
    );
    assert!(
        !safe.starts_with("issue-501-architecture-review-backlog-2-"),
        "prefix must not hard-cut mid-segment before hash: {safe}"
    );
}

#[test]
fn claude_binary_override_accepts_only_claude_executables() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_path = tmp.path().join("claude");
    std::fs::write(&claude_path, "#!/bin/sh\nexit 0\n").unwrap();
    let claude_beta_path = tmp.path().join("claude-beta");
    std::fs::write(&claude_beta_path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&claude_beta_path, std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    assert_eq!(validate_claude_binary_override("claude").unwrap(), "claude");
    assert_eq!(
        validate_claude_binary_override(claude_path.to_str().unwrap()).unwrap(),
        claude_path.to_str().unwrap()
    );
    assert_eq!(
        validate_claude_binary_override(claude_beta_path.to_str().unwrap()).unwrap(),
        claude_beta_path.to_str().unwrap()
    );

    for bad in [
        "",
        "claude --dangerously-skip-permissions",
        "./claude",
        "/tmp/not-claude",
        "/tmp/../tmp/claude",
        "/nonexistent/claude",
    ] {
        assert!(
            validate_claude_binary_override(bad).is_err(),
            "expected invalid override: {bad}"
        );
    }
}

#[test]
fn resolve_claude_binary_reports_invalid_override() {
    let prev = std::env::var("CLAUDE_BIN").ok();
    std::env::set_var("CLAUDE_BIN", "/nonexistent/claude");

    let err = resolve_claude_binary().expect_err("invalid override should be surfaced");
    assert!(
        err.contains("existing executable"),
        "unexpected error: {err}"
    );

    match prev {
        Some(v) => std::env::set_var("CLAUDE_BIN", v),
        None => std::env::remove_var("CLAUDE_BIN"),
    }
}

#[test]
fn parse_claude_envelope_covers_supported_shapes_and_empty_error() {
    let cases = [
        (
            "result envelope",
            r#"{"result":"hello world","cost_usd":0.01}"#,
            Some("hello world"),
        ),
        (
            "raw text fallback",
            "raw text output",
            Some("raw text output"),
        ),
        (
            "content array",
            r#"{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#,
            Some("ab"),
        ),
        ("empty output", "   ", None),
    ];

    for (name, stdout, expected) in cases {
        match expected {
            Some(expected) => assert_eq!(
                parse_claude_json_envelope(stdout).unwrap(),
                expected,
                "case `{name}`"
            ),
            None => assert!(
                parse_claude_json_envelope(stdout).is_err(),
                "case `{name}` must remain an error"
            ),
        }
    }
}

#[test]
fn cleanup_removes_old_failed_dirs_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Fresh success — should be kept.
    let fresh = root.join("fresh-1");
    std::fs::create_dir_all(&fresh).unwrap();
    std::fs::write(fresh.join("status.json"), r#"{"status":"success"}"#).unwrap();

    // "Old" failed — same on-disk mtime, but we pretend `now` is 60 days
    // in the future so the dir's age exceeds the failed-retention cap.
    let old_failed = root.join("old-failed");
    std::fs::create_dir_all(&old_failed).unwrap();
    std::fs::write(old_failed.join("status.json"), r#"{"status":"failed"}"#).unwrap();

    // "now" = real now + 60 days → all entries appear 60 days old.
    let future_now = SystemTime::now() + Duration::from_secs(60 * 86_400);
    let (removed, scanned) = cleanup_runs_dir(root, future_now);
    // Both are 60 days old: failed (30d cap) removed, success (7d cap) also removed.
    assert_eq!(scanned, 2);
    assert_eq!(removed, 2);
    assert!(!fresh.exists());
    assert!(!old_failed.exists());

    // Second scenario: pretend now is 10 days in the future. Failed
    // (30d cap) stays; success (7d cap) is removed.
    let fresh2 = root.join("fresh-2");
    std::fs::create_dir_all(&fresh2).unwrap();
    std::fs::write(fresh2.join("status.json"), r#"{"status":"success"}"#).unwrap();
    let failed2 = root.join("failed-2");
    std::fs::create_dir_all(&failed2).unwrap();
    std::fs::write(failed2.join("status.json"), r#"{"status":"failed"}"#).unwrap();

    let near_future = SystemTime::now() + Duration::from_secs(10 * 86_400);
    let (removed, scanned) = cleanup_runs_dir(root, near_future);
    assert_eq!(scanned, 2);
    assert_eq!(removed, 1, "only the success dir should be over its 7d cap");
    assert!(!fresh2.exists());
    assert!(failed2.exists());
}

#[test]
fn format_pool_prompt_combines_system_and_user() {
    let out = format_pool_prompt("be precise", "the question");
    assert!(out.contains("be precise"));
    assert!(out.contains("the question"));
    assert!(out.contains("<system>"));
    assert!(out.contains("</system>"));
}

#[test]
fn format_pool_prompt_skips_separator_when_system_empty() {
    let out = format_pool_prompt("   ", "just user");
    assert_eq!(out, "just user");
}

#[cfg(unix)]
#[tokio::test]
async fn run_claude_cli_kills_timed_out_child() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_claude = tmp.path().join("claude");
    let sentinel = tmp.path().join("sentinel");
    std::fs::write(
        &fake_claude,
        "#!/bin/sh\nsleep 0.2\nprintf done > \"$SENTINEL_FILE\"\nprintf '{\"result\":\"late\"}\\n'\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let previous_sentinel = std::env::var("SENTINEL_FILE").ok();
    std::env::set_var("SENTINEL_FILE", &sentinel);
    let pool = ClaudePool {
        sem: Arc::new(Semaphore::new(1)),
        runs_dir: tmp.path().to_path_buf(),
        timeout: Duration::from_millis(50),
        binary: Ok(fake_claude.to_string_lossy().to_string()),
    };

    let err = pool
        .run_claude_cli("prompt from stdin")
        .await
        .expect_err("fake claude should time out");
    assert!(err.contains("timed out"), "unexpected error: {err}");
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert!(
        !sentinel.exists(),
        "timed-out claude child should be killed before it continues work"
    );

    match previous_sentinel {
        Some(value) => std::env::set_var("SENTINEL_FILE", value),
        None => std::env::remove_var("SENTINEL_FILE"),
    }
}

#[test]
fn skip_permissions_defaults_to_false_when_env_unset() {
    // clear the env var so .unwrap_or(false) fires.
    let prev = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS").ok();
    std::env::remove_var("TACHI_CLAUDE_SKIP_PERMISSIONS");
    let skip = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    assert!(
        !skip,
        "default should be false (preserve interactive prompts)"
    );
    match prev {
        Some(v) => std::env::set_var("TACHI_CLAUDE_SKIP_PERMISSIONS", v),
        None => std::env::remove_var("TACHI_CLAUDE_SKIP_PERMISSIONS"),
    }
}

#[test]
fn skip_permissions_is_false_when_env_is_false() {
    let prev = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS").ok();
    std::env::set_var("TACHI_CLAUDE_SKIP_PERMISSIONS", "false");
    let skip = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    assert!(!skip, "explicit false should disable skip");
    match prev {
        Some(v) => std::env::set_var("TACHI_CLAUDE_SKIP_PERMISSIONS", v),
        None => std::env::remove_var("TACHI_CLAUDE_SKIP_PERMISSIONS"),
    }
}

#[tokio::test]
async fn pool_call_with_fallback_propagates_fallback_error() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = ClaudePool {
        sem: Arc::new(Semaphore::new(1)),
        runs_dir: tmp.path().to_path_buf(),
        timeout: Duration::from_secs(5),
        binary: Ok("/nonexistent/__tachi_test_no_such_claude_2__".to_string()),
    };

    let err = pool_call_with_fallback(&pool, "sys", "usr", "unit-test-err", || async {
        Err::<String, _>("raw api also down".to_string())
    })
    .await
    .expect_err("fallback error should surface");
    assert!(err.contains("raw api also down"));
}

#[tokio::test]
#[cfg(unix)]
async fn prompt_file_is_owner_readable_only() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let pool = ClaudePool {
        sem: Arc::new(Semaphore::new(1)),
        runs_dir: tmp.path().to_path_buf(),
        timeout: Duration::from_secs(1),
        binary: Ok("/nonexistent/__tachi_test_no_such_claude_prompt__".to_string()),
    };

    let _ = pool
        .call("perm-test", "secret prompt body")
        .await
        .expect_err("binary missing so call fails after writing prompt");

    let prompt_file = std::fs::read_dir(tmp.path())
        .unwrap()
        .flat_map(|e| e.ok())
        .find(|e| e.file_type().unwrap().is_dir())
        .map(|e| e.path().join("prompt.md"))
        .expect("run dir with prompt.md should exist");

    let meta = std::fs::metadata(&prompt_file).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "prompt.md should be owner-readable only");
    let contents = std::fs::read_to_string(&prompt_file).unwrap();
    assert!(contents.contains("secret prompt body"));
}

#[cfg(unix)]
#[test]
fn foundry_runs_dir_is_created_with_0o700() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("temp home");
    let app_home = tmp.path().join(".tachi");

    let _pool = ClaudePool::new_in_app_home(1, &app_home);
    let runs_dir = app_home.join("foundry-runs");
    assert!(runs_dir.exists(), "foundry-runs dir should be created");
    let mode = std::fs::metadata(&runs_dir)
        .expect("foundry-runs metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o700,
        "foundry-runs dir should be restricted to owner"
    );
}

#[test]
fn foundry_runs_dir_honors_tachi_home() {
    let _guard = crate::test_support::global_test_lock().lock();
    let tmp = tempfile::tempdir().expect("temp home");
    let custom_home = tmp.path().join("custom-tachi-home");
    let default_home = tmp.path().join("home");
    std::fs::create_dir_all(&default_home).expect("create default home");

    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", &default_home);
    std::env::set_var("TACHI_HOME", &custom_home);

    let pool = ClaudePool::new(1);
    assert_eq!(pool.runs_dir(), custom_home.join("foundry-runs").as_path());
    assert!(custom_home.join("foundry-runs").exists());
    assert!(
        !default_home.join(".tachi").join("foundry-runs").exists(),
        "ClaudePool should not fall back to HOME/.tachi when TACHI_HOME is set"
    );

    restore_os_env("HOME", original_home);
    restore_os_env("TACHI_HOME", original_tachi_home);
}

fn restore_os_env(key: &str, value: Option<std::ffi::OsString>) {
    if let Some(value) = value {
        std::env::set_var(key, value);
    } else {
        std::env::remove_var(key);
    }
}

// ── #1087 provider-path rollout ─────────────────────────────────────────

#[tokio::test]
async fn call_via_provider_writes_same_artifact_contract_as_call() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = ClaudePool {
        sem: Arc::new(Semaphore::new(1)),
        runs_dir: tmp.path().to_path_buf(),
        timeout: Duration::from_secs(5),
        binary: Ok("/nonexistent/__tachi_test_no_such_claude_provider__".to_string()),
    };

    let outcome = pool
        .call_via_provider("provider-test", "recorded prompt text", || async {
            Ok::<_, String>("provider response text".to_string())
        })
        .await
        .expect("provider executor should succeed without touching the CLI binary");
    assert_eq!(outcome.text, "provider response text");

    let run_dir = std::fs::read_dir(tmp.path())
        .unwrap()
        .flat_map(|e| e.ok())
        .find(|e| e.file_type().unwrap().is_dir())
        .map(|e| e.path())
        .expect("run dir should exist");

    let prompt = std::fs::read_to_string(run_dir.join("prompt.md")).unwrap();
    assert_eq!(prompt, "recorded prompt text");
    let result = std::fs::read_to_string(run_dir.join("result.md")).unwrap();
    assert_eq!(result, "provider response text");
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["status"], "success");
    assert_eq!(status["label"], "provider-test");
}

#[tokio::test]
async fn call_via_provider_records_failure_status_on_executor_error() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = ClaudePool {
        sem: Arc::new(Semaphore::new(1)),
        runs_dir: tmp.path().to_path_buf(),
        timeout: Duration::from_secs(5),
        binary: Ok("/nonexistent/__tachi_test_no_such_claude_provider_err__".to_string()),
    };

    let err = pool
        .call_via_provider("provider-fail", "prompt", || async {
            Err::<String, _>("provider unreachable".to_string())
        })
        .await
        .expect_err("executor error should surface");
    assert_eq!(err, "provider unreachable");

    let run_dir = std::fs::read_dir(tmp.path())
        .unwrap()
        .flat_map(|e| e.ok())
        .find(|e| e.file_type().unwrap().is_dir())
        .map(|e| e.path())
        .expect("run dir should exist");
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["status"], "failed");
    assert_eq!(status["error"], "provider unreachable");
}

// The four tests below all mutate the process-wide `PROVIDER_ROLLOUT_ENV`
// var, so — matching this crate's `global_test_lock` convention (see
// `foundry_runs_dir_honors_tachi_home` above, and 5a561225 in
// tachi-server) — each is a plain `#[test] fn` that acquires the lock
// SYNCHRONOUSLY (guard lives for the whole fn body, never across an
// `.await`) and drives its async call through a local
// `tokio::runtime::Runtime::block_on`, not `#[tokio::test]`. That keeps the
// lock held for the entire critical section (env mutation → the call that
// reads it) while satisfying clippy's `await_holding_lock`.

#[test]
fn pool_call_with_fallback_provider_first_skips_cli_when_provider_succeeds() {
    let _guard = crate::test_support::global_test_lock().lock();
    let prev = std::env::var(super::rollout::PROVIDER_ROLLOUT_ENV).ok();
    std::env::set_var(super::rollout::PROVIDER_ROLLOUT_ENV, "true");

    let tmp = tempfile::tempdir().unwrap();
    // A CLI binary that would fail loudly if invoked — proves the CLI pool
    // is never touched when the provider succeeds.
    let pool = ClaudePool {
        sem: Arc::new(Semaphore::new(1)),
        runs_dir: tmp.path().to_path_buf(),
        timeout: Duration::from_secs(5),
        binary: Ok("/nonexistent/__tachi_test_provider_first_cli__".to_string()),
    };

    let (text, source) = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(pool_call_with_fallback(
            &pool,
            "sys",
            "usr",
            "provider-first-ok",
            || async { Ok::<_, String>("provider-first-text".to_string()) },
        ))
        .expect("provider path should succeed");

    assert_eq!(text, "provider-first-text");
    assert_eq!(source, PoolCallSource::ProviderTachiLlm);
    assert_eq!(source.as_str(), "provider_tachi_llm");

    match prev {
        Some(v) => std::env::set_var(super::rollout::PROVIDER_ROLLOUT_ENV, v),
        None => std::env::remove_var(super::rollout::PROVIDER_ROLLOUT_ENV),
    }
}

#[ignore = "issue #1261: forks+execs a fake claude shell binary; broken-pipe flake on shared CI runners where spawn() returns before the child is schedulable. Run with --ignored"]
#[test]
fn pool_call_with_fallback_provider_first_falls_back_to_cli_on_provider_error() {
    let _guard = crate::test_support::global_test_lock().lock();
    let prev = std::env::var(super::rollout::PROVIDER_ROLLOUT_ENV).ok();
    std::env::set_var(super::rollout::PROVIDER_ROLLOUT_ENV, "true");

    let tmp = tempfile::tempdir().unwrap();
    let fake_claude = tmp.path().join("claude");
    std::fs::write(
        &fake_claude,
        "#!/bin/sh\nprintf '{\"result\":\"cli-fallback-text\"}\\n'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let pool = ClaudePool {
        sem: Arc::new(Semaphore::new(1)),
        runs_dir: tmp.path().to_path_buf(),
        timeout: Duration::from_secs(5),
        binary: Ok(fake_claude.to_string_lossy().to_string()),
    };

    let (text, source) = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(pool_call_with_fallback(
            &pool,
            "sys",
            "usr",
            "provider-first-degrade",
            || async { Err::<String, _>("provider down".to_string()) },
        ))
        .expect("CLI pool fallback should succeed after provider error");

    assert_eq!(text, "cli-fallback-text");
    assert_eq!(source, PoolCallSource::ClaudeCli);

    match prev {
        Some(v) => std::env::set_var(super::rollout::PROVIDER_ROLLOUT_ENV, v),
        None => std::env::remove_var(super::rollout::PROVIDER_ROLLOUT_ENV),
    }
}

#[test]
fn pool_call_with_fallback_provider_first_errors_when_both_paths_fail() {
    let _guard = crate::test_support::global_test_lock().lock();
    let prev = std::env::var(super::rollout::PROVIDER_ROLLOUT_ENV).ok();
    std::env::set_var(super::rollout::PROVIDER_ROLLOUT_ENV, "true");

    let tmp = tempfile::tempdir().unwrap();
    let pool = ClaudePool {
        sem: Arc::new(Semaphore::new(1)),
        runs_dir: tmp.path().to_path_buf(),
        timeout: Duration::from_secs(5),
        binary: Ok("/nonexistent/__tachi_test_provider_first_both_fail__".to_string()),
    };

    let err = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(pool_call_with_fallback(
            &pool,
            "sys",
            "usr",
            "provider-first-both-fail",
            || async { Err::<String, _>("provider down".to_string()) },
        ))
        .expect_err("both paths failing should surface an error");
    assert!(err.contains("provider down"), "unexpected error: {err}");
    assert!(err.contains("CLI pool fallback"), "unexpected error: {err}");

    match prev {
        Some(v) => std::env::set_var(super::rollout::PROVIDER_ROLLOUT_ENV, v),
        None => std::env::remove_var(super::rollout::PROVIDER_ROLLOUT_ENV),
    }
}

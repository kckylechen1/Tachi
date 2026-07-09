use super::*;

// ─── Phase 6: Dispatch V2 two-stage smoke test ──────────────────────────────
//
// Spawns the full V2 flow against a fake `claude` binary that emits a
// canned plan envelope. Marked `#[ignore]` because:
//   * it writes under a temp `TACHI_HOME` and shells out to `bash`;
//   * it requires `bash` on PATH and a writable temp dir;
//   * it mutates env vars (CLAUDE_BIN, DISPATCH_V2_ENABLED, TACHI_HOME)
//     so it must not run concurrently with other env-sensitive tests.
//
// Run explicitly via:
//     cargo test -p tachi-server v2_two_stage_smoke -- --ignored --test-threads=1

#[tokio::test]
#[ignore]
async fn v2_two_stage_smoke() {
    use std::io::Write;

    // Isolated TACHI_HOME so run files don't pollute real one.
    let temp_home = std::env::temp_dir().join(format!("tachi-v2-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_home).expect("create temp tachi home");

    // Fake claude binary: prints a JSON envelope matching the pool's
    // expected `{"result": "..."}` shape, embedding a valid plan.
    let fake_claude = temp_home.join("claude-test");
    {
        let mut f = std::fs::File::create(&fake_claude).expect("create fake claude");
        // The pool invokes `claude -p --output-format json --dangerously-skip-permissions`
        // with the prompt on stdin. We ignore stdin and emit a fixed envelope.
        writeln!(
            f,
            "#!/usr/bin/env bash\ncat <<'JSON'\n{{\"result\":\"## Goal\\nDo the smoke test.\\n\\n## Steps\\n1. inspect\\n2. ship\\n\\n## Files\\n- src/lib.rs\\n\\n## Validation\\n- cargo test\\n\"}}\nJSON"
        )
        .unwrap();
    }
    let mut perms = std::fs::metadata(&fake_claude).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&fake_claude, perms).unwrap();

    // Activate V2 + isolate.
    std::env::set_var("TACHI_HOME", &temp_home);
    std::env::set_var("CLAUDE_BIN", &fake_claude);
    std::env::set_var("TACHI_CLAUDE_SKIP_PERMISSIONS", "true");
    std::env::set_var("DISPATCH_V2_ENABLED", "true");
    std::env::set_var("DISPATCH_V2_PLAN_REVIEW", "false");

    let server = make_server();

    // We can't easily call the private handle_tachi_dispatch from
    // outside the crate, but tests live inside the crate so the
    // `pub(crate)` visibility is accessible via crate path.
    let mut params = dispatch_params(Some("custom"), "smoke v2");
    // Execute stage uses a no-op command so the test doesn't need
    // a working claude/codex CLI for Stage 2.
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let resp_json = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("v2 dispatch should succeed");

    let resp: serde_json::Value = serde_json::from_str(&resp_json).expect("v2 response JSON");
    assert_eq!(resp["v2"], serde_json::json!(true), "response: {resp:#}");
    let run_dir = resp["run_dir"].as_str().expect("run_dir present");
    let run_dir = std::path::PathBuf::from(run_dir);

    // Stage 1 artifacts exist immediately.
    let plan = std::fs::read_to_string(run_dir.join("plan.md")).expect("plan.md written");
    assert!(plan.contains("## Goal"), "plan.md content: {plan}");
    assert!(plan.contains("## Validation"), "plan.md content: {plan}");

    let trajectory_path = run_dir.join("trajectory.jsonl");
    let mut trajectory = std::fs::read_to_string(&trajectory_path).expect("trajectory present");
    for _ in 0..30 {
        if trajectory.contains("\"event\":\"execute_started\"") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        trajectory = std::fs::read_to_string(&trajectory_path).expect("trajectory present");
    }
    assert!(trajectory.contains("\"event\":\"dispatch_started\""));
    assert!(trajectory.contains("\"event\":\"plan_generated\""));
    assert!(trajectory.contains("\"event\":\"execute_started\""));
    let progress =
        std::fs::read_to_string(run_dir.join("progress.jsonl")).expect("progress present");
    assert!(progress.contains("\"event\":\"dispatch_started\""));
    assert!(progress.contains("\"event\":\"plan_generated\""));

    // Wait briefly for the spawned stage-2 task to write final status.json.
    for _ in 0..30 {
        if let Ok(raw) = std::fs::read_to_string(run_dir.join("status.json")) {
            if let Ok(status) = serde_json::from_str::<serde_json::Value>(&raw) {
                if status["duration_ms_plan"].as_u64().is_some()
                    && status["duration_ms_execute"].as_u64().is_some()
                {
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status.json written"),
    )
    .expect("status.json valid");
    assert_eq!(status["v2"], serde_json::json!(true));
    assert!(status["duration_ms_plan"].as_u64().is_some());
    assert_eq!(status["plan_review_status"], serde_json::json!("approved"));

    // Cleanup.
    std::env::remove_var("CLAUDE_BIN");
    std::env::remove_var("DISPATCH_V2_ENABLED");
    std::env::remove_var("DISPATCH_V2_PLAN_REVIEW");
    std::env::remove_var("TACHI_HOME");
    let _ = std::fs::remove_dir_all(&temp_home);
}

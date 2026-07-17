use super::super::make_server;
use crate::tool_params::{CompactContextParams, Message};
use rmcp::handler::server::wrapper::Parameters;

fn base_params(persist: bool) -> CompactContextParams {
    CompactContextParams {
        agent_id: "main".to_string(),
        conversation_id: "conv-1099".to_string(),
        window_id: "window-1099".to_string(),
        trigger: "manual".to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: "Some content to compact.".to_string(),
        }],
        current_summary: None,
        path_prefix: None,
        project: None,
        target_tokens: 128,
        max_output_tokens: 256,
        persist,
    }
}

/// #1099 discrimination: on origin/main, `persist=true` returned a nominal
/// `"status": "skipped"|"completed"` success with two always-empty
/// (`captured_memory_ids`/`queued_job_ids`) fields and, at best, an
/// easy-to-miss `warning` — nominal success with no persistence, forbidden
/// by the #1099 contract. This must now be a loud refusal (`Err`) that never
/// reaches the empty-messages/model-call paths, pointing callers at the real
/// persistence tool. Red on origin/main (old code returns `Ok(..)` with
/// `status: "skipped"`), green after this change (returns `Err(..)`).
#[tokio::test]
async fn compact_context_persist_true_is_refused_not_nominal_success() {
    let server = make_server();

    let err = server
        .compact_context(Parameters(base_params(true)))
        .await
        .expect_err("persist=true must be refused, not a nominal success");

    assert!(
        err.contains("compact_session_memory"),
        "refusal must point at the real persistence path: {err}"
    );
    assert!(
        err.contains("persist"),
        "refusal must explain which parameter triggered it: {err}"
    );
}

/// The refusal fires before the empty-messages short-circuit too — persist
/// intent is rejected regardless of whether there is anything to compact.
#[tokio::test]
async fn compact_context_persist_true_is_refused_even_with_empty_messages() {
    let server = make_server();
    let mut params = base_params(true);
    params.messages = vec![Message {
        role: "user".to_string(),
        content: "   ".to_string(),
    }];

    let err = server
        .compact_context(Parameters(params))
        .await
        .expect_err("persist=true must be refused even when messages are empty");
    assert!(err.contains("compact_session_memory"), "{err}");
}

/// #1099 round-2 (codex adversarial review, 2026-07-17): the persist=true
/// refusal must fire *before* any daemon forwarding is attempted, not only
/// inside the in-process handler. `cli_client::detect::daemon_version_matches`
/// compares nothing but `CARGO_PKG_VERSION` strings, so it cannot tell a
/// stale pre-#1099 daemon apart from the current binary — forwarding
/// `persist=true` to a same-version daemon was exactly how an old nominal-
/// success body could still reach a caller. This test proves the *ordering*:
/// it stands up a reachable, version/db-matching daemon stand-in (discoverable
/// via the same scoped pid file `cli_client::detect` reads) and asserts it is
/// never contacted at all — the refusal belongs to the facade
/// (`tools/runtime_context_facade.rs::compact_context`), before
/// `maybe_forward_server_read` ever runs, not to a race between forwarding
/// and a fallback.
#[tokio::test]
async fn compact_context_persist_true_never_contacts_a_reachable_daemon() {
    let (server, _temp_home) = super::super::make_server_with_temp_home();
    let global_db = server.global_db_path_buf();
    let app_home = crate::path_utils::tachi_home();

    // A raw TCP listener stands in for a reachable daemon. It never speaks
    // the MCP protocol at all — if `compact_context` ever tried to forward to
    // it, that attempt would fail loudly, and because read-forwarding falls
    // back to in-process on any dispatch failure, a failed forward attempt
    // alone would NOT distinguish "never forwarded" from "forwarded and
    // failed". The `contacted` flag is the real assertion: it can only flip
    // if a TCP connection actually reaches this listener, which only happens
    // if forwarding was attempted at all.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind daemon stand-in");
    let port = listener.local_addr().expect("local addr").port();
    let contacted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let contacted_writer = contacted.clone();
    let accept_task = tokio::spawn(async move {
        if listener.accept().await.is_ok() {
            contacted_writer.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });

    let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&app_home, &global_db);
    std::fs::create_dir_all(pid_path.parent().expect("pid parent")).expect("pid parent dir");
    std::fs::write(
        &pid_path,
        serde_json::json!({
            "pid": std::process::id(),
            "port": port,
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "global_db": global_db.display().to_string(),
            "project_db": null,
            "version": env!("CARGO_PKG_VERSION"),
        })
        .to_string(),
    )
    .expect("write scoped daemon pid file");

    let err = server
        .compact_context(Parameters(base_params(true)))
        .await
        .expect_err(
            "persist=true must be refused even with a version/db-matching daemon reachable",
        );
    assert!(
        err.contains("compact_session_memory"),
        "refusal must still point at the real persistence path: {err}"
    );

    // Headroom for the (should-never-fire) accept task to prove it really
    // didn't run, rather than racing a fresh spawn.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    accept_task.abort();
    assert!(
        !contacted.load(std::sync::atomic::Ordering::SeqCst),
        "compact_context persist=true must never contact a daemon, even a \
         version/db-matching one — the refusal happens in the facade, before \
         any forwarding attempt, so a stale daemon's old nominal-success body \
         can never reach the caller"
    );

    let _ = std::fs::remove_file(&pid_path);
}

/// `persist=false` (the default) is unaffected — the handler still reaches
/// the ordinary empty-messages short-circuit and returns success with no
/// dead `captured_memory_ids`/`queued_job_ids` fields (#1099: those fields
/// never described a real operation and are gone entirely now).
#[tokio::test]
async fn compact_context_persist_false_reaches_normal_short_circuit() {
    let server = make_server();
    let mut params = base_params(false);
    params.messages = vec![Message {
        role: "user".to_string(),
        content: "   ".to_string(),
    }];

    let result = server
        .compact_context(Parameters(params))
        .await
        .expect("persist=false should not be refused");
    let json: serde_json::Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["status"], serde_json::json!("skipped"));
    assert!(
        json.get("captured_memory_ids").is_none(),
        "dead placeholder field must not be in the response: {json}"
    );
    assert!(
        json.get("queued_job_ids").is_none(),
        "dead placeholder field must not be in the response: {json}"
    );
}

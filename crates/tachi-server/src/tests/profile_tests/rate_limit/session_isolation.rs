use super::*;
use crate::server_state::MemoryServer;

/// Identical tool+args for every call so burst keys collide on purpose.
/// Driven through real `ServerHandler::call_tool` (via `call_tool_on_server`)
/// so a hard-coded `"default"` session key at the handler gate fails these tests.
fn isolation_args() -> serde_json::Map<String, serde_json::Value> {
    // `runtime_info` ignores unknown fields; a fixed marker keeps the args hash
    // stable and non-empty across the MCP oneshot path.
    let mut args = serde_json::Map::new();
    args.insert(
        "session_isolation_marker".to_string(),
        json!("1255-burst-probe"),
    );
    args
}

async fn call_tool_rate_limit_outcome(server: MemoryServer) -> &'static str {
    match call_tool_on_server(server, "runtime_info", Some(isolation_args())).await {
        Ok(_) => "ok",
        Err(err) if err.message.contains("Loop detected") => "rate_limited",
        Err(err) => panic!("unexpected call_tool error: {}", err.message),
    }
}

#[tokio::test]
async fn rate_limit_burst_does_not_bleed_across_mcp_session_clones() {
    let base = make_server();
    let session_a = base.clone_for_mcp_session();
    let session_b = base.clone_for_mcp_session();

    assert_ne!(
        session_a.rate_limit_session_id(),
        session_b.rate_limit_session_id(),
        "clone_for_mcp_session must stamp distinct rate-limit session ids"
    );

    // Drive session A to the default burst cap (8) through the real MCP gate.
    for i in 0..8 {
        let outcome = call_tool_rate_limit_outcome(session_a.clone()).await;
        assert_eq!(
            outcome,
            "ok",
            "session A call {} should succeed via call_tool, got {outcome}",
            i + 1
        );
    }
    let ninth = call_tool_rate_limit_outcome(session_a.clone()).await;
    assert_eq!(
        ninth, "rate_limited",
        "session A 9th identical call_tool should hit burst"
    );

    // Session B shares the RateLimiter Arc and uses the same tool+args, but
    // must not inherit A's burst window — only true when call_tool keys by
    // the stamped session id (not a hard-coded "default").
    let first_b = call_tool_rate_limit_outcome(session_b.clone()).await;
    assert_eq!(
        first_b, "ok",
        "session B must still succeed after A hit burst; got {first_b}"
    );
}

/// Concurrent receipt harness (#1255 leaf B): N parallel `call_tool`s across
/// ≥2 session clones. Asserts the exact independent default burst budget
/// (8 ok + remaining rate_limited) per session — a shared `"default"` key
/// cannot satisfy both sessions' exact 8/4 split.
#[tokio::test]
async fn rate_limit_concurrent_session_receipt_harness() {
    let base = make_server();
    let session_a = base.clone_for_mcp_session();
    let session_b = base.clone_for_mcp_session();
    let sessions = [session_a, session_b];

    const CALLS_PER_SESSION: usize = 12;
    const DEFAULT_BURST: usize = 8;
    let mut handles = Vec::with_capacity(sessions.len() * CALLS_PER_SESSION);

    for (session_idx, session) in sessions.iter().cloned().enumerate() {
        for call_idx in 0..CALLS_PER_SESSION {
            let server = session.clone();
            handles.push(tokio::spawn(async move {
                let outcome = call_tool_rate_limit_outcome(server).await;
                (session_idx, call_idx, outcome)
            }));
        }
    }

    let mut receipts: Vec<(usize, usize, &'static str)> = Vec::with_capacity(handles.len());
    for handle in handles {
        receipts.push(handle.await.expect("join concurrent rate-limit task"));
    }
    receipts.sort_by_key(|(session_idx, call_idx, _)| (*session_idx, *call_idx));

    assert_eq!(
        receipts.len(),
        sessions.len() * CALLS_PER_SESSION,
        "harness must record every concurrent call_tool"
    );

    let mut ok_by_session = [0usize; 2];
    let mut limited_by_session = [0usize; 2];
    for (session_idx, _call_idx, outcome) in &receipts {
        match *outcome {
            "ok" => ok_by_session[*session_idx] += 1,
            "rate_limited" => limited_by_session[*session_idx] += 1,
            other => panic!("unknown outcome {other}"),
        }
    }

    for session_idx in 0..2 {
        assert_eq!(
            ok_by_session[session_idx], DEFAULT_BURST,
            "session {session_idx} must get exactly {DEFAULT_BURST} ok via call_tool; \
             receipts={receipts:?}"
        );
        assert_eq!(
            limited_by_session[session_idx],
            CALLS_PER_SESSION - DEFAULT_BURST,
            "session {session_idx} must get exactly {} rate_limited via call_tool; \
             receipts={receipts:?}",
            CALLS_PER_SESSION - DEFAULT_BURST
        );
    }
}

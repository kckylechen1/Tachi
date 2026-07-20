use super::*;

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

    // Drive session A to the default burst cap (8) with identical tool+args.
    for i in 0..8 {
        session_a
            .check_session_rate_limit("save_memory", "hash-shared")
            .unwrap_or_else(|e| panic!("session A call {} should succeed: {:?}", i + 1, e));
    }
    let err = session_a
        .check_session_rate_limit("save_memory", "hash-shared")
        .expect_err("session A 9th identical call should hit burst");
    assert!(
        err.message.contains("Loop detected"),
        "expected loop detection on session A, got: {}",
        err.message
    );

    // Session B shares the RateLimiter Arc and uses the same tool+args, but
    // must not inherit A's burst window.
    session_b
        .check_session_rate_limit("save_memory", "hash-shared")
        .expect("session B must still succeed after A hit burst");
}

/// Concurrent receipt harness (#1255 leaf B): N parallel checks across ≥2
/// session clones, recording ok / rate-limit rejection per call. Foundation
/// for later live-timeout receipts — does not exercise daemon idle timeouts.
#[tokio::test]
async fn rate_limit_concurrent_session_receipt_harness() {
    let base = make_server();
    let session_a = base.clone_for_mcp_session();
    let session_b = base.clone_for_mcp_session();
    let sessions = [session_a, session_b];

    const CALLS_PER_SESSION: usize = 12;
    let mut handles = Vec::with_capacity(sessions.len() * CALLS_PER_SESSION);

    for (session_idx, session) in sessions.iter().cloned().enumerate() {
        for call_idx in 0..CALLS_PER_SESSION {
            let server = session.clone();
            handles.push(tokio::spawn(async move {
                let outcome = match server.check_session_rate_limit("save_memory", "hash-concurrent")
                {
                    Ok(_) => "ok",
                    Err(err) if err.message.contains("Loop detected") => "rate_limited",
                    Err(err) => panic!(
                        "unexpected rate-limit error session={session_idx} call={call_idx}: {}",
                        err.message
                    ),
                };
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
        "harness must record every concurrent call"
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

    // Default burst is 8: each session should see some oks and some rejections
    // of its own, not a single shared pool that collapses one session to zero.
    for session_idx in 0..2 {
        assert!(
            ok_by_session[session_idx] > 0,
            "session {session_idx} should record at least one ok; receipts={receipts:?}"
        );
        assert!(
            limited_by_session[session_idx] > 0,
            "session {session_idx} should record at least one rate_limited; receipts={receipts:?}"
        );
        assert_eq!(
            ok_by_session[session_idx] + limited_by_session[session_idx],
            CALLS_PER_SESSION,
            "session {session_idx} receipt count mismatch; receipts={receipts:?}"
        );
    }
}

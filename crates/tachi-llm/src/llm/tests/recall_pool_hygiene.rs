// recall_pool_hygiene.rs — #926 cross-vendor review regression coverage.
//
// A prior version of the pool-hygiene accounting recorded a recall-path
// provider outcome as SUCCESS the instant `send()` returned Ok — i.e. as soon
// as response *headers* arrived — resetting the consecutive-timeout streak
// before the body was ever read. A provider that sends headers and then
// stalls the body forever would therefore never trip the rebuild-after-3
// threshold: each stalled call reset the streak to 0 right at the header
// stage, then (only later) the body-read timeout error propagated without
// ever touching the streak at all.
//
// This test reproduces exactly that shape (headers now, body never) against
// a real local listener and asserts the streak only grows on the genuine
// timeout-class outcome, reaching the rebuild threshold and resetting once
// rebuilt — proving accounting now waits for the full round trip.

use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn recall_pool_streak_counts_header_then_stall_as_timeout_not_success() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Reentrant GlobalTestLock (R2): .lock() already swallows poison and
    // returns TestLockGuard — do not chain .unwrap_or_else(into_inner).
    let _guard = crate::test_support::global_test_lock().lock();
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

    // Header-then-stall mock provider: accepts each connection, writes
    // response headers that promise a body, then never sends the body and
    // never closes the socket. The client's body read stalls until its own
    // per-request deadline fires.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind header-then-stall provider");
    let port = listener.local_addr().expect("provider addr").port();
    let held = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let held_task = held.clone();
    let server_task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let response = b"HTTP/1.1 200 OK\r\ncontent-length: 4096\r\ncontent-type: application/json\r\n\r\n";
            if socket.write_all(response).await.is_err() {
                continue;
            }
            // Hold the socket open without ever writing the body — the
            // headers are already on the wire, so a pre-fix client would
            // have recorded this connection as "success" right here.
            held_task.lock().await.push(socket);
        }
    });

    let _base_guard = EnvRestore::set("VOYAGE_BASE_URL", format!("http://127.0.0.1:{port}"));
    let _key_guard = EnvRestore::set("VOYAGE_API_KEY", "test-voyage-key");
    let _timeout_guard = EnvRestore::set("TACHI_RECALL_PROVIDER_TIMEOUT_SECS", "1");
    // One attempt per call so each `embed_voyage_batch` invocation below maps
    // to exactly one send + one body-read-timeout, making the streak math
    // deterministic (no internal retry loop to account for).
    let _attempts_guard = EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");

    let client = LlmClient::new().expect("client should initialize");
    assert_eq!(
        client.recall_timeout_streak_for_tests(),
        0,
        "streak should start clean"
    );

    let started = std::time::Instant::now();
    for expected_streak in 1..LlmClient::POOL_TIMEOUT_REBUILD_THRESHOLD {
        let err = client
            .embed_voyage_batch(&["blackhole regression probe".to_string()], "query")
            .await
            .expect_err("header-then-stall provider must surface a body read/timeout error");
        assert!(
            err.contains("Voyage batch API request failed") || err.contains("body read failed"),
            "unexpected error shape for a stalled body: {err}"
        );
        assert_eq!(
            client.recall_timeout_streak_for_tests(),
            expected_streak,
            "consecutive header-then-stall outcomes must grow the timeout \
             streak, not reset it at the header-received stage"
        );
    }

    // One more stalled call reaches POOL_TIMEOUT_REBUILD_THRESHOLD and
    // triggers a pooled-client rebuild, which resets the streak to 0.
    let _ = client
        .embed_voyage_batch(&["blackhole regression probe".to_string()], "query")
        .await
        .expect_err("final header-then-stall call should also fail");
    assert_eq!(
        client.recall_timeout_streak_for_tests(),
        0,
        "reaching the rebuild threshold must rebuild the pooled client and \
         reset the streak"
    );

    // Bounded wall-clock budget across all attempts: proves this test itself
    // never depends on the old 60s-class timeout to observe the behavior.
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "recall pool hygiene regression should stay bounded, took {elapsed:?}"
    );

    server_task.abort();
    drop(held);
}

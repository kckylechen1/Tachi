// recall_failsafe.rs — #926 discrimination test.
//
// Reproduces the 2026-07-09 incident: a blackholed embedding provider (a dead
// proxy that accepts TCP but never responds) froze every embed-requiring
// recall. Asserts the fail-safe: query embedding is bounded, search falls back
// to lexical-only, and the response carries a machine-readable degraded marker.
//
// RED-by-construction against the old behavior: pre-#926 the shared client had
// a blanket 60s timeout and embed retried MAX_ATTEMPTS = 3, so this same dead
// listener would have hung ~180s. The elapsed-budget assertion below fails hard
// under that behavior.

use super::*;

/// Restores an env var to its prior value (or unset) on drop.
struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Bind a loopback TCP listener that accepts connections and never replies —
/// the stand-in for the blackholed embedding proxy. Returns the bound port.
/// The accept loop runs on a detached thread and holds streams open so the
/// client's per-request read deadline (not a connection refusal) is what fires.
fn spawn_blackhole_listener() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind blackhole listener");
    let port = listener.local_addr().expect("blackhole local addr").port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            match stream {
                Ok(s) => held.push(s), // keep alive, never respond
                Err(_) => break,
            }
        }
    });
    port
}

#[tokio::test]
async fn search_falls_back_to_lexical_with_degraded_marker_when_embed_provider_blackholes() {
    // VOYAGE_BASE_URL + the embedding-enable toggle + recall timeouts are all
    // process-wide env; serialize against other env-mutating tests.
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let server = make_server();
    if !server.global_vec_available() {
        // Without sqlite-vec the embed branch never runs, so there is no
        // provider call to bound — nothing to discriminate. Mirrors the
        // guard used by the other vector-boundary tests.
        return;
    }

    server
        .with_global_store(|store| {
            let mut memory = make_entry("blackhole-lexical-row");
            memory.path = "/facts/blackhole-recall".to_string();
            memory.text = "BlackholeRecallNeedle lexical fallback memory row.".to_string();
            memory.summary = "Blackhole recall row".to_string();
            store.upsert(&memory).map_err(|e| e.to_string())
        })
        .expect("seed lexical fallback row");

    let port = spawn_blackhole_listener();

    let _base = EnvVarGuard::set("VOYAGE_BASE_URL", &format!("http://127.0.0.1:{port}"));
    let _timeout = EnvVarGuard::set("TACHI_RECALL_PROVIDER_TIMEOUT_SECS", "1");
    let _attempts = EnvVarGuard::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");
    // ensure_test_env disables query embedding suite-wide; re-enable here so the
    // recall path actually calls the (blackholed) provider.
    let _embed = EnvVarGuard::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "0");

    let started = std::time::Instant::now();
    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "BlackholeRecallNeedle".to_string(),
            query_vec: None,
            top_k: 5,
            path_prefix: None,
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        }))
        .await
        .expect("search should succeed despite a dead embedding provider");
    let elapsed = started.elapsed();

    // Bounded budget. ~1s expected (1 attempt × 1s deadline); generous ceiling
    // for CI. Fails hard under the old 60s × 3 unbounded behavior (~180s).
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "recall should stay bounded when the embed provider blackholes, took {elapsed:?}"
    );

    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("search JSON");

    // Lexical/FTS fallback still surfaces the row despite the failed embedding.
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("blackhole-lexical-row")),
        "lexical-only fallback should still return the matching row: {rows:#?}"
    );

    // Machine-readable degradation marker: recall_quality.degraded == "lexical_only: …".
    let marker = rows
        .iter()
        .find_map(|row| {
            row.get("recall_quality")
                .and_then(|rq| rq.get("degraded"))
                .and_then(|d| d.as_str())
        })
        .unwrap_or_else(|| panic!("lexical-only fallback must carry a degraded marker: {rows:#?}"));
    assert!(
        marker.starts_with("lexical_only"),
        "unexpected degraded marker: {marker}"
    );
}

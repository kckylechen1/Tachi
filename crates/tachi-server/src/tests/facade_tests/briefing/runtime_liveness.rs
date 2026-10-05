use super::*;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, Condvar, Mutex,
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

async fn legacy_rpc(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    payload: Value,
) -> Result<(Value, HeaderMap), String> {
    let response = client
        .post(url)
        .headers(headers.clone())
        .json(&payload)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let response_headers = response.headers().clone();
    let body = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("legacy RPC HTTP {status}: {body}"));
    }
    let parsed = serde_json::from_str::<Value>(&body)
        .map_err(|error| error.to_string())
        .or_else(|_| {
            body.lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .find_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
                .ok_or_else(|| format!("legacy RPC has no JSON response: {body}"))
        })?;
    if parsed.get("error").is_some() {
        return Err(format!("legacy RPC failed: {parsed}"));
    }
    Ok((parsed, response_headers))
}

// This client runs outside Tokio, so its deadline still fires if the server's
// only executor worker is pinned by a synchronous project open.
fn raw_liveness_probe(address: SocketAddr) -> Result<(), String> {
    let deadline = Duration::from_secs(1);
    let mut stream = TcpStream::connect_timeout(&address, deadline)
        .map_err(|error| format!("connect liveness: {error}"))?;
    stream
        .set_read_timeout(Some(deadline))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(deadline))
        .map_err(|error| error.to_string())?;
    write!(
        stream,
        "GET /health/live HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| format!("write liveness: {error}"))?;
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    let read_deadline = std::time::Instant::now() + deadline;
    while !response.ends_with(b"\r\n") && response.len() < 256 {
        let remaining = read_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("liveness HTTP status line exceeded its wall deadline".to_string());
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|error| error.to_string())?;
        let read = stream
            .read(&mut byte)
            .map_err(|error| format!("read liveness while project opens are delayed: {error}"))?;
        if read == 0 {
            return Err("liveness closed before an HTTP status line".to_string());
        }
        response.push(byte[0]);
    }
    let status = String::from_utf8_lossy(&response);
    if !status.starts_with("HTTP/1.1 200 ") {
        return Err(format!("expected liveness HTTP 200, got {status:?}"));
    }
    Ok(())
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Use the production router and legacy project-binding rail, with the real
/// multi-thread runtime flavor reduced to one worker. Both retrieval legs
/// must reach the delayed fixture open before liveness is measured: gating
/// only the first open could exercise the already-offloaded memory leg and
/// falsely pass while Wiki still pins the executor.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[allow(clippy::await_holding_lock)]
async fn legacy_project_briefing_keeps_http_live_while_project_opens_are_delayed() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _daemon = crate::test_support::EnvRestore::set("TACHI_DAEMON", "1");
    let ambient = tempfile::tempdir().expect("fixture ambient home");
    std::fs::create_dir_all(ambient.path().join("repo/.git")).expect("fixture workspace");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", ambient.path());
    let _workspace = crate::test_support::EnvRestore::set_path(
        "TACHI_PROJECT_ROOT",
        &ambient.path().join("repo"),
    );
    let _runs =
        crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &ambient.path().join("runs"));
    let server = make_server();
    assert!(server.project_db_path_buf().is_none(), "global-only daemon");
    let project = "runtime-open-fixture";
    let project_db = server
        .tachi_home_dir()
        .join("projects")
        .join(project)
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(project_db.parent().expect("project parent"))
        .expect("fixture project directory");
    let mut store = memcore::MemoryStore::open(project_db.to_str().expect("fixture path"))
        .expect("fresh disposable project DB");
    for (id, path) in [
        ("runtime-open-memory", "/scratch/runtime-open"),
        ("runtime-open-wiki", "/wiki/runtime-open"),
    ] {
        let mut entry = make_entry(id);
        entry.path = path.to_string();
        entry.summary = "RuntimeOpenNeedle fixture evidence".to_string();
        entry.text = entry.summary.clone();
        entry.keywords = vec!["RuntimeOpenNeedle".to_string()];
        store.upsert(&entry).expect("seed fixture evidence");
    }
    drop(store);
    let mut manifest = crate::manifest::Manifest::empty();
    manifest.dbs.push(crate::manifest::DbEntry {
        path: project_db.display().to_string(),
        role: crate::manifest::DbRole::Project,
        owner: "test".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: Utc::now().to_rfc3339(),
        last_classification: "healthy".to_string(),
        scope_hint: format!("project:{project}"),
        notes: String::new(),
    });
    manifest
        .save(&server.tachi_home_dir().join("manifest.json"))
        .expect("fixture project manifest");

    let stop = CancellationToken::new();
    let _cancel = CancelOnDrop(stop.clone());
    let router = crate::bootstrap::daemon_http_router((*server).clone(), stop.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let shutdown = stop.clone();
    let serving = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .expect("fixture HTTP server");
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("fixture HTTP client");
    let url = format!("http://{address}/mcp");
    let mut headers = HeaderMap::new();
    headers.insert("x-tachi-profile", HeaderValue::from_static("standard"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "x-tachi-project",
        HeaderValue::from_static("runtime-open-fixture"),
    );
    let (initialized, response_headers) = legacy_rpc(
        &client,
        &url,
        &headers,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "briefing-runtime-regression", "version": "1"}},
        }),
    )
    .await
    .expect("legacy project initialization");
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    headers.remove("x-tachi-project");
    headers.insert(
        "mcp-session-id",
        response_headers
            .get("mcp-session-id")
            .expect("legacy session ID")
            .clone(),
    );
    headers.insert(
        "mcp-protocol-version",
        HeaderValue::from_static("2025-11-25"),
    );
    let notified = client
        .post(&url)
        .headers(headers.clone())
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .expect("initialized notification");
    assert!(notified.status().is_success());
    let (tools, _) = legacy_rpc(
        &client,
        &url,
        &headers,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await
    .expect("bound session tools/list");
    assert!(tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .any(|tool| tool["name"] == "tachi_memory"));

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let hook_gate = gate.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let delayed_opens = Arc::new(AtomicUsize::new(0));
    let hook_opens = delayed_opens.clone();
    let expired = Arc::new(AtomicBool::new(false));
    let hook_expired = expired.clone();
    let hook = memcore::private_partition::install_before_generic_open_hook_for_test(
        &project_db,
        move |_| {
            let (released, changed) = &*hook_gate;
            let released = released
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *released {
                return;
            }
            hook_opens.fetch_add(1, Ordering::SeqCst);
            let _ = entered_tx.send(());
            let (released, _) = changed
                .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !*released {
                hook_expired.store(true, Ordering::SeqCst);
            }
        },
    )
    .expect("path-scoped fixture open hook");
    let completed = Arc::new(AtomicBool::new(false));
    let observer_completed = completed.clone();
    let observer_gate = gate.clone();
    let observer = std::thread::spawn(move || {
        let observation = (|| {
            for _ in 0..2 {
                entered_rx
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|error| {
                        format!("both retrieval legs must reach delayed opens: {error}")
                    })?;
            }
            if observer_completed.load(Ordering::SeqCst) {
                return Err("briefing completed before the delayed-open probe".to_string());
            }
            raw_liveness_probe(address)?;
            if observer_completed.load(Ordering::SeqCst) {
                return Err("briefing completed before releasing its project opens".to_string());
            }
            Ok(())
        })();
        // Release on every observation outcome, including the expected RED.
        let (released, changed) = &*observer_gate;
        *released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        changed.notify_all();
        observation
    });
    let briefing = legacy_rpc(
        &client,
        &url,
        &headers,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "tachi_memory", "arguments": {
            "action": "briefing", "compact": true, "format": "json",
            "query": "RuntimeOpenNeedle", "enable_rerank": false,
        }}}),
    )
    .await;
    completed.store(true, Ordering::SeqCst);
    let observation = tokio::task::spawn_blocking(move || observer.join())
        .await
        .expect("observer join task")
        .expect("external observer");
    drop(hook);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("fixture server shutdown deadline")
        .expect("fixture server join");

    assert!(
        !expired.load(Ordering::SeqCst),
        "fixture open wait exceeded its safety bound"
    );
    assert!(
        delayed_opens.load(Ordering::SeqCst) >= 2,
        "both retrieval legs were delayed"
    );
    let (briefing, _) = briefing.expect("briefing completes after project opens are released");
    assert_ne!(briefing["result"]["isError"], json!(true), "{briefing}");
    let content = briefing["result"]["content"]
        .as_array()
        .expect("briefing content");
    let text = content
        .iter()
        .find_map(|block| block.get("text").and_then(Value::as_str))
        .expect("briefing text payload");
    let body: Value = serde_json::from_str(text).expect("briefing JSON payload");
    assert_eq!(body["status"], "completed");
    assert_eq!(
        body["project"], project,
        "session initialization supplied the project"
    );
    observation.expect("production /health/live must respond while both project opens are pending");
}

/// Readiness may wait for a fixture writer, but the store-free liveness route
/// must continue serving on the same production HTTP router.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[allow(clippy::await_holding_lock)]
async fn readiness_contention_keeps_http_live_while_global_writer_is_held() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let ambient = tempfile::tempdir().expect("fixture ambient home");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", ambient.path());
    let server = make_server();
    let stop = CancellationToken::new();
    let _cancel = CancelOnDrop(stop.clone());
    let router = crate::bootstrap::daemon_http_router((*server).clone(), stop.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let shutdown = stop.clone();
    let serving = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .expect("fixture HTTP server");
    });

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let writer_gate = gate.clone();
    let writer_server = (*server).clone();
    let expired = Arc::new(AtomicBool::new(false));
    let writer_expired = expired.clone();
    let (writer_ready_tx, writer_ready_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        writer_server.with_global_store(|_| {
            // This runs after the runtime's global write gate and write-store
            // mutex have been acquired, not merely after thread spawn.
            let _ = writer_ready_tx.send(());
            let (released, changed) = &*writer_gate;
            let released = released
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let (released, _) = changed
                .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !*released {
                writer_expired.store(true, Ordering::SeqCst);
            }
            Ok::<(), String>(())
        })
    });
    let writer_ready =
        tokio::task::spawn_blocking(move || writer_ready_rx.recv_timeout(Duration::from_secs(3)))
            .await
            .expect("writer phase join");
    let (health_entered_tx, health_entered_rx) = mpsc::channel();
    let phase = crate::bootstrap::observe_daemon_health_reads_for_test(
        server.global_db_path_buf(),
        move || {
            let _ = health_entered_tx.send(());
        },
    );
    let completed = Arc::new(AtomicBool::new(false));
    let observer_completed = completed.clone();
    let observer_gate = gate.clone();
    let observer = std::thread::spawn(move || {
        let observation = (|| {
            health_entered_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|error| format!("readiness must enter the global read phase: {error}"))?;
            if observer_completed.load(Ordering::SeqCst) {
                return Err("readiness completed while the global writer was held".to_string());
            }
            raw_liveness_probe(address)
        })();
        // Even the baseline failure must release the writer and drain health.
        let (released, changed) = &*observer_gate;
        *released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        changed.notify_all();
        observation
    });
    let readiness = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("fixture client")
        .get(format!("http://{address}/health"))
        .send()
        .await;
    completed.store(true, Ordering::SeqCst);
    let (observation, write_result) =
        tokio::task::spawn_blocking(move || (observer.join(), writer.join()))
            .await
            .expect("external fixture joins");
    drop(phase);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("fixture server shutdown deadline")
        .expect("fixture server join");

    assert!(
        !expired.load(Ordering::SeqCst),
        "fixture writer exceeded its safety bound"
    );
    writer_ready.expect("fixture writer acquired its gate");
    write_result
        .expect("writer thread")
        .expect("fixture writer");
    assert_eq!(
        readiness
            .expect("readiness completes after release")
            .status(),
        reqwest::StatusCode::OK
    );
    observation
        .expect("observer thread")
        .expect("production /health/live must respond while readiness waits for the global writer");
}

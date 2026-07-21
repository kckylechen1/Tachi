//! #1255 controlled concurrency receipt harness (transport path).
//!
//! Drives serial vs concurrent `call_daemon_tool_raw` / `_with_phases` against
//! an in-process Streamable-HTTP test daemon (same fixture shape as stdio /
//! CLI dispatch tests). Emits machine-readable JSON-line receipts and asserts:
//! - every call is recorded client-side
//! - the HTTP service observes the same unique tool-call markers server-side
//! - serial baseline is predominantly `ok` on a healthy local daemon
//! - concurrent mode produces overlapping client wall times AND ≥2 overlapping
//!   in-flight tool handlers on the server (not merely serializable scheduling)
//!
//! Non-goals (explicit): do not raise `DAEMON_CALL_TIMEOUT`, do not add a
//! session pool, do not change idle-reaper / rate-limit policy.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, InitializeRequestParams, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServerHandler;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::detect::DaemonInfo;
use super::transport::{call_daemon_tool_raw_with_phases, DaemonCallError, DaemonCallPhaseTiming};

/// Allowlisted receipt error tags — never copy arbitrary error text.
pub(crate) const MSG_DAEMON_CALL_TIMEOUT: &str = "daemon_call_timeout";
pub(crate) const MSG_LOOP_DETECTED: &str = "loop_detected";
pub(crate) const MSG_HANDSHAKE_FAILED: &str = "handshake_failed";
pub(crate) const MSG_INVALID_PROXY_PROJECT: &str = "invalid_proxy_project";
pub(crate) const MSG_TOOL_ERROR: &str = "tool_error";
pub(crate) const MSG_OTHER: &str = "other";

/// Outcome class for one transport-path call (no secrets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CallOutcome {
    Ok,
    Timeout,
    RateLimited,
    OtherError,
}

/// One machine-readable receipt line for the #1255 harness.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConcurrencyCallReceipt {
    pub call_id: usize,
    pub mode: &'static str,
    pub caller_idx: usize,
    /// Unix-epoch millis when the call started (local clock).
    pub start_unix_ms: u64,
    /// Unix-epoch millis when the call finished.
    pub end_unix_ms: u64,
    /// Monotonic harness-relative start (ms since harness epoch).
    pub start_rel_ms: u64,
    /// Monotonic harness-relative end.
    pub end_rel_ms: u64,
    pub duration_ms: u64,
    pub handshake_ms: u64,
    pub call_ms: u64,
    pub outcome: CallOutcome,
    /// Allowlisted error tag only (see `MSG_*`); never arbitrary error text.
    pub message_class: Option<String>,
    /// Loopback `host:port` only — no scheme, path, query, fragment, or userinfo.
    pub daemon_endpoint: String,
    /// Opaque hash of the sanitized `host:port` endpoint (never raw URL secrets).
    pub daemon_id: String,
    pub daemon_pid: Option<i64>,
    pub tool: &'static str,
}

/// Server-side observation for the receipt harness (tool-handler enter/exit).
#[derive(Debug)]
struct ReceiptObserver {
    tool_calls: AtomicU64,
    in_flight: AtomicU64,
    max_in_flight: AtomicU64,
    markers: Mutex<HashSet<u64>>,
    /// Brief hold at tool-handler entry so concurrent sessions can latch
    /// overlapping in-flight work. Harness-only; not production behavior.
    hold_ms: u64,
}

impl ReceiptObserver {
    fn new(hold_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            tool_calls: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            max_in_flight: AtomicU64::new(0),
            markers: Mutex::new(HashSet::new()),
            hold_ms,
        })
    }

    fn enter(self: &Arc<Self>, marker: Option<u64>) -> InFlightGuard {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(cur, Ordering::SeqCst);
        if let Some(m) = marker {
            if let Ok(mut set) = self.markers.lock() {
                set.insert(m);
            }
        }
        InFlightGuard {
            observer: Arc::clone(self),
        }
    }

    fn tool_call_count(&self) -> u64 {
        self.tool_calls.load(Ordering::SeqCst)
    }

    fn max_in_flight(&self) -> u64 {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    fn unique_markers(&self) -> HashSet<u64> {
        self.markers.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

struct InFlightGuard {
    observer: Arc<ReceiptObserver>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.observer.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// MemoryServer wrapper that records server-side tool-call observation.
#[derive(Clone)]
struct ObservedServer {
    inner: crate::MemoryServer,
    observer: Arc<ReceiptObserver>,
}

impl ServerHandler for ObservedServer {
    fn get_info(&self) -> ServerInfo {
        self.inner.get_info()
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, rmcp::ErrorData>> + Send + '_ {
        self.inner.initialize(request, context)
    }

    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_ {
        self.inner.list_tools(request, context)
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send + '_ {
        async move {
            let marker = extract_receipt_marker(&request);
            let _guard = self.observer.enter(marker);
            if self.observer.hold_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.observer.hold_ms)).await;
            }
            self.inner.call_tool(request, context).await
        }
    }
}

fn extract_receipt_marker(params: &CallToolRequestParams) -> Option<u64> {
    params
        .arguments
        .as_ref()
        .and_then(|args| args.get("receipt_marker"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
        })
}

/// Strip scheme / path / query / fragment / userinfo — keep `host:port` only.
pub(crate) fn sanitize_daemon_endpoint(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest).trim();
    // Drop userinfo (`user:pass@`) so receipts never fingerprint credentials.
    let host_port = match authority.rsplit_once('@') {
        Some((_, host_port)) => host_port,
        None => authority,
    };
    if host_port.is_empty() {
        return "redacted".into();
    }
    host_port.to_string()
}

/// Opaque daemon identity derived from the sanitized `host:port` endpoint.
/// Never hashes raw URL credentials/query — that would be a stable secret fingerprint.
pub(crate) fn opaque_daemon_id(url: &str) -> String {
    let mut hasher = DefaultHasher::new();
    sanitize_daemon_endpoint(url).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(crate) fn classify_daemon_call_outcome(
    result: &Result<rmcp::model::CallToolResult, DaemonCallError>,
) -> (CallOutcome, Option<String>) {
    match result {
        Ok(tool_result) if tool_result.is_error.unwrap_or(false) => {
            let text = first_text_preview(tool_result);
            if text.contains("Loop detected") {
                (CallOutcome::RateLimited, Some(MSG_LOOP_DETECTED.into()))
            } else {
                (CallOutcome::OtherError, Some(MSG_TOOL_ERROR.into()))
            }
        }
        Ok(_) => (CallOutcome::Ok, None),
        Err(err) => {
            let msg = err.message();
            if msg.contains("timed out") {
                (CallOutcome::Timeout, Some(MSG_DAEMON_CALL_TIMEOUT.into()))
            } else if msg.contains("Loop detected") {
                (CallOutcome::RateLimited, Some(MSG_LOOP_DETECTED.into()))
            } else if msg.contains("handshake failed") {
                (CallOutcome::OtherError, Some(MSG_HANDSHAKE_FAILED.into()))
            } else if msg.contains("invalid proxy project") {
                (
                    CallOutcome::OtherError,
                    Some(MSG_INVALID_PROXY_PROJECT.into()),
                )
            } else {
                (CallOutcome::OtherError, Some(MSG_OTHER.into()))
            }
        }
    }
}

fn first_text_preview(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .find_map(|c| match &c.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

fn is_allowlisted_message_class(class: &str) -> bool {
    matches!(
        class,
        MSG_DAEMON_CALL_TIMEOUT
            | MSG_LOOP_DETECTED
            | MSG_HANDSHAKE_FAILED
            | MSG_INVALID_PROXY_PROJECT
            | MSG_TOOL_ERROR
            | MSG_OTHER
    )
}

pub(crate) fn intervals_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    a_start < b_end && b_start < a_end
}

pub(crate) fn concurrent_receipts_overlap(receipts: &[ConcurrencyCallReceipt]) -> bool {
    let concurrent: Vec<_> = receipts.iter().filter(|r| r.mode == "concurrent").collect();
    for (i, a) in concurrent.iter().enumerate() {
        for b in concurrent.iter().skip(i + 1) {
            if intervals_overlap(a.start_rel_ms, a.end_rel_ms, b.start_rel_ms, b.end_rel_ms) {
                return true;
            }
        }
    }
    false
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn rel_ms(epoch: Instant) -> u64 {
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn runtime_info_params(call_id: usize) -> CallToolRequestParams {
    let mut params = CallToolRequestParams::new("runtime_info".to_string());
    let mut args = serde_json::Map::new();
    args.insert("receipt_marker".into(), serde_json::json!(call_id as u64));
    params.arguments = Some(args);
    params
}

async fn record_runtime_info_call(
    info: DaemonInfo,
    call_id: usize,
    mode: &'static str,
    caller_idx: usize,
    harness_epoch: Instant,
) -> ConcurrencyCallReceipt {
    let start_unix_ms = unix_ms_now();
    let start_rel_ms = rel_ms(harness_epoch);
    let params = runtime_info_params(call_id);
    let (result, phases) = call_daemon_tool_raw_with_phases(&info, params, None).await;
    let end_unix_ms = unix_ms_now();
    let end_rel_ms = rel_ms(harness_epoch);
    let (outcome, message_class) = classify_daemon_call_outcome(&result);
    if let Some(ref class) = message_class {
        debug_assert!(
            is_allowlisted_message_class(class),
            "message_class must be allowlisted: {class}"
        );
    }
    let DaemonCallPhaseTiming {
        handshake_ms,
        call_ms,
        total_ms,
    } = phases;

    ConcurrencyCallReceipt {
        call_id,
        mode,
        caller_idx,
        start_unix_ms,
        end_unix_ms,
        start_rel_ms,
        end_rel_ms,
        duration_ms: total_ms.max(end_rel_ms.saturating_sub(start_rel_ms)),
        handshake_ms,
        call_ms,
        outcome,
        message_class,
        daemon_endpoint: sanitize_daemon_endpoint(&info.url),
        daemon_id: opaque_daemon_id(&info.url),
        daemon_pid: info.pid,
        tool: "runtime_info",
    }
}

fn emit_receipt_jsonl(receipts: &[ConcurrencyCallReceipt]) {
    for receipt in receipts {
        let line = serde_json::to_string(receipt).expect("receipt json");
        // Machine-readable receipt stream for harness consumers / CI logs.
        println!("1255_concurrency_receipt {line}");
    }
}

async fn spawn_receipt_http_daemon(
    server: crate::MemoryServer,
    global_db_path: &Path,
    observer: Arc<ReceiptObserver>,
) -> (DaemonInfo, CancellationToken, tokio::task::JoinHandle<()>) {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind receipt daemon listener");
    let local_addr = listener.local_addr().expect("local addr");
    let ct = CancellationToken::new();
    let ct_shutdown = ct.clone();

    let mut http_config = StreamableHttpServerConfig::default();
    http_config.stateful_mode = true;
    http_config.cancellation_token = ct.child_token();

    // Fresh MCP session id per Streamable connection so concurrent callers do
    // not share one burst window (post-#1328 per-session limiter). ObservedServer
    // wraps each session clone and records tool-handler enter/exit.
    let service = StreamableHttpService::new(
        move || {
            Ok(ObservedServer {
                inner: server.clone_for_mcp_session(),
                observer: Arc::clone(&observer),
            })
        },
        Arc::new(LocalSessionManager::default()),
        http_config,
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { ct_shutdown.cancelled_owned().await })
            .await
            .expect("receipt daemon axum::serve failed");
    });

    (
        DaemonInfo {
            url: format!("http://{local_addr}/mcp"),
            global_db: Some(global_db_path.display().to_string()),
            project_db: None,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            pid: Some(std::process::id() as i64),
        },
        ct,
        handle,
    )
}

async fn wait_until_server_saw(observer: &ReceiptObserver, expected: u64, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let seen = observer.tool_call_count();
        if seen >= expected {
            return;
        }
        if Instant::now() >= deadline {
            panic!("server did not observe {expected} {label} tool calls within 5s; saw {seen}");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Poll TCP accept readiness instead of a fixed sleep (review non-blocking #1338).
async fn wait_for_daemon_tcp_ready(info: &DaemonInfo) {
    let endpoint = sanitize_daemon_endpoint(&info.url);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match tokio::net::TcpStream::connect(&endpoint).await {
            Ok(_) => return,
            Err(err) if Instant::now() >= deadline => {
                panic!("receipt daemon TCP not ready at {endpoint} within 2s: {err}");
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
        }
    }
}

/// Cancel the daemon token on panic/early return so teardown is not skipped
/// when assertions fail after spawn (review non-blocking #1338).
struct CancelDaemonOnDrop(CancellationToken);

impl Drop for CancelDaemonOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn run_serial_baseline(
    info: &DaemonInfo,
    n: usize,
    harness_epoch: Instant,
    next_id: &mut usize,
) -> Vec<ConcurrencyCallReceipt> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let id = *next_id;
        *next_id += 1;
        out.push(record_runtime_info_call(info.clone(), id, "serial", 0, harness_epoch).await);
    }
    out
}

async fn run_concurrent_burst(
    info: &DaemonInfo,
    callers: usize,
    calls_per_caller: usize,
    harness_epoch: Instant,
    next_id: &mut usize,
) -> Vec<ConcurrencyCallReceipt> {
    let mut handles = Vec::with_capacity(callers * calls_per_caller);
    for caller_idx in 0..callers {
        for _ in 0..calls_per_caller {
            let id = *next_id;
            *next_id += 1;
            let info = info.clone();
            handles.push(tokio::spawn(async move {
                record_runtime_info_call(info, id, "concurrent", caller_idx, harness_epoch).await
            }));
        }
    }
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.expect("join concurrent receipt task"));
    }
    out.sort_by_key(|r| r.call_id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_uses_allowlisted_message_classes_only() {
        let timeout = Err(DaemonCallError::AfterDispatch(
            "daemon call 'runtime_info' timed out after 60s".into(),
        ));
        let (outcome, class) = classify_daemon_call_outcome(&timeout);
        assert_eq!(outcome, CallOutcome::Timeout);
        assert_eq!(class.as_deref(), Some(MSG_DAEMON_CALL_TIMEOUT));

        let limited = Err(DaemonCallError::AfterDispatch(
            "Loop detected: identical runtime_info calls".into(),
        ));
        assert_eq!(
            classify_daemon_call_outcome(&limited).0,
            CallOutcome::RateLimited
        );
        assert_eq!(
            classify_daemon_call_outcome(&limited).1.as_deref(),
            Some(MSG_LOOP_DETECTED)
        );

        let handshake = Err(DaemonCallError::BeforeDispatch(
            "daemon handshake failed at http://127.0.0.1:9/mcp?token=secret: connect".into(),
        ));
        let (outcome, class) = classify_daemon_call_outcome(&handshake);
        assert_eq!(outcome, CallOutcome::OtherError);
        assert_eq!(class.as_deref(), Some(MSG_HANDSHAKE_FAILED));

        let proxy = Err(DaemonCallError::BeforeDispatch(
            "invalid proxy project header value: bad".into(),
        ));
        assert_eq!(
            classify_daemon_call_outcome(&proxy).1.as_deref(),
            Some(MSG_INVALID_PROXY_PROJECT)
        );

        let other = Err(DaemonCallError::AfterDispatch(
            "daemon call 'runtime_info' failed: totally-arbitrary-secret-text".into(),
        ));
        let (outcome, class) = classify_daemon_call_outcome(&other);
        assert_eq!(outcome, CallOutcome::OtherError);
        assert_eq!(class.as_deref(), Some(MSG_OTHER));
        assert!(!class.unwrap().contains("secret"));
    }

    #[test]
    fn sanitize_daemon_endpoint_strips_path_query_and_userinfo() {
        assert_eq!(
            sanitize_daemon_endpoint("http://127.0.0.1:1234/mcp"),
            "127.0.0.1:1234"
        );
        assert_eq!(
            sanitize_daemon_endpoint("http://127.0.0.1:1234/mcp?token=sekret#frag"),
            "127.0.0.1:1234"
        );
        assert_eq!(
            sanitize_daemon_endpoint("http://user:pass@127.0.0.1:9/mcp"),
            "127.0.0.1:9"
        );
        let id = opaque_daemon_id("http://127.0.0.1:9/mcp?token=x");
        assert_eq!(id.len(), 16);
        assert!(!id.contains("token"));
    }

    #[test]
    fn opaque_daemon_id_ignores_credentials_query_and_fragment() {
        let base = opaque_daemon_id("http://127.0.0.1:4242/mcp");
        assert_eq!(
            opaque_daemon_id("http://user:sekret@127.0.0.1:4242/mcp"),
            base,
            "userinfo must not change daemon_id"
        );
        assert_eq!(
            opaque_daemon_id("http://127.0.0.1:4242/mcp?token=sekret"),
            base,
            "query must not change daemon_id"
        );
        assert_eq!(
            opaque_daemon_id("http://127.0.0.1:4242/mcp#frag"),
            base,
            "fragment must not change daemon_id"
        );
        assert_eq!(
            opaque_daemon_id("http://other:pass@127.0.0.1:4242/other?q=1#x"),
            base,
            "combined credential/query/fragment/path noise must not change daemon_id"
        );
        let other_port = opaque_daemon_id("http://127.0.0.1:4243/mcp");
        assert_ne!(
            other_port, base,
            "different ports must yield different daemon_ids"
        );
        assert!(!base.contains("sekret"));
        assert!(!base.contains("token"));
        assert!(!base.contains("user"));
    }

    #[test]
    fn receipt_json_shape_has_required_fields_without_secrets() {
        let receipt = ConcurrencyCallReceipt {
            call_id: 1,
            mode: "serial",
            caller_idx: 0,
            start_unix_ms: 1_000,
            end_unix_ms: 1_050,
            start_rel_ms: 0,
            end_rel_ms: 50,
            duration_ms: 50,
            handshake_ms: 30,
            call_ms: 20,
            outcome: CallOutcome::Ok,
            message_class: None,
            daemon_endpoint: "127.0.0.1:1234".into(),
            daemon_id: "abcd1234abcd1234".into(),
            daemon_pid: Some(42),
            tool: "runtime_info",
        };
        let value = serde_json::to_value(&receipt).expect("serialize");
        assert_eq!(value["call_id"], json!(1));
        assert_eq!(value["mode"], json!("serial"));
        assert_eq!(value["outcome"], json!("ok"));
        assert_eq!(value["handshake_ms"], json!(30));
        assert_eq!(value["call_ms"], json!(20));
        assert_eq!(value["tool"], json!("runtime_info"));
        assert_eq!(value["daemon_endpoint"], json!("127.0.0.1:1234"));
        assert!(value.get("daemon_url").is_none());
        assert!(value.get("arguments").is_none());
        assert!(value.get("token").is_none());
        assert!(value.get("authorization").is_none());
        let dumped = value.to_string();
        assert!(!dumped.contains("http://"));
        assert!(!dumped.contains("/mcp"));
    }

    #[test]
    fn concurrent_overlap_detector_requires_real_interval_overlap() {
        let a = ConcurrencyCallReceipt {
            call_id: 0,
            mode: "concurrent",
            caller_idx: 0,
            start_unix_ms: 0,
            end_unix_ms: 0,
            start_rel_ms: 0,
            end_rel_ms: 100,
            duration_ms: 100,
            handshake_ms: 10,
            call_ms: 90,
            outcome: CallOutcome::Ok,
            message_class: None,
            daemon_endpoint: "127.0.0.1:1".into(),
            daemon_id: "deadbeefdeadbeef".into(),
            daemon_pid: None,
            tool: "runtime_info",
        };
        let mut b = a.clone();
        b.call_id = 1;
        b.caller_idx = 1;
        b.start_rel_ms = 40;
        b.end_rel_ms = 140;
        assert!(concurrent_receipts_overlap(&[a.clone(), b.clone()]));

        b.start_rel_ms = 100;
        b.end_rel_ms = 200;
        assert!(!concurrent_receipts_overlap(&[a, b]));
    }

    /// Live transport-path harness: serial baseline + concurrent burst via
    /// `call_daemon_tool_raw_with_phases` against an in-process test daemon
    /// with server-side call observation.
    ///
    /// Run:
    /// ```text
    /// cargo test -p tachi-server --lib concurrency_receipt
    /// ```
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::await_holding_lock)]
    async fn transport_concurrency_receipt_serial_and_burst() {
        crate::ensure_tls_provider();

        // Isolate app home: MemoryServer::new → path_utils::tachi_home() →
        // LlmCallRecorder writes `<home>/foundry-runs/`. Without a temp
        // TACHI_HOME + env lock, nextest would mutate the real ~/.tachi.
        let _env_lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp_home = tempfile::tempdir().expect("temp TACHI_HOME");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let _sigil_home = crate::test_support::EnvRestore::remove("SIGIL_HOME");
        let _app_home = crate::test_support::EnvRestore::remove("TACHI_APP_HOME");

        let global: PathBuf = temp_home.path().join("global/memory.db");
        std::fs::create_dir_all(global.parent().expect("parent")).expect("mkdir");
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        assert_eq!(
            crate::path_utils::tachi_home(),
            temp_home.path(),
            "harness must bind tachi_home to the tempdir, not ambient ~/.tachi"
        );
        assert_eq!(
            server.tachi_home_dir(),
            temp_home.path(),
            "MemoryServer home_dir must match isolated TACHI_HOME"
        );
        let foundry_runs = temp_home.path().join("foundry-runs");
        assert!(
            foundry_runs.is_dir(),
            "LlmCallRecorder must create foundry-runs under the temp home; path={}",
            foundry_runs.display()
        );

        // 40ms enter-hold widens the overlap window so concurrent sessions can
        // prove a real in-flight high-water mark ≥ 2 on the tool handler.
        let observer = ReceiptObserver::new(40);
        let (daemon, ct, daemon_task) =
            spawn_receipt_http_daemon(server, &global, Arc::clone(&observer)).await;
        let _cancel_on_drop = CancelDaemonOnDrop(ct.clone());

        wait_for_daemon_tcp_ready(&daemon).await;

        let harness_epoch = Instant::now();
        let mut next_id = 0usize;

        const SERIAL_N: usize = 4;
        const CALLERS: usize = 2;
        const CALLS_PER_CALLER: usize = 3;

        let mut serial = run_serial_baseline(&daemon, SERIAL_N, harness_epoch, &mut next_id).await;
        wait_until_server_saw(&observer, SERIAL_N as u64, "serial").await;
        assert_eq!(
            observer.tool_call_count(),
            SERIAL_N as u64,
            "server-observed serial tool calls must equal client-emitted receipts"
        );

        let max_after_serial = observer.max_in_flight();
        let mut concurrent = run_concurrent_burst(
            &daemon,
            CALLERS,
            CALLS_PER_CALLER,
            harness_epoch,
            &mut next_id,
        )
        .await;

        let expected = SERIAL_N + CALLERS * CALLS_PER_CALLER;
        wait_until_server_saw(&observer, expected as u64, "total").await;

        assert_eq!(
            serial.len() + concurrent.len(),
            expected,
            "harness must record every call"
        );
        assert_eq!(
            observer.tool_call_count(),
            expected as u64,
            "server-observed tool calls must equal client-emitted receipt count"
        );

        let markers = observer.unique_markers();
        assert_eq!(
            markers.len(),
            expected,
            "server must see every unique receipt_marker; markers={markers:?}"
        );
        for receipt in serial.iter().chain(concurrent.iter()) {
            assert!(
                markers.contains(&(receipt.call_id as u64)),
                "missing server marker for call_id={}",
                receipt.call_id
            );
        }

        let serial_ok = serial
            .iter()
            .filter(|r| r.outcome == CallOutcome::Ok)
            .count();
        assert!(
            serial_ok * 100 / SERIAL_N >= 75,
            "serial baseline should be predominantly ok on a healthy local daemon; \
             ok={serial_ok}/{SERIAL_N} receipts={serial:?}"
        );

        assert!(
            concurrent_receipts_overlap(&concurrent),
            "concurrent mode must produce overlapping wall times; receipts={concurrent:?}"
        );

        let max_in_flight = observer.max_in_flight();
        assert!(
            max_in_flight >= 2,
            "server must accept ≥2 overlapping in-flight tool calls during concurrent burst \
             (max_in_flight={max_in_flight}, max_after_serial={max_after_serial}); \
             if this fails the stack may be serializing call_tool — do not weaken this gate"
        );

        let expected_endpoint = sanitize_daemon_endpoint(&daemon.url);
        let expected_daemon_id = opaque_daemon_id(&daemon.url);
        let mut seen_call_ids = HashSet::new();
        for receipt in serial.iter().chain(concurrent.iter()) {
            // Pin the full receipt shape (review non-blocking #1338).
            assert!(
                seen_call_ids.insert(receipt.call_id),
                "duplicate call_id in receipts: {}",
                receipt.call_id
            );
            assert!(
                matches!(receipt.mode, "serial" | "concurrent"),
                "mode must be serial|concurrent: {:?}",
                receipt.mode
            );
            assert_eq!(receipt.tool, "runtime_info");
            assert_eq!(receipt.daemon_endpoint, expected_endpoint);
            assert_eq!(receipt.daemon_id, expected_daemon_id);
            assert_eq!(receipt.daemon_id.len(), 16);
            assert_eq!(receipt.daemon_pid, daemon.pid);
            assert!(!receipt.daemon_endpoint.contains("://"));
            assert!(!receipt.daemon_endpoint.contains('/'));
            assert!(receipt.end_unix_ms >= receipt.start_unix_ms);
            assert!(receipt.end_rel_ms >= receipt.start_rel_ms);
            assert!(receipt.duration_ms > 0 || receipt.outcome != CallOutcome::Ok);
            if let Some(ref class) = receipt.message_class {
                assert!(
                    is_allowlisted_message_class(class),
                    "non-allowlisted message_class: {class}"
                );
            }
            // Successful calls should report a measurable handshake phase —
            // this is the #1255 evidence leaf for later transport-reuse work.
            if receipt.outcome == CallOutcome::Ok {
                assert!(
                    receipt.handshake_ms > 0 || receipt.call_ms > 0 || receipt.duration_ms > 0,
                    "ok receipt should carry timing: {receipt:?}"
                );
            }
        }
        assert_eq!(seen_call_ids.len(), expected);

        let mut all = Vec::with_capacity(expected);
        all.append(&mut serial);
        all.append(&mut concurrent);
        emit_receipt_jsonl(&all);

        let timeout_count = all
            .iter()
            .filter(|r| r.outcome == CallOutcome::Timeout)
            .count();
        // Observational: in-process short daemon should not hit the 60s ceiling.
        assert_eq!(
            timeout_count, 0,
            "unexpected DAEMON_CALL_TIMEOUT under in-process harness; receipts={all:?}"
        );

        println!(
            "1255_concurrency_receipt_server_obs tool_calls={} max_in_flight={} unique_markers={} tachi_home={}",
            observer.tool_call_count(),
            max_in_flight,
            markers.len(),
            temp_home.path().display()
        );

        // Explicit graceful join (CancelDaemonOnDrop also cancels on panic).
        ct.cancel();
        let join = tokio::time::timeout(Duration::from_secs(2), daemon_task)
            .await
            .expect("receipt daemon shutdown timed out after 2s");
        join.expect("receipt daemon task join failed");
    }
}

//! #1255 controlled concurrency receipt harness (transport path).
//!
//! Drives serial vs concurrent `call_daemon_tool_raw` / `_with_phases` against
//! an in-process Streamable-HTTP test daemon (same fixture shape as stdio /
//! CLI dispatch tests). Emits machine-readable JSON-line receipts and asserts:
//! - every call is recorded
//! - serial baseline is predominantly `ok` on a healthy local daemon
//! - concurrent mode produces overlapping wall-time intervals
//!
//! Non-goals (explicit): do not raise `DAEMON_CALL_TIMEOUT`, do not add a
//! session pool, do not change idle-reaper / rate-limit policy.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::model::CallToolRequestParams;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::detect::DaemonInfo;
use super::transport::{
    call_daemon_tool_raw_with_phases, DaemonCallError, DaemonCallPhaseTiming,
};

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
    /// Short non-secret message class (error prefix / tag), never full payloads.
    pub message_class: Option<String>,
    pub daemon_url: String,
    pub daemon_pid: Option<i64>,
    pub tool: &'static str,
}

pub(crate) fn classify_daemon_call_outcome(
    result: &Result<rmcp::model::CallToolResult, DaemonCallError>,
) -> (CallOutcome, Option<String>) {
    match result {
        Ok(tool_result) if tool_result.is_error.unwrap_or(false) => {
            let text = first_text_preview(tool_result);
            if text.contains("Loop detected") {
                (CallOutcome::RateLimited, Some("loop_detected".into()))
            } else {
                (CallOutcome::OtherError, Some(message_class(&text)))
            }
        }
        Ok(_) => (CallOutcome::Ok, None),
        Err(err) => {
            let msg = err.message();
            if msg.contains("timed out") {
                (CallOutcome::Timeout, Some("daemon_call_timeout".into()))
            } else if msg.contains("Loop detected") {
                (CallOutcome::RateLimited, Some("loop_detected".into()))
            } else if msg.contains("handshake failed") {
                (CallOutcome::OtherError, Some("handshake_failed".into()))
            } else if msg.contains("invalid proxy project") {
                (CallOutcome::OtherError, Some("invalid_proxy_project".into()))
            } else {
                (CallOutcome::OtherError, Some(message_class(msg)))
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

fn message_class(msg: &str) -> String {
    let head = msg.split(':').next().unwrap_or(msg).trim();
    head.chars().take(64).collect()
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

async fn record_runtime_info_call(
    info: DaemonInfo,
    call_id: usize,
    mode: &'static str,
    caller_idx: usize,
    harness_epoch: Instant,
) -> ConcurrencyCallReceipt {
    let start_unix_ms = unix_ms_now();
    let start_rel_ms = rel_ms(harness_epoch);
    let params = CallToolRequestParams::new("runtime_info".to_string());
    let (result, phases) = call_daemon_tool_raw_with_phases(&info, params, None).await;
    let end_unix_ms = unix_ms_now();
    let end_rel_ms = rel_ms(harness_epoch);
    let (outcome, message_class) = classify_daemon_call_outcome(&result);
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
        daemon_url: info.url,
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
) -> (
    DaemonInfo,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
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
    // not share one burst window (post-#1328 per-session limiter).
    let service = StreamableHttpService::new(
        move || Ok(server.clone_for_mcp_session()),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { ct_shutdown.cancelled_owned().await })
            .await;
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
    fn classify_timeout_and_rate_limit_message_classes() {
        let timeout = Err(DaemonCallError::AfterDispatch(
            "daemon call 'runtime_info' timed out after 60s".into(),
        ));
        assert_eq!(
            classify_daemon_call_outcome(&timeout).0,
            CallOutcome::Timeout
        );

        let limited = Err(DaemonCallError::AfterDispatch(
            "Loop detected: identical runtime_info calls".into(),
        ));
        assert_eq!(
            classify_daemon_call_outcome(&limited).0,
            CallOutcome::RateLimited
        );

        let handshake = Err(DaemonCallError::BeforeDispatch(
            "daemon handshake failed at http://127.0.0.1:9/mcp: connect".into(),
        ));
        let (outcome, class) = classify_daemon_call_outcome(&handshake);
        assert_eq!(outcome, CallOutcome::OtherError);
        assert_eq!(class.as_deref(), Some("handshake_failed"));
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
            daemon_url: "http://127.0.0.1:1234/mcp".into(),
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
        assert!(value.get("arguments").is_none());
        assert!(value.get("token").is_none());
        assert!(value.get("authorization").is_none());
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
            daemon_url: "http://127.0.0.1:1/mcp".into(),
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
    /// `call_daemon_tool_raw_with_phases` against an in-process test daemon.
    ///
    /// Run:
    /// ```text
    /// cargo test -p tachi-server --lib concurrency_receipt
    /// ```
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn transport_concurrency_receipt_serial_and_burst() {
        crate::ensure_tls_provider();

        let temp = tempfile::tempdir().expect("tempdir");
        let global: PathBuf = temp.path().join("global/memory.db");
        std::fs::create_dir_all(global.parent().expect("parent")).expect("mkdir");
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_receipt_http_daemon(server, &global).await;

        // Give the axum listener a beat to accept.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let harness_epoch = Instant::now();
        let mut next_id = 0usize;

        const SERIAL_N: usize = 4;
        const CALLERS: usize = 2;
        const CALLS_PER_CALLER: usize = 3;

        let mut serial = run_serial_baseline(&daemon, SERIAL_N, harness_epoch, &mut next_id).await;
        let mut concurrent =
            run_concurrent_burst(&daemon, CALLERS, CALLS_PER_CALLER, harness_epoch, &mut next_id)
                .await;

        let expected = SERIAL_N + CALLERS * CALLS_PER_CALLER;
        assert_eq!(
            serial.len() + concurrent.len(),
            expected,
            "harness must record every call"
        );

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

        for receipt in serial.iter().chain(concurrent.iter()) {
            assert_eq!(receipt.tool, "runtime_info");
            assert_eq!(receipt.daemon_url, daemon.url);
            assert_eq!(receipt.daemon_pid, daemon.pid);
            // Successful calls should report a measurable handshake phase —
            // this is the #1255 evidence leaf for later transport-reuse work.
            if receipt.outcome == CallOutcome::Ok {
                assert!(
                    receipt.handshake_ms > 0 || receipt.call_ms > 0 || receipt.duration_ms > 0,
                    "ok receipt should carry timing: {receipt:?}"
                );
            }
        }

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

        ct.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), daemon_task).await;
    }
}

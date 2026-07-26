use super::handler::{
    bounded_board_limit, bounded_kanban_fetch_limit, bounded_run_scan_limit,
    BOARD_KANBAN_FETCH_CANDIDATE_HARD_MAX, BOARD_RETURN_LIMIT_HARD_MAX,
};
use super::*;
use crate::dispatch_ops::probe_harness_server_status;
use crate::test_support::{spawn_opencode_probe_server, EnvRestore};
use crate::tool_params::TachiBoardParams;
use chrono::{Duration, Utc};
use serde_json::json;
use std::ffi::OsStr;
use std::io::{Read, Write};

fn board_entry(id: &str, state: &str, timestamp: String) -> memcore::MemoryEntry {
    memcore::MemoryEntry {
        id: id.to_string(),
        path: format!("/kanban/tasks/{id}"),
        summary: format!("board row {id}"),
        text: "Dispatch Task".to_string(),
        importance: 0.7,
        timestamp: timestamp.clone(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "kanban".to_string(),
        keywords: vec!["kanban".to_string(), "dispatch".to_string()],
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "test".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        metadata: json!({
            "type": "a2a_task",
            "dispatch_id": id,
            "a2a_state": state,
            "updated_at": timestamp,
        }),
        retention_policy: Some(memcore::RetentionPolicy::Pinned.as_str().to_string()),
        domain: Some("system".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

fn seed_board_entries(server: &crate::MemoryServer, entries: Vec<memcore::MemoryEntry>) {
    server
        .with_global_store(|store| {
            for entry in entries {
                store
                    .upsert(&entry)
                    .map_err(|error| format!("seed board row: {error}"))?;
            }
            Ok(())
        })
        .expect("seed board entries");
}

#[test]
fn dispatch_timestamp_key_extracts_embedded_timestamp() {
    assert_eq!(
        dispatch_timestamp_key(OsStr::new("flow_20260606T151945Z_tachi")),
        Some("20260606T151945Z".to_string())
    );
    assert_eq!(
        dispatch_timestamp_key(OsStr::new("20260607T045032Z-codex-09802a7f")),
        Some("20260607T045032Z".to_string())
    );
    assert_eq!(dispatch_timestamp_key(OsStr::new("mcp-smoke-test")), None);
}

#[test]
fn board_limits_cap_oversized_requests_and_keep_zero_explicit() {
    assert_eq!(bounded_board_limit(Some(0)), 0);
    assert_eq!(
        bounded_board_limit(Some(usize::MAX)),
        BOARD_RETURN_LIMIT_HARD_MAX,
        "the public board limit must never exceed its named hard maximum"
    );
    assert_eq!(
        bounded_run_scan_limit(usize::MAX),
        super::runs::BOARD_RUN_DIRECTORY_CANDIDATE_HARD_MAX,
        "derived scan work must saturate before the directory-inspection maximum"
    );
    assert_eq!(
        bounded_kanban_fetch_limit(usize::MAX),
        BOARD_KANBAN_FETCH_CANDIDATE_HARD_MAX,
        "derived indexed-ledger work must saturate before its named fetch maximum"
    );
}

#[tokio::test]
async fn board_zero_limit_returns_before_any_row_collection() {
    let server = crate::tests::make_server();
    let raw = handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: None,
            limit: Some(0),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("zero-limit board response");
    let board: serde_json::Value = serde_json::from_str(&raw).expect("board JSON");

    assert_eq!(board["limit"], json!(0));
    assert_eq!(board["count"], json!(0));
    assert_eq!(board["tasks"], json!([]));
}

#[tokio::test]
async fn board_oversized_limit_reports_the_hard_maximum() {
    let server = crate::tests::make_server();
    let raw = handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(usize::MAX),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("oversized-limit board response");
    let board: serde_json::Value = serde_json::from_str(&raw).expect("board JSON");

    assert_eq!(board["limit"], json!(BOARD_RETURN_LIMIT_HARD_MAX));
    assert!(
        board["count"].as_u64().unwrap_or_default() <= BOARD_RETURN_LIMIT_HARD_MAX as u64,
        "board response exceeded its hard maximum: {board:#}"
    );
}

#[tokio::test]
async fn board_applies_limit_after_state_filtering_and_default_terminal_folding() {
    let (server, _temp_home) = crate::tests::make_server_with_temp_home();
    let base = Utc::now() + Duration::minutes(10);
    seed_board_entries(
        &server,
        vec![
            board_entry("older-active", "TASK_STATE_WORKING", base.to_rfc3339()),
            board_entry(
                "newer-active",
                "TASK_STATE_PENDING",
                (base + Duration::seconds(1)).to_rfc3339(),
            ),
            board_entry(
                "new-terminal",
                "TASK_STATE_COMPLETED",
                (base + Duration::seconds(2)).to_rfc3339(),
            ),
            board_entry(
                "newest-terminal",
                "TASK_STATE_COMPLETED",
                (base + Duration::seconds(3)).to_rfc3339(),
            ),
        ],
    );

    let active_raw = handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("active".to_string()),
            limit: Some(2),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("active board response");
    let active: serde_json::Value = serde_json::from_str(&active_raw).expect("active board JSON");
    let active_ids: Vec<_> = active["tasks"]
        .as_array()
        .expect("active tasks")
        .iter()
        .filter_map(|task| task["dispatch_id"].as_str())
        .collect();
    assert_eq!(
        active_ids,
        vec!["newer-active", "older-active"],
        "newer nonmatching terminal rows must not consume the visible active-row limit: {active:#}"
    );

    let default_raw = handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: None,
            limit: Some(2),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("default board response");
    let default: serde_json::Value =
        serde_json::from_str(&default_raw).expect("default board JSON");
    assert_eq!(default["count"], json!(2), "{default:#}");
    assert!(
        default["tasks"]
            .as_array()
            .expect("default tasks")
            .iter()
            .any(|task| task["dispatch_id"] == json!("newer-active")),
        "terminal folding must happen before the visible limit is applied: {default:#}"
    );
    assert!(
        default["tasks"]
            .as_array()
            .expect("default tasks")
            .iter()
            .any(|task| task["folded"] == json!(true)),
        "default terminal rows must remain represented by a folded row: {default:#}"
    );
}

#[tokio::test]
async fn board_response_marks_bounded_run_fallback_as_incomplete_and_counts_probe() {
    let (server, _temp_home) = crate::tests::make_server_with_temp_home();
    let runs_dir = super::paths::runs_dir_for_server(&server);
    std::fs::create_dir_all(&runs_dir).expect("create runs dir");
    let scan_limit = bounded_run_scan_limit(1);
    for index in 0..=scan_limit {
        std::fs::write(runs_dir.join(format!("ignored-{index:04}")), "fixture")
            .expect("write fallback fixture");
    }

    let raw = handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(1),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("bounded fallback board response");
    let board: serde_json::Value = serde_json::from_str(&raw).expect("board JSON");

    assert_eq!(board["run_scan_limit"], json!(scan_limit), "{board:#}");
    assert_eq!(
        board["run_scan_inspected"],
        json!(scan_limit + 1),
        "the truncation probe is filesystem inspection work and must be in the budget: {board:#}"
    );
    assert_eq!(board["run_fallback_incomplete"], json!(true), "{board:#}");
    assert_eq!(board["incomplete"], json!(true), "{board:#}");
    assert!(
        board["warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("directory order is not a recency index")),
        "the response must not claim exact-newest fallback rows after bounded read_dir sampling: {board:#}"
    );
}

#[tokio::test]
async fn board_marks_hard_capped_kanban_fetch_when_filtered_rows_cannot_fill_limit() {
    let (server, _temp_home) = crate::tests::make_server_with_temp_home();
    let base = Utc::now() + Duration::minutes(10);
    let mut entries = vec![board_entry(
        "active-beyond-fetch-cap",
        "TASK_STATE_WORKING",
        base.to_rfc3339(),
    )];
    for index in 0..=BOARD_KANBAN_FETCH_CANDIDATE_HARD_MAX {
        entries.push(board_entry(
            &format!("newer-terminal-{index:04}"),
            "TASK_STATE_COMPLETED",
            (base + Duration::seconds(index as i64 + 1)).to_rfc3339(),
        ));
    }
    seed_board_entries(&server, entries);

    let raw = handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("active".to_string()),
            limit: Some(usize::MAX),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("hard-capped board response");
    let board: serde_json::Value = serde_json::from_str(&raw).expect("board JSON");

    assert_eq!(
        board["kanban_fetch_limit"],
        json!(BOARD_KANBAN_FETCH_CANDIDATE_HARD_MAX),
        "{board:#}"
    );
    assert_eq!(
        board["limit"],
        json!(BOARD_RETURN_LIMIT_HARD_MAX),
        "{board:#}"
    );
    assert_eq!(board["kanban_fetch_truncated"], json!(true), "{board:#}");
    assert_eq!(board["limit_incomplete"], json!(true), "{board:#}");
    assert_eq!(board["incomplete"], json!(true), "{board:#}");
    assert_eq!(board["count"], json!(0), "{board:#}");
}

#[cfg(unix)]
#[tokio::test]
async fn board_flow_lookup_surfaces_symlinked_status_as_an_error() {
    let (server, _temp_home) = crate::tests::make_server_with_temp_home();
    let flow_id = "flow_20260725T000000Z_board_symlink";
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    let outside_dir = tempfile::tempdir().expect("outside flow fixture");
    let outside = outside_dir.path().join("outside-flow-status.json");
    std::fs::write(&outside, json!({"dispatch_ids": []}).to_string())
        .expect("write outside flow status");
    std::os::unix::fs::symlink(&outside, run_dir.join("status.json")).expect("symlink flow status");

    let error = handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(1),
            project: None,
            flow_id: Some(flow_id.to_string()),
            verbose: None,
        },
    )
    .await
    .expect_err("symlinked flow status must fail loudly");
    assert!(
        error.contains("resolves outside containment root"),
        "unexpected flow status error: {error}"
    );
}

#[test]
fn harness_probe_rejects_non_local_urls() {
    let status = probe_harness_server_status(Some("https://example.com:4321"));
    assert_eq!(status["reachable"], serde_json::Value::Null);
    assert_eq!(status["evidence_strength"], json!("none"));
}

#[test]
fn harness_probe_labels_tcp_only_as_weak_evidence() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local listener");
    let port = listener.local_addr().expect("local addr").port();

    let status = probe_harness_server_status(Some(&format!("http://127.0.0.1:{port}")));

    assert_eq!(status["reachable"], json!(true));
    assert_eq!(status["probe"], json!("tcp"));
    assert_eq!(status["evidence_strength"], json!("weak"));
    assert_eq!(status["readiness"], json!("tcp_only"));
    assert!(
        status["warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("OpenCode API version")),
        "TCP-only probe must explain what it did not verify: {status:#}"
    );
}

#[test]
fn harness_probe_reports_http_responsive_readiness() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local listener");
    let port = listener.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept probe");
        let mut buf = [0_u8; 1024];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\nopencode service")
            .expect("write response");
    });

    let status = probe_harness_server_status(Some(&format!("http://127.0.0.1:{port}")));
    server.join().expect("probe server thread");

    assert_eq!(status["reachable"], json!(true));
    assert_eq!(status["probe"], json!("http"));
    assert_eq!(status["evidence_strength"], json!("medium"));
    assert_eq!(status["readiness"], json!("http_responsive"));
    assert_eq!(status["layers"]["tcp_reachable"], json!("passed"));
    assert_eq!(status["layers"]["http_health"], json!("passed"));
    assert_eq!(status["opencode_hint"], json!(true));
}

#[test]
fn harness_probe_requires_password_for_opencode_api_attach_ready() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _password = EnvRestore::remove("OPENCODE_SERVER_PASSWORD");
    let (server_url, server) = spawn_opencode_probe_server();

    let status = probe_harness_server_status(Some(&server_url));
    server.join().expect("probe server thread");

    assert_eq!(status["reachable"], json!(true));
    assert_eq!(status["attach_ready"], json!(false));
    assert_eq!(status["readiness"], json!("server_auth_required"));
    assert_eq!(status["layers"]["opencode_api_version"], json!("passed"));
    assert_eq!(
        status["layers"]["session_create_smoke"],
        json!("route_available")
    );
    assert_eq!(
        status["layers"]["credential_ready"],
        json!("unsafe_to_probe")
    );
    assert!(status["sensitive_endpoints_skipped"]
        .as_array()
        .is_some_and(|items| items.contains(&json!("/api/model"))));
}

#[test]
fn harness_probe_reports_attach_ready_for_authenticated_opencode_schema() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _password = EnvRestore::set("OPENCODE_SERVER_PASSWORD", "test-password");
    let (server_url, server) = spawn_opencode_probe_server();

    let status = probe_harness_server_status(Some(&server_url));
    server.join().expect("probe server thread");

    assert_eq!(status["reachable"], json!(true));
    assert_eq!(status["attach_ready"], json!(true));
    assert_eq!(status["readiness"], json!("attach_ready"));
    assert_eq!(status["evidence_strength"], json!("strong"));
    assert_eq!(
        status["api_capabilities"]["routes"]["session_create"],
        json!(true)
    );
    assert_eq!(
        status["layers"]["model_available"],
        json!("unsafe_to_probe")
    );
}

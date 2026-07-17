//! #1182 checkpoint 2 (cross-vendor review, 2026-07-17): issue #1173's
//! promised escape hatch for the full mbit_card is "verbose=true OR
//! action='profile'" — two independent opt-ins. `action='profiles'` (the
//! listing item 2 slims) must default to the slim row shape; the
//! singular-sounding `action='profile'`/`action='card'` aliases (pre-existing
//! before #1173, routed through the same `dispatch_profiles_json_for_server`
//! call) must default to the full card unless the caller explicitly asks for
//! the slim shape.
//!
//! These go through the real `tachi_task` MCP tool entry point
//! (`MemoryServer::tachi_task`), not the internal helper directly, so they
//! exercise the actual action-routing split in `task_router.rs`.

use super::super::make_server;
use super::task_params;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::Value;

#[tokio::test]
async fn action_profiles_listing_defaults_to_slim_rows() {
    let server = make_server();

    let raw = server
        .tachi_task(Parameters(task_params("profiles")))
        .await
        .expect("action='profiles' should succeed");
    let response: Value = serde_json::from_str(&raw).expect("profiles JSON");

    assert_eq!(response["verbose"], serde_json::json!(false), "{response}");
    let rows = response["dispatch_profiles"]
        .as_array()
        .expect("dispatch_profiles array");
    assert!(!rows.is_empty(), "{response}");
    for row in rows {
        assert!(
            row.get("mbit_card").is_none(),
            "action='profiles' default must not carry mbit_card: {row}"
        );
    }
}

#[tokio::test]
async fn action_profile_singular_defaults_to_full_card() {
    let server = make_server();

    let raw = server
        .tachi_task(Parameters(task_params("profile")))
        .await
        .expect("action='profile' should succeed");
    let response: Value = serde_json::from_str(&raw).expect("profile JSON");

    assert_eq!(response["verbose"], serde_json::json!(true), "{response}");
    let rows = response["dispatch_profiles"]
        .as_array()
        .expect("dispatch_profiles array");
    assert!(!rows.is_empty(), "{response}");
    for row in rows {
        assert!(
            row.get("mbit_card").is_some_and(Value::is_object),
            "action='profile' is the issue #1173 full-card escape hatch and \
             must default to the full mbit_card, got: {row}"
        );
    }
}

#[tokio::test]
async fn action_card_defaults_to_full_card() {
    let server = make_server();

    let raw = server
        .tachi_task(Parameters(task_params("card")))
        .await
        .expect("action='card' should succeed");
    let response: Value = serde_json::from_str(&raw).expect("card JSON");

    assert_eq!(response["verbose"], serde_json::json!(true), "{response}");
    let rows = response["dispatch_profiles"]
        .as_array()
        .expect("dispatch_profiles array");
    assert!(!rows.is_empty(), "{response}");
    for row in rows {
        assert!(
            row.get("mbit_card").is_some_and(Value::is_object),
            "action='card' must default to the full mbit_card, got: {row}"
        );
    }
}

#[tokio::test]
async fn action_profile_singular_honors_explicit_verbose_false() {
    let server = make_server();

    let mut params = task_params("profile");
    params.verbose = Some(false);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("action='profile' with verbose=false should succeed");
    let response: Value = serde_json::from_str(&raw).expect("profile JSON");

    assert_eq!(response["verbose"], serde_json::json!(false), "{response}");
    let rows = response["dispatch_profiles"]
        .as_array()
        .expect("dispatch_profiles array");
    for row in rows {
        assert!(
            row.get("mbit_card").is_none(),
            "explicit verbose=false must still slim action='profile': {row}"
        );
    }
}

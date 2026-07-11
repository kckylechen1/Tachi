//! #757 fold discrimination: standalone memory-admin + pipeline tools folded
//! into `tachi_memory` actions must (a) delegate to the SAME handler with
//! handler-identical results, and (b) leave the old standalone names off the
//! tool router.
//!
//! These tests are the red→green discrimination for the fold: they would fail
//! on the pre-fold code (no `delete`/`gc`/`doctor_scan`/`ingest`/`ingest_source`
//! actions existed → "Invalid action" error) and pass after.

use super::*;

/// Build a `TachiMemoryParams` seeded for a fold action with all the ingest
/// defaults filled in by the helper, so each test only overrides what it needs.
fn fold_params(action: &str) -> TachiMemoryParams {
    let mut p = tachi_memory_params(action);
    p.format = Some("json".to_string());
    p
}

#[tokio::test]
async fn delete_action_matches_delete_memory_handler() {
    let server = make_server();
    let id = "fold757-delete-equiv";

    let mut seed = tachi_memory_params("save");
    seed.format = Some("json".to_string());
    seed.id = Some(id.to_string());
    seed.force = true;
    seed.scope = Some("global".to_string());
    seed.text = Some("fold delete equivalence seed".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, seed.clone())
        .await
        .expect("seed before facade delete");

    let mut facade_params = fold_params("delete");
    facade_params.id = Some(id.to_string());
    let via_facade = crate::facade_memory_ops::handle_tachi_memory(&server, facade_params)
        .await
        .expect("facade delete should succeed");
    let facade_json: Value = serde_json::from_str(&via_facade).expect("facade delete json");
    assert_eq!(facade_json["deleted"], json!(true), "facade delete removed row");

    // Re-seed the same id so the direct handler sees the identical pre-state.
    crate::facade_memory_ops::handle_tachi_memory(&server, seed)
        .await
        .expect("re-seed before direct delete");
    let via_direct = crate::memory_ops::handle_delete_memory(
        &server,
        crate::tool_params::DeleteMemoryParams {
            id: id.to_string(),
            project: None,
        },
    )
    .await
    .expect("direct delete should succeed");

    assert_eq!(via_facade, via_direct, "facade action='delete' must match delete_memory handler byte-for-byte");
}

#[tokio::test]
async fn gc_action_matches_memory_gc_handler() {
    let server = make_server();
    let via_facade = crate::facade_memory_ops::handle_tachi_memory(&server, fold_params("gc"))
        .await
        .expect("facade gc should succeed");
    let via_direct = crate::memory_ops::handle_memory_gc(&server)
        .await
        .expect("direct gc should succeed");
    assert_eq!(
        via_facade, via_direct,
        "facade action='gc' must match memory_gc handler byte-for-byte"
    );
}

#[tokio::test]
async fn doctor_scan_action_matches_tachi_doctor_scan_handler() {
    let server = make_server();
    let via_facade =
        crate::facade_memory_ops::handle_tachi_memory(&server, fold_params("doctor_scan"))
            .await
            .expect("facade doctor_scan should succeed");
    let via_direct = crate::doctor_ops::handle_tachi_doctor_scan()
        .await
        .expect("direct doctor_scan should succeed");

    // `generated_at` (doctor/scan.rs: `scan()` stamps `Utc::now()`) is set
    // fresh on each independent call, so it legitimately differs between the
    // two invocations here even though the handler is a literal 1:1
    // delegation (facade_memory_ops/mod.rs "doctor_scan" arm). Strip it from
    // both sides before comparing so the assertion checks content
    // equivalence, not wall-clock equality.
    let mut facade_json: Value =
        serde_json::from_str(&via_facade).expect("facade doctor_scan output is valid JSON");
    let mut direct_json: Value =
        serde_json::from_str(&via_direct).expect("direct doctor_scan output is valid JSON");
    for value in [&mut facade_json, &mut direct_json] {
        if let Some(obj) = value.as_object_mut() {
            obj.remove("generated_at");
        }
    }

    assert_eq!(
        facade_json, direct_json,
        "facade action='doctor_scan' must match tachi_doctor_scan handler (ignoring generated_at)"
    );
}

#[tokio::test]
async fn ingest_action_matches_ingest_handler() {
    let server = make_server();
    // Empty content exercises the deterministic source-mode skip path (no
    // network/LLM), which is the stable handler contract we can byte-compare.
    let mut facade_params = fold_params("ingest");
    facade_params.content = None;
    let via_facade = crate::facade_memory_ops::handle_tachi_memory(&server, facade_params)
        .await
        .expect("facade ingest should succeed");

    let via_direct = crate::pipeline_ops::handle_ingest(
        &server,
        crate::tool_params::IngestParams {
            ingest_type: "source".to_string(),
            content: None,
            source_url: None,
            source: None,
            path_prefix: None,
            auto_chunk: true,
            auto_summarize: true,
            auto_link: true,
            importance: 0.7,
            scope: "project".to_string(),
            project: None,
            domain: None,
            chunk_size_chars: 1200,
            chunk_overlap_chars: 120,
            conversation_id: None,
            turn_id: None,
            event_type: None,
            messages: Vec::new(),
            metadata: None,
        },
    )
    .await
    .expect("direct ingest should succeed");

    assert_eq!(
        via_facade, via_direct,
        "facade action='ingest' must match ingest handler byte-for-byte"
    );
}

#[tokio::test]
async fn ingest_source_action_matches_ingest_source_handler() {
    let server = make_server();
    // An explicit empty *string* is still type-valid (satisfies the required
    // `content: String` contract) and exercises the same deterministic
    // whitespace/empty-content skip path the direct handler uses (see
    // `ingest_source_empty_content_records_skip_audit`), so both sides stay
    // byte-comparable without touching the network/LLM.
    let mut facade_params = fold_params("ingest_source");
    facade_params.content = Some(json!(""));
    let via_facade = crate::facade_memory_ops::handle_tachi_memory(&server, facade_params)
        .await
        .expect("facade ingest_source should succeed");

    let via_direct = crate::pipeline_ops::handle_ingest_source(
        &server,
        crate::tool_params::IngestSourceParams {
            content: String::new(),
            source_url: None,
            source: None,
            path_prefix: None,
            auto_chunk: true,
            auto_summarize: true,
            auto_link: true,
            importance: 0.7,
            scope: "project".to_string(),
            project: None,
            domain: None,
            chunk_size_chars: 1200,
            chunk_overlap_chars: 120,
            metadata: None,
        },
    )
    .await
    .expect("direct ingest_source should succeed");

    assert_eq!(
        via_facade, via_direct,
        "facade action='ingest_source' must match ingest_source handler byte-for-byte"
    );
}

/// #757-fold fix (gpt-5.6-terra review): the standalone `ingest_source` tool
/// required `content: String` — omission was a hard deserialization failure
/// before the handler ever ran. The fold's facade previously masked this by
/// silently defaulting a missing `content` to `String::new()` (this test used
/// to assert THAT byte-matched a hand-built empty string; it now asserts the
/// omission is rejected instead, restoring the original required-field
/// boundary).
#[tokio::test]
async fn ingest_source_action_rejects_omitted_content() {
    let server = make_server();
    let mut facade_params = fold_params("ingest_source");
    facade_params.content = None;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, facade_params)
        .await
        .expect_err("ingest_source must reject omitted content");
    assert!(
        err.to_ascii_lowercase().contains("content"),
        "expected a content-related rejection, got: {err}"
    );
}

/// Companion to the omission case: the standalone tool's `content: String`
/// field also rejected non-string JSON (deserialization type mismatch). The
/// fold previously coerced any JSON value to text via
/// `value_to_template_text` instead of rejecting it — restore the rejection.
#[tokio::test]
async fn ingest_source_action_rejects_non_string_content() {
    let server = make_server();
    let mut facade_params = fold_params("ingest_source");
    facade_params.content = Some(json!(42));
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, facade_params)
        .await
        .expect_err("ingest_source must reject non-string content");
    assert!(
        err.to_ascii_lowercase().contains("content"),
        "expected a content-related rejection, got: {err}"
    );
}

#[tokio::test]
async fn invalid_action_still_rejected_after_fold() {
    let server = make_server();
    let mut p = tachi_memory_params("bogus_action");
    p.format = Some("json".to_string());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, p)
        .await
        .expect_err("unknown action must be rejected");
    assert!(err.contains("Invalid action"), "unexpected error: {err}");
}

#[tokio::test]
async fn folded_standalone_names_no_longer_registered_on_router() {
    let server = make_server();
    let names: std::collections::BTreeSet<String> = server
        .tool_router
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();

    // (b) old standalone names must be gone from the router.
    for retired in [
        "delete_memory",
        "memory_gc",
        "tachi_doctor_scan",
        "ingest",
        "ingest_source",
    ] {
        assert!(
            !names.contains(retired),
            "folded standalone tool '{retired}' must no longer be registered on the router"
        );
    }

    // The unified facade that now fronts them must remain registered.
    assert!(
        names.contains("tachi_memory"),
        "tachi_memory facade must remain registered after the fold"
    );
}

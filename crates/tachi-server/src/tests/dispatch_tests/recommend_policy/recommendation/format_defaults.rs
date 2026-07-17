//! tachi#1201: end-to-end (through the real `tachi_task` MCP entry point,
//! not the `format_facade_response` unit tests in `tools/tests.rs`) coverage
//! for action='recommend' response-shape defaults:
//! 1. format omitted -> markdown (table header present, not JSON).
//! 2. format='json' -> slim candidate rows (no per-row mbit_card / dropped
//!    telemetry fields).
//! 3. include_card=true -> exactly one top-level mbit_card.

use super::*;

#[tokio::test]
async fn tachi_task_recommend_defaults_to_markdown_when_format_omitted() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a small documentation update".to_string());
    params.limit = Some(10);
    params.format = None;

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed with format omitted");

    assert!(raw.starts_with("## Tachi task recommend"), "{raw}");
    assert!(
        raw.contains("| profile | role | score | useful_rate | top reason |"),
        "{raw}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&raw).is_err(),
        "action='recommend' with format omitted must default to markdown, not JSON: {raw}"
    );
}

#[tokio::test]
async fn tachi_task_recommend_json_format_slims_candidates_by_default() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a small documentation update".to_string());
    params.limit = Some(10);
    params.format = Some("json".to_string());

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed with format=json");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert!(rec.get("mbit_card").is_none(), "{rec}");
    assert!(rec.get("identity_receipt").is_none(), "{rec}");
    let candidates = rec["candidates"].as_array().expect("candidates");
    assert!(!candidates.is_empty(), "{rec}");
    for row in candidates {
        assert!(
            row.as_object()
                .expect("candidate row object")
                .keys()
                .all(|key| matches!(key.as_str(), "profile" | "role" | "score" | "reasons")),
            "slim candidate row must only carry profile/role/score/reasons, got: {row}"
        );
        assert!(row.get("mbit_card").is_none(), "{row}");
        assert!(row.get("live_samples").is_none(), "{row}");
        assert!(row.get("human_override_rate").is_none(), "{row}");
    }
}

#[tokio::test]
async fn tachi_task_recommend_include_card_surfaces_exactly_one_card() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a small documentation update".to_string());
    params.limit = Some(10);
    params.format = Some("json".to_string());
    params.include_card = Some(true);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed with include_card=true");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert!(rec["mbit_card"].is_object(), "{rec}");
    // Exactly one top-level card — not re-embedded per candidate row.
    let candidates = rec["candidates"].as_array().expect("candidates");
    for row in candidates {
        assert!(
            row.get("mbit_card").is_none(),
            "mbit_card must not be re-embedded per candidate row: {row}"
        );
    }
}

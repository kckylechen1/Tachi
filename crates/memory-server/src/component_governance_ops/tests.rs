//! Tests for the component governance read model (Issue #796).

use super::*;
use crate::tests::make_server;
use crate::tool_params::TachiComponentParams;
use serde_json::Value;

fn list_params() -> TachiComponentParams {
    TachiComponentParams {
        action: "list".to_string(),
        format: Some("json".to_string()),
        component_id: None,
        component_type: None,
        include_archived: None,
        limit: None,
        project: None,
        repo: None,
    }
}

fn show_params(component_id: &str) -> TachiComponentParams {
    TachiComponentParams {
        action: "show".to_string(),
        format: Some("json".to_string()),
        component_id: Some(component_id.to_string()),
        component_type: None,
        include_archived: None,
        limit: None,
        project: None,
        repo: None,
    }
}

#[tokio::test]
async fn seed_component_records_persists_five_records_under_components_v0() {
    let server = make_server();

    let seeded = seed_component_records(&server).expect("seed should succeed");
    assert!(seeded, "first seed call should report true (seeded)");

    // Five records must be queryable under /components/v0/.
    let count = server
        .with_global_store_read(|store| {
            let entries = store
                .list_by_path(COMPONENT_PATH_PREFIX, 100, false)
                .map_err(|e| format!("list: {e}"))?;
            Ok::<_, String>(entries.len())
        })
        .expect("list should succeed");
    assert_eq!(count, 5, "expected 5 component records, got {count}");

    // Each must carry a component_record metadata object.
    let with_record = server
        .with_global_store_read(|store| {
            let entries = store
                .list_by_path(COMPONENT_PATH_PREFIX, 100, false)
                .map_err(|e| format!("list: {e}"))?;
            let n = entries
                .iter()
                .filter(|e| extract_component_record(&e.metadata).is_some())
                .count();
            Ok::<_, String>(n)
        })
        .expect("metadata check");
    assert_eq!(
        with_record, 5,
        "all 5 records must carry metadata.component_record"
    );
}

#[tokio::test]
async fn seed_component_records_is_idempotent_across_calls() {
    let server = make_server();

    let first = seed_component_records(&server).expect("first seed");
    assert!(first, "first call seeds");
    let second = seed_component_records(&server).expect("second seed");
    assert!(!second, "second call must be a no-op (marker already set)");

    let count = server
        .with_global_store_read(|store| {
            store
                .list_by_path(COMPONENT_PATH_PREFIX, 100, false)
                .map(|e| e.len())
                .map_err(|e| format!("list: {e}"))
        })
        .expect("list");
    assert_eq!(
        count, 5,
        "idempotent seed must not duplicate rows: got {count}"
    );
}

#[tokio::test]
async fn component_list_returns_compact_records() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(&server, list_params())
        .await
        .expect("list action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    let records = parsed["records"].as_array().expect("records array");
    assert_eq!(records.len(), 5, "list must return 5 compact records");
    // Each compact record carries the four required fields.
    for rec in records {
        assert!(rec.get("component_id").is_some(), "missing component_id");
        assert!(
            rec.get("component_type").is_some(),
            "missing component_type"
        );
        assert!(rec.get("owner_repo").is_some(), "missing owner_repo");
        assert!(rec.get("summary").is_some(), "missing summary");
    }
}

#[tokio::test]
async fn component_show_returns_full_record_and_edges() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(&server, show_params("tachi-memory-kernel"))
        .await
        .expect("show action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    let record = &parsed["record"];
    assert_eq!(
        record["component_id"],
        json!("tachi-memory-kernel"),
        "show must return the requested record"
    );
    assert_eq!(
        record["component_type"],
        json!("kernel"),
        "kernel record type"
    );
    // The kernel record declares three known downstream consumers → owns edges.
    let edges = parsed["edges"].as_array().expect("edges array");
    let owns_count = edges
        .iter()
        .filter(|e| e["relation"].as_str() == Some("owns"))
        .count();
    assert_eq!(
        owns_count, 3,
        "kernel must own its 3 known downstream consumers: got {owns_count}"
    );
}

#[tokio::test]
async fn component_show_unknown_returns_not_found() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(&server, show_params("nonexistent-component-xyz"))
        .await
        .expect("show action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        parsed["status"],
        json!("not_found"),
        "unknown component must report not_found"
    );
}

// ─── Issue #797: read-only downstream classifier tests ───────────────────────

fn check_params(repo: &str) -> TachiComponentParams {
    TachiComponentParams {
        action: "check".to_string(),
        format: Some("json".to_string()),
        component_id: None,
        component_type: None,
        include_archived: None,
        limit: None,
        project: None,
        repo: Some(repo.to_string()),
    }
}

#[tokio::test]
async fn component_check_classifies_tachi_checkout_as_kernel_drift() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap();
    let body = handle_tachi_component(&server, check_params(&repo_root.display().to_string()))
        .await
        .expect("check action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    let category = parsed["category"].as_str().expect("category");
    assert!(
        category == CATEGORY_KERNEL_DRIFT || category == CATEGORY_BRIDGE,
        "tachi checkout must classify as kernel_drift or bridge, got {category} (gaps: {:?})",
        parsed["evidence_gaps"]
    );
}

#[tokio::test]
async fn component_check_unknown_repo_returns_unknown_with_evidence_gaps() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    let temp = tempfile::tempdir().expect("temp dir");
    let body = handle_tachi_component(
        &server,
        check_params(&temp.path().display().to_string()),
    )
    .await
    .expect("check action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["category"], json!(CATEGORY_UNKNOWN));
    let gaps = parsed["evidence_gaps"].as_array().expect("evidence_gaps");
    assert!(!gaps.is_empty(), "unknown must carry evidence gaps");
}

#[tokio::test]
async fn component_check_nonexistent_path_returns_unknown() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    let body = handle_tachi_component(
        &server,
        check_params("/tmp/nonexistent-component-check-path-xyz-797"),
    )
    .await
    .expect("check action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["category"], json!(CATEGORY_UNKNOWN));
    let gaps = parsed["evidence_gaps"].as_array().expect("gaps");
    assert!(
        gaps.iter().any(|g| g.as_str().map(|s| s.contains("does not exist")).unwrap_or(false)),
        "nonexistent path must report the missing-path gap: {gaps:?}"
    );
}

#[test]
fn normalize_remote_handles_https_and_ssh_forms() {
    assert_eq!(
        normalize_remote_to_owner_repo("https://github.com/kckylechen1/tachi.git"),
        "kckylechen1/tachi"
    );
    assert_eq!(
        normalize_remote_to_owner_repo("git@github.com:kckylechen1/Quant_Analyzer_2026.git"),
        "kckylechen1/Quant_Analyzer_2026"
    );
    assert_eq!(
        normalize_remote_to_owner_repo("https://github.com/kckylechen1/RomanBath"),
        "kckylechen1/RomanBath"
    );
}

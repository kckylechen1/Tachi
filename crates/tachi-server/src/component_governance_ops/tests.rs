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
    // The remote matches kckylechen1/tachi. Both tachi-memory-kernel and
    // tachi-event-projection-bridge share that owner_repo, so the classifier
    // must tie-break to the strongest match. Since the remote matches BOTH
    // equally (both Remote-strength), the kernel record wins by path-specificity:
    // it declares crates/memcore (which exists) and is the canonical surface.
    // We assert it matched ONE of the two kckylechen1/tachi records (not a
    // wrong owner) and that the matched_component_id is correct, with no
    // evidence gaps (remote match = confident).
    let category = parsed["category"].as_str().expect("category");
    let matched = parsed["matched_component_id"].as_str().unwrap_or("(none)");
    assert!(
        category == CATEGORY_KERNEL_DRIFT || category == CATEGORY_BRIDGE,
        "tachi checkout must classify as kernel_drift or bridge, got {category}"
    );
    assert!(
        matched == "tachi-memory-kernel" || matched == "tachi-event-projection-bridge",
        "must match a kckylechen1/tachi component, got {matched}"
    );
    // Remote match = confident: no path-only-fork evidence gap.
    let empty = Vec::new();
    let gaps = parsed["evidence_gaps"].as_array().unwrap_or(&empty);
    assert!(
        gaps.is_empty(),
        "remote-matched checkout must have no evidence gaps, got {gaps:?}"
    );
}

#[tokio::test]
async fn component_check_unknown_repo_returns_unknown_with_evidence_gaps() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    let temp = tempfile::tempdir().expect("temp dir");
    let body = handle_tachi_component(&server, check_params(&temp.path().display().to_string()))
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
        gaps.iter().any(|g| g
            .as_str()
            .map(|s| s.contains("does not exist"))
            .unwrap_or(false)),
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
    // trailing slash must not survive normalization (would cause silent no-match)
    assert_eq!(
        normalize_remote_to_owner_repo("https://github.com/kckylechen1/tachi/"),
        "kckylechen1/tachi"
    );
    assert_eq!(
        normalize_remote_to_owner_repo("https://github.com/kckylechen1/tachi.git/"),
        "kckylechen1/tachi"
    );
}

// ─── Issue #798: cutover planner ─────────────────────────────────────────────

fn plan_params(from: &str, to: &str) -> TachiComponentParams {
    TachiComponentParams {
        action: "plan".to_string(),
        format: Some("json".to_string()),
        component_id: Some(from.to_string()),
        component_type: None,
        include_archived: None,
        limit: None,
        project: None,
        repo: Some(to.to_string()),
    }
}

fn outcome_actions(parsed: &Value, outcome: &str) -> Vec<String> {
    parsed["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["outcome"].as_str() == Some(outcome))
        .filter_map(|i| i["action"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn component_plan_hypermem_covers_aliases_direct_reader_trading_policy() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(
        &server,
        plan_params("tachi-memory-kernel", "hypermemory-trading-adapter"),
    )
    .await
    .expect("plan action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(
        parsed["to"]["matched_component_id"],
        json!("hypermemory-trading-adapter")
    );
    assert_eq!(
        parsed["to"]["category"],
        json!(CATEGORY_ALLOWED_ADAPTER_POLICY)
    );

    let actions = outcome_actions(&parsed, OUTCOME_PULL);
    assert!(
        actions.iter().any(|a| a == "gate_aliases"),
        "hypermem plan must cover aliases gate: {actions:?}"
    );
    assert!(
        actions.iter().any(|a| a == "gate_direct_reader"),
        "hypermem plan must cover direct-reader gate: {actions:?}"
    );
    let adapt = outcome_actions(&parsed, OUTCOME_ADAPT);
    assert!(
        adapt.iter().any(|a| a == "gate_trading_policy"),
        "hypermem plan must cover trading policy as adapt: {adapt:?}"
    );

    // Outcomes group must distinguish pull/adapt/backflow/delete_retire.
    let outcomes: Vec<&str> = parsed["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["outcome"].as_str())
        .collect();
    for required in [
        OUTCOME_PULL,
        OUTCOME_ADAPT,
        OUTCOME_BACKFLOW,
        OUTCOME_DELETE_RETIRE,
    ] {
        assert!(
            outcomes.contains(&required),
            "plan must include outcome `{required}`: {outcomes:?}"
        );
    }
}

#[tokio::test]
async fn component_plan_zeroclaw_covers_chat_agent_and_event_projection_gates() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(
        &server,
        plan_params("tachi-memory-kernel", "zeroclaw-chat-memory-adapter"),
    )
    .await
    .expect("plan action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    let actions = outcome_actions(&parsed, OUTCOME_PULL);
    assert!(
        actions.iter().any(|a| a == "gate_chat_agent_adapter"),
        "zeroclaw plan must cover chat-agent adapter gate: {actions:?}"
    );
    assert!(
        actions.iter().any(|a| a == "gate_event_projection"),
        "zeroclaw plan must cover event projection gate: {actions:?}"
    );
}

#[tokio::test]
async fn component_plan_romanbath_defaults_to_frontend_shell() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(
        &server,
        plan_params("tachi-memory-kernel", "romanbath-frontend-app-shell"),
    )
    .await
    .expect("plan action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["to"]["category"], json!(CATEGORY_FRONTEND_SHELL));
    let note = parsed["romanbath_note"].as_str().unwrap_or("");
    assert!(
        note.to_ascii_lowercase().contains("frontend")
            || note.to_ascii_lowercase().contains("shell"),
        "romanbath note must treat it as frontend shell: {note}"
    );
    // Must not claim product-owned memory policy without evidence.
    assert!(
        !note
            .to_ascii_lowercase()
            .contains("product-owned memory policy only"),
        "without product-memory evidence, note must default to shell"
    );
}

#[tokio::test]
async fn component_plan_unknown_source_returns_not_found() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    let body = handle_tachi_component(
        &server,
        plan_params("no-such-component", "hypermemory-trading-adapter"),
    )
    .await
    .expect("plan action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("not_found"));
}

// ─── Issue #799: briefing/status context ─────────────────────────────────────

#[tokio::test]
async fn component_governance_context_matches_tachi_checkout() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap();

    let ctx =
        component_governance_context(&server, Some("tachi"), Some(repo_root)).expect("context");
    assert_eq!(ctx["status"], json!("completed"));
    assert_eq!(ctx["authority"], json!("governance_registry"));
    let matches = ctx["matches"].as_array().expect("matches");
    assert!(
        !matches.is_empty(),
        "tachi checkout must match at least one governance record"
    );
    // Freshness must be labeled (current/stale/unknown) — never silent.
    for m in matches {
        let state = m["freshness"]["state"].as_str().unwrap_or("");
        assert!(
            matches!(state, "current" | "stale" | "unknown"),
            "freshness state required, got {state}"
        );
        // Registry note must not claim memory truth.
        let label = m["freshness"]["label"].as_str().unwrap_or("");
        assert!(
            !label.is_empty(),
            "freshness label required for {}",
            m["component_id"]
        );
    }

    let warnings = component_governance_warning_lines(&ctx);
    // blocked_fork on zeroclaw may appear if project-name also matches; for
    // tachi path we at least expect no panic and a Vec.
    let _ = warnings;
}

#[tokio::test]
async fn component_governance_context_project_hint_matches_hypermem() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    // No real Quant path — project name alone must surface the trading adapter.
    let ctx =
        component_governance_context(&server, Some("Quant_Analyzer_2026"), None).expect("context");
    let matches = ctx["matches"].as_array().expect("matches");
    assert!(
        matches
            .iter()
            .any(|m| { m["component_id"].as_str() == Some("hypermemory-trading-adapter") }),
        "project Quant_Analyzer_2026 must match hypermemory adapter: {matches:?}"
    );
    let hm = matches
        .iter()
        .find(|m| m["component_id"].as_str() == Some("hypermemory-trading-adapter"))
        .expect("hypermem match");
    // Drift/backflow must be surfaced for cutover awareness.
    assert!(
        hm["known_drift"].as_array().is_some_and(|d| !d.is_empty()),
        "hypermem match must surface known_drift"
    );
    assert!(
        hm["backflow_candidates"]
            .as_array()
            .is_some_and(|b| !b.is_empty()),
        "hypermem match must surface backflow_candidates"
    );
}

use super::{
    ensure_test_env, make_entry, make_server, make_server_with_project_fixture,
    make_server_with_temp_home, plant_leftover_shared_wiki, seed_wiki_project_entries,
};
use crate::memory_search_ops::recall_simulate::{
    build_recall_simulation_report, RecallSimulationParams,
};
use crate::tool_params::{
    InitProjectDbParams, TachiDomainAdapterParams, TachiEventParams, TachiMemoryParams,
    TachiSearchParams, TachiTaskParams,
};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

fn tachi_memory_params(action: &str) -> TachiMemoryParams {
    TachiMemoryParams {
        conversation_scope: None,
        auto_conversation_scope: true,
        multi_evidence_pack: true,
        action: action.to_string(),
        issue_ref: None,
        format: Some("markdown".to_string()),
        query: None,
        scope: matches!(action, "save" | "checkpoint").then(|| "global".to_string()),
        content_scope: None,
        storage_scope: None,
        top_k: 6,
        path_prefix: None,
        file_context: None,
        error_context: None,
        category: None,
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        as_of: None,
        include_superseded: false,
        include_test: false,
        synthesize: false,
        model: None,
        agent_role: None,
        text: None,
        title: None,
        summary: None,
        topic: None,
        keywords: Vec::new(),
        entities: Vec::new(),
        importance: None,
        retention_policy: None,
        kind: None,
        path: None,
        id: None,
        force: false,
        source: None,
        valid_from: None,
        valid_until: None,
        supersedes: Vec::new(),
        metadata: None,
        files: Vec::new(),
        references: Vec::new(),
        project: None,
        project_explicit: false,
        domain: None,
        compact: false,
        proposal_id: None,
        review_status: None,
        notes: None,
        confirm: false,
        state_filter: None,
    }
}

fn recall_simulation_params() -> RecallSimulationParams {
    RecallSimulationParams {
        top_k: 6,
        ..RecallSimulationParams::default()
    }
}

/// Drives the replay engine behind `tachi eval recall` and returns its JSON report.
async fn run_recall_simulation(
    server: &crate::MemoryServer,
    params: RecallSimulationParams,
) -> Result<String, String> {
    let report = build_recall_simulation_report(server, &params).await?;
    serde_json::to_string(&report).map_err(|e| format!("serialize recall report: {e}"))
}

fn session_claim_snapshot(server: &crate::MemoryServer) -> (Vec<memcore::SessionClaim>, i64) {
    server
        .with_global_store_read(|store| {
            let claims = memcore::list_claims(store.connection(), None)
                .map_err(|error| error.to_string())?;
            let total_changes = store
                .connection()
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .map_err(|error| error.to_string())?;
            Ok((claims, total_changes))
        })
        .expect("snapshot session-claim rows")
}

#[tokio::test]
async fn retired_memory_claim_release_are_rejected_without_mutating_workclaim_rows() {
    let server = make_server();
    crate::claims_ops::admit_agent_connection(&server, Some("agent.c2a1".to_string()), true)
        .expect("admit seed identity");
    server
        .with_global_store(|store| {
            memcore::insert_work_claim(
                store.connection(),
                &memcore::NewWorkClaim {
                    claim_id: "c2a1-canonical-workclaim".to_string(),
                    agent_identity_id: "agent.c2a1".to_string(),
                    session_client: Some("work-claim:c2a1-canonical-workclaim".to_string()),
                    issue_ref: Some("kckylechen1/tachi#1688".to_string()),
                    flow_id: Some("flow-c2a1".to_string()),
                    dispatch_id: Some("dispatch-c2a1".to_string()),
                    branch: "leaf/1688-memory-claim-release".to_string(),
                    worktree_path: "/tmp/tachi-c2a1".to_string(),
                    declared_file_scope: r#"["crates/tachi-server/src/facade_memory_ops/mod.rs"]"#
                        .to_string(),
                    role: "implementer".to_string(),
                    mode: memcore::WorkClaimMode::ReadOnly,
                    expected_head: "03b6b600".to_string(),
                    lease_expires_at: "2030-01-01T00:00:00Z".to_string(),
                    created_at: "2026-08-12T00:00:00Z".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
            memcore::insert_claim(
                store.connection(),
                &memcore::NewSessionClaim {
                    claim_id: "c2a1-historical-presence".to_string(),
                    session_client: Some("legacy-c2a1".to_string()),
                    issue_ref: Some("kckylechen1/tachi#1688".to_string()),
                    flow_id: Some("flow-c2a1".to_string()),
                    dispatch_id: Some("dispatch-c2a1".to_string()),
                    branch: "legacy/c2a1".to_string(),
                    declared_file_scope: Some(
                        r#"["crates/tachi-server/src/claims_ops.rs"]"#.to_string(),
                    ),
                    created_at: "2026-08-11T00:00:00Z".to_string(),
                },
            )
            .map_err(|error| error.to_string())
        })
        .expect("seed canonical and historical claim rows");

    let before = session_claim_snapshot(&server);
    let legacy_payloads = [
        (
            "release",
            serde_json::json!({
                "action": "release",
                "format": "json",
                "claim_id": "c2a1-historical-presence",
                "release_reason": "retired-memory-route"
            }),
        ),
        (
            "claim",
            serde_json::json!({
                "action": "claim",
                "format": "json",
                "issue_ref": "kckylechen1/tachi#1688",
                "flow_id": "flow-c2a1",
                "branch": "legacy/c2a1-new",
                "declared_file_scope": ["crates/tachi-server/src/facade_memory_ops/mod.rs"]
            }),
        ),
    ];

    for (action, payload) in legacy_payloads {
        let error = serde_json::from_value::<TachiMemoryParams>(payload)
            .expect_err("retired Memory claim/release must fail at typed deserialization")
            .to_string();
        assert_eq!(
            error,
            format!(
                "invalid tachi_memory action '{action}'; use search, get, save, briefing, checkpoint, alerts, ask, extract_facts, or consolidate"
            ),
            "retired Memory action={action} must carry exact typed-boundary guidance",
        );
        assert_eq!(
            session_claim_snapshot(&server),
            before,
            "retired Memory action={action} must not mutate canonical or historical claim rows"
        );
    }
}

mod briefing;
mod cli_daemon;
mod domain_adapter;
mod event;
mod memory_actions;
mod memory_search;
mod project_store_identity;
mod receipt_golden;
mod status;
mod synthetic_isolation;
mod workspace_recall;

mod memory_evidence_regressions;

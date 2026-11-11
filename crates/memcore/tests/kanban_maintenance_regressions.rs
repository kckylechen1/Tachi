//! Public-API regressions kept as a small integration target: compiling the
//! entire memcore lib-test harness is not necessary to exercise this boundary.
use memcore::{
    GcConfig, MaintenanceClassFact, MemoryEdge, MemoryEntry, MemoryError, MemoryStore,
    OperatorMaintenanceCommittedReceiptBinding, OperatorMaintenancePlanBinding,
};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

const AS_OF: &str = "2026-08-13T00:00:00Z";

fn terminal(id: &str) -> MemoryEntry {
    serde_json::from_value(json!({
        "id": id, "path": format!("/kanban/cards/{id}"),
        "text": format!("fixture body {id}"), "summary": format!("fixture {id}"),
        "timestamp": "2020-01-01T00:00:00Z", "category": "kanban",
        "retention_policy": "ephemeral",
        "metadata": {"status": "resolved", "terminal_at": "2026-01-01T00:00:00Z"}
    }))
    .unwrap()
}

fn selected(store: &MemoryStore) -> usize {
    store
        .plan_operator_gc(&GcConfig::default(), AS_OF, 30, true)
        .unwrap()
        .into_iter()
        .find(|fact| fact.class == "kanban_terminal_or_stale")
        .unwrap()
        .count
}

#[test]
fn gc_uses_terminal_age_and_protects_active_pinned_and_unknown_history() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    for id in [
        "eligible",
        "pinned",
        "recent-terminal",
        "unknown-terminal",
        "active",
    ] {
        let mut entry = terminal(id);
        match id {
            "pinned" => entry.retention_policy = Some("pinned".to_string()),
            "recent-terminal" => entry.metadata["terminal_at"] = json!("2026-08-12T00:00:00Z"),
            "unknown-terminal" => {
                entry
                    .metadata
                    .as_object_mut()
                    .unwrap()
                    .remove("terminal_at");
            }
            "active" => {
                entry.category = "fact".to_string();
                entry.path = "/kanban/tasks/active".to_string();
                entry.metadata = json!({"a2a_state": "TASK_STATE_WORKING"});
            }
            _ => {}
        }
        store.upsert(&entry).unwrap();
    }
    assert_eq!(
        selected(&store),
        1,
        "only the unpinned old terminal is eligible"
    );
}

#[test]
fn gc_keeps_supersession_targets_and_graph_references() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    let mut store = MemoryStore::open(&path.to_string_lossy()).unwrap();
    for id in ["supersession-target", "edge-target", "eligible"] {
        store.upsert(&terminal(id)).unwrap();
    }
    let mut retained = terminal("retained");
    retained.path = "/notes/history".to_string();
    retained.category = "fact".to_string();
    retained.metadata = json!({});
    store.upsert(&retained).unwrap();
    // An unrestricted fixture connection seeds the historical supersession
    // column only; insertion and graph writes use normal public store APIs.
    let fixture = rusqlite::Connection::open(&path).unwrap();
    fixture
        .execute(
            "UPDATE memories SET superseded_by='supersession-target' WHERE id='retained'",
            [],
        )
        .unwrap();
    store
        .add_edge(
            &serde_json::from_value::<MemoryEdge>(json!({
                "source_id": "retained", "target_id": "edge-target", "relation": "references"
            }))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(selected(&store), 1);
    assert_eq!(
        fixture
            .query_row::<String, _, _>(
                "SELECT superseded_by FROM memories WHERE id='retained'",
                [],
                |row| row.get(0)
            )
            .unwrap(),
        "supersession-target"
    );
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Policy {
    Gc {
        access_history_keep_per_memory: usize,
        processed_events_max_days: u32,
        audit_log_max_days: u32,
        audit_log_max_rows: usize,
        agent_known_state_max_days: u32,
        recall_impression_max_groups: usize,
        recall_impression_max_days: u32,
        kanban_max_age_days: u64,
    },
}

#[derive(Serialize)]
struct Plan {
    version: u32,
    operation: &'static str,
    target_path: String,
    target_physical_identity: String,
    profile: String,
    as_of: &'static str,
    policy_version: &'static str,
    policy: Policy,
    source: Vec<MaintenanceClassFact>,
    digest: String,
}

fn frozen_plan(store: &MemoryStore, path: &std::path::Path) -> OperatorMaintenancePlanBinding {
    let cfg = GcConfig::default();
    let mut plan = Plan {
        version: 1,
        operation: "gc",
        target_path: path.to_string_lossy().into_owned(),
        target_physical_identity: store.opened_physical_db_identity().unwrap().to_string(),
        profile: store.store_profile().as_str().to_string(),
        as_of: AS_OF,
        policy_version: "tachi-memory-maintenance-v2",
        policy: Policy::Gc {
            access_history_keep_per_memory: cfg.access_history_keep_per_memory,
            processed_events_max_days: cfg.processed_events_max_days,
            audit_log_max_days: cfg.audit_log_max_days,
            audit_log_max_rows: cfg.audit_log_max_rows,
            agent_known_state_max_days: cfg.agent_known_state_max_days,
            recall_impression_max_groups: cfg.recall_impression_max_groups,
            recall_impression_max_days: cfg.recall_impression_max_days,
            kanban_max_age_days: 30,
        },
        source: store.plan_operator_gc(&cfg, AS_OF, 30, true).unwrap(),
        digest: String::new(),
    };
    // The same regression runs against the old policy and the repaired one.
    // Selection/apply semantics, not unsupported-version rejection, must fail.
    for policy in ["tachi-memory-maintenance-v2", "tachi-memory-maintenance-v1"] {
        plan.policy_version = policy;
        plan.digest.clear();
        plan.digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&plan).unwrap()));
        if let Ok(binding) =
            OperatorMaintenancePlanBinding::from_canonical_json(&serde_json::to_vec(&plan).unwrap())
        {
            return binding;
        }
    }
    panic!("fixture must produce a canonical plan under the tested policy");
}

#[test]
fn gc_apply_refuses_a_new_complete_nested_metadata_reference() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    let mut store = MemoryStore::open(&path.to_string_lossy()).unwrap();
    store.upsert(&terminal("target")).unwrap();
    let mut source = terminal("source");
    source.path = "/notes/source".to_string();
    source.category = "fact".to_string();
    source.metadata = json!({"padding": "x".repeat(1200), "z_evidence": []});
    store.upsert(&source).unwrap();
    assert_eq!(
        selected(&store),
        1,
        "the control must initially be eligible"
    );
    let plan = frozen_plan(&store, &path);
    // Keep the row set and unrelated maintenance facts unchanged. Only the
    // complete nested reference payload changes after the plan is frozen.
    source.metadata = json!({"padding": "x".repeat(1200), "z_evidence": [{"id": "memory:target"}]});
    store.upsert(&source).unwrap();
    let error = store
        .apply_operator_gc_with_precommit_receipt(
            &plan,
            |_, _| -> Result<OperatorMaintenanceCommittedReceiptBinding, MemoryError> {
                panic!("reference drift must abort before mutation/receipt publication")
            },
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("source facts changed"),
        "{error}"
    );
    assert!(store.get("target").unwrap().is_some());
}

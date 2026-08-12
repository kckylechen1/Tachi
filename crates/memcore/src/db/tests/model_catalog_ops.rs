//! Model-broker catalog store discriminators (tachi#1681 D7 PR-B, item 1).
//!
//! The two the design names for this slice:
//!
//! - **Discrimination 5** — a probe or auth failure never erases catalog
//!   metadata. Asserted the only way that means anything: write the failure
//!   evidence, then prove the deployment row is byte-identical and its event
//!   log only grew.
//! - **Discrimination 10** — pricing snapshots are immutable by construction.
//!   The type-level half is in `catalog::tests`; here it is the store half —
//!   re-importing an unchanged sheet dedupes instead of rewriting.
//!
//! Plus the property the env import (item 2) rests on: a re-import of
//! unchanged content is a genuine no-op, not a revision bump and an event.

use super::*;

use crate::catalog::{
    CatalogSource, DeploymentCapabilities, DeploymentEventKind, EmbeddingsCapability,
    ModelDeployment, NewModelDeployment, NewModelDeploymentEvent, PricingSnapshot, ProtocolKind,
    DEPLOYMENT_STATUS_ACTIVE, DEPLOYMENT_STATUS_RETIRED,
};
use crate::db::model_catalog::{
    append_model_deployment_event, get_model_deployment, get_model_deployment_health,
    get_pricing_snapshot, list_all_model_deployment_events, list_model_deployment_events,
    list_model_deployments, list_model_deployments_by_source, retire_model_deployment,
    upsert_model_deployment, upsert_pricing_snapshot, DeploymentWrite, PricingSnapshotWrite,
};

fn catalog_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    init_schema(&conn).expect("schema initializes");
    conn
}

fn extract_lane() -> NewModelDeployment {
    NewModelDeployment::observed(
        "env:extract",
        "env:api.siliconflow.cn",
        ProtocolKind::OpenAiChatCompletions,
        "Qwen/Qwen3.5-27B",
        CatalogSource::Env,
        "2026-08-11T00:00:00.000Z",
    )
    .with_endpoint_ref("https://api.siliconflow.cn/v1/chat/completions")
    .with_capabilities(DeploymentCapabilities {
        chat: true,
        ..DeploymentCapabilities::default()
    })
    .with_source_refs(vec![
        "env_api_key:EXTRACT_API_KEY".to_string(),
        "env_api_key:SILICONFLOW_API_KEY".to_string(),
    ])
}

/// Stands in for PR-C's `record_deployment_outcome`, which does not exist
/// yet. Written as raw SQL on purpose: the point of discrimination 5 is that
/// **no** failure path — present or future — can reach catalog metadata, so
/// the test simulates the harshest version (a direct writer) rather than
/// waiting for the sanctioned one.
fn record_failure_health(conn: &Connection, deployment_id: &str, state: &str, error: &str) {
    conn.execute(
        "INSERT INTO model_deployment_health
            (deployment_id, state, last_attempt_at, last_error, error_count, evidence_kind,
             observed_at, metadata, updated_at)
         VALUES (?1, ?2, ?3, ?4, 1, 'probed', ?3, '{}', ?3)
         ON CONFLICT(deployment_id) DO UPDATE SET
            state = excluded.state,
            last_attempt_at = excluded.last_attempt_at,
            last_error = excluded.last_error,
            error_count = model_deployment_health.error_count + 1,
            observed_at = excluded.observed_at,
            updated_at = excluded.updated_at",
        rusqlite::params![deployment_id, state, "2026-08-11T01:00:00.000Z", error],
    )
    .expect("health row writes");
}

// ─── import / re-import ──────────────────────────────────────────────────────

#[test]
fn a_first_import_creates_the_row_at_revision_one_with_its_event() {
    let conn = catalog_conn();

    let write = upsert_model_deployment(&conn, &extract_lane()).expect("import succeeds");
    let DeploymentWrite::Created { revision, event_id } = write else {
        panic!("first import must be Created, got {write:?}");
    };
    assert_eq!(revision, 1);
    assert!(event_id > 0);

    let stored = get_model_deployment(&conn, "env:extract")
        .expect("read succeeds")
        .expect("row exists");
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.catalog_source, CatalogSource::Env);
    assert_eq!(stored.status, DEPLOYMENT_STATUS_ACTIVE);
    assert_eq!(stored.provider_model_id, "Qwen/Qwen3.5-27B");
    assert!(stored.capabilities.chat);
    assert_eq!(
        stored.source_refs,
        vec![
            "env_api_key:EXTRACT_API_KEY".to_string(),
            "env_api_key:SILICONFLOW_API_KEY".to_string()
        ]
    );

    let events = list_model_deployment_events(&conn, "env:extract").expect("events read");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].event_kind,
        DeploymentEventKind::DeploymentImported.as_str()
    );
    assert_eq!(events[0].revision, 1);
}

#[test]
fn re_importing_unchanged_content_is_a_no_op_that_only_moves_fetched_at() {
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("first import");

    let mut later = extract_lane();
    later.fetched_at = "2026-08-12T06:00:00.000Z".to_string();
    let write = upsert_model_deployment(&conn, &later).expect("second import");

    assert_eq!(
        write,
        DeploymentWrite::Unchanged { revision: 1 },
        "a process restart that re-resolves the same env chains must not bump the revision — \
         otherwise the counter every later slice binds preconditions to is pure noise"
    );
    assert!(!write.moved());

    let stored = get_model_deployment(&conn, "env:extract")
        .expect("read")
        .expect("row");
    assert_eq!(stored.revision, 1);
    assert_eq!(
        stored.fetched_at, "2026-08-12T06:00:00.000Z",
        "'when did we last confirm this' still moves; it is just not a catalog change"
    );

    let events = list_model_deployment_events(&conn, "env:extract").expect("events");
    assert_eq!(
        events.len(),
        1,
        "an unchanged re-import must not append an event"
    );
}

#[test]
fn a_changed_env_chain_advances_the_revision_and_records_both_digests() {
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("first import");
    let before_digest = extract_lane().content_digest();

    let mut moved = extract_lane();
    moved.provider_model_id = "Qwen/Qwen3.5-72B".to_string();
    let write = upsert_model_deployment(&conn, &moved).expect("second import");

    let DeploymentWrite::Advanced { revision, .. } = write else {
        panic!("a content change must be Advanced, got {write:?}");
    };
    assert_eq!(revision, 2);

    let stored = get_model_deployment(&conn, "env:extract")
        .expect("read")
        .expect("row");
    assert_eq!(stored.provider_model_id, "Qwen/Qwen3.5-72B");
    assert_eq!(stored.revision, 2);

    let events = list_model_deployment_events(&conn, "env:extract").expect("events");
    assert_eq!(events.len(), 2);
    let evidence: serde_json::Value =
        serde_json::from_str(&events[1].evidence).expect("evidence is JSON");
    assert_eq!(
        evidence["previous_content_digest"],
        serde_json::json!(before_digest),
        "the event has to say what it moved *from*, or the log cannot be replayed against the row"
    );
    assert_eq!(
        evidence["content_digest"],
        serde_json::json!(moved.content_digest())
    );
}

#[test]
fn retiring_a_deployment_is_a_status_transition_not_a_delete() {
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");

    let write = retire_model_deployment(&conn, "env:extract")
        .expect("retire succeeds")
        .expect("deployment exists");
    assert!(matches!(
        write,
        DeploymentWrite::Advanced { revision: 2, .. }
    ));

    let stored = get_model_deployment(&conn, "env:extract")
        .expect("read")
        .expect("the row is still there — retirement is not deletion");
    assert_eq!(stored.status, DEPLOYMENT_STATUS_RETIRED);
    assert_eq!(stored.provider_model_id, "Qwen/Qwen3.5-27B");

    // Retiring twice is idempotent, not a second event.
    let again = retire_model_deployment(&conn, "env:extract")
        .expect("retire again")
        .expect("still exists");
    assert_eq!(again, DeploymentWrite::Unchanged { revision: 2 });
    assert_eq!(
        list_model_deployment_events(&conn, "env:extract")
            .expect("events")
            .len(),
        2
    );

    assert_eq!(
        retire_model_deployment(&conn, "env:nonexistent").expect("no row is not an error"),
        None
    );
}

#[test]
fn deployments_are_listable_by_catalog_source() {
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("env import");
    let manual = NewModelDeployment::observed(
        "manual:claude-sonnet",
        "acct-anthropic",
        ProtocolKind::OpenAiChatCompletions,
        "claude-sonnet-4",
        CatalogSource::Manual,
        "2026-08-11T00:00:00.000Z",
    );
    upsert_model_deployment(&conn, &manual).expect("manual import");

    let env_rows =
        list_model_deployments_by_source(&conn, CatalogSource::Env).expect("env rows read");
    assert_eq!(
        env_rows
            .iter()
            .map(|row| row.deployment_id.as_str())
            .collect::<Vec<_>>(),
        vec!["env:extract"],
        "the #1685 cutover has to be able to ask 'what did env produce' exactly"
    );
    assert_eq!(list_model_deployments(&conn).expect("all rows").len(), 2);
}

// ─── discrimination 5: failures never erase catalog metadata ─────────────────

#[test]
fn probe_and_auth_failures_leave_the_deployment_row_byte_identical() {
    let conn = catalog_conn();
    let full = extract_lane().with_capabilities(DeploymentCapabilities {
        chat: true,
        tools: true,
        embeddings: Some(EmbeddingsCapability { dimension: 1024 }),
        ..DeploymentCapabilities::default()
    });
    upsert_model_deployment(&conn, &full).expect("import");
    let before: ModelDeployment = get_model_deployment(&conn, "env:extract")
        .expect("read")
        .expect("row");

    // Every failure class the design names, back to back.
    record_failure_health(&conn, "env:extract", "auth_failed", "401 unauthorized");
    record_failure_health(
        &conn,
        "env:extract",
        "rate_limited",
        "429 too many requests",
    );
    record_failure_health(&conn, "env:extract", "unreachable", "connect timeout");
    append_model_deployment_event(
        &conn,
        &NewModelDeploymentEvent::new("env:extract", 1, DeploymentEventKind::DeploymentUpdated)
            .with_evidence(r#"{"note":"probe cycle"}"#.to_string()),
    )
    .expect("event appends");

    let after = get_model_deployment(&conn, "env:extract")
        .expect("read")
        .expect("row");
    assert_eq!(
        before, after,
        "an auth/quota/transport failure says nothing about what the deployment *is*; the \
         catalog row must survive it untouched, field for field"
    );
    assert!(after.capabilities.chat);
    assert_eq!(
        after.capabilities.embeddings,
        Some(EmbeddingsCapability { dimension: 1024 })
    );
    assert_eq!(after.context_window, before.context_window);
    assert_eq!(after.revision, 1);

    // The failures did land — on the health table, which is the point of the
    // table boundary.
    let health = get_model_deployment_health(&conn, "env:extract")
        .expect("health read")
        .expect("health row exists");
    assert_eq!(health.state, "unreachable");
    assert_eq!(health.error_count, 3);
}

#[test]
fn the_event_log_only_ever_grows() {
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");
    let first = list_all_model_deployment_events(&conn).expect("events");

    let mut moved = extract_lane();
    moved.provider_model_id = "Qwen/Qwen3.5-72B".to_string();
    upsert_model_deployment(&conn, &moved).expect("update");
    retire_model_deployment(&conn, "env:extract").expect("retire");

    let all = list_all_model_deployment_events(&conn).expect("events");
    assert_eq!(all.len(), 3);
    assert_eq!(
        &all[..first.len()],
        &first[..],
        "earlier events must be untouched — an append-only log whose head can be edited is not \
         an audit trail"
    );
    let ids: Vec<i64> = all.iter().map(|event| event.id).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "ids are monotonic in append order");
}

// ─── discrimination 10: store half ───────────────────────────────────────────

#[test]
fn re_importing_an_unchanged_price_sheet_dedupes_without_rewriting_the_row() {
    let conn = catalog_conn();
    let prices = serde_json::json!({"input_per_mtok": "0.14", "output_per_mtok": "0.28"});

    let first = PricingSnapshot::mint(
        "deepseek",
        prices.clone(),
        Some(CatalogSource::ProviderApi),
        "2026-08-11T00:00:00.000Z",
    );
    assert_eq!(
        upsert_pricing_snapshot(&conn, &first).expect("first write"),
        PricingSnapshotWrite::Created
    );

    let hours_later = PricingSnapshot::mint(
        "deepseek",
        prices.clone(),
        Some(CatalogSource::ProviderApi),
        "2026-08-11T09:30:00.000Z",
    );
    assert_eq!(
        upsert_pricing_snapshot(&conn, &hours_later).expect("second write"),
        PricingSnapshotWrite::Deduped
    );

    let stored = get_pricing_snapshot(&conn, first.snapshot_id())
        .expect("read")
        .expect("row");
    assert_eq!(stored.snapshot_id(), first.snapshot_id());
    assert_eq!(
        stored.fetched_at, "2026-08-11T00:00:00.000Z",
        "the stored row must not be touched at all — a historical outcome row points at this id, \
         and rewriting even its timestamp mutates what that outcome resolves to"
    );
    assert_eq!(stored.pricing_data(), &prices);
}

#[test]
fn a_price_change_mints_a_second_snapshot_and_leaves_the_first_intact() {
    let conn = catalog_conn();
    let cheap = PricingSnapshot::mint(
        "deepseek",
        serde_json::json!({"input_per_mtok": "0.14"}),
        None,
        "2026-08-11T00:00:00.000Z",
    );
    let dearer = PricingSnapshot::mint(
        "deepseek",
        serde_json::json!({"input_per_mtok": "0.55"}),
        None,
        "2026-08-12T00:00:00.000Z",
    );
    upsert_pricing_snapshot(&conn, &cheap).expect("first");
    upsert_pricing_snapshot(&conn, &dearer).expect("second");

    assert_ne!(cheap.snapshot_id(), dearer.snapshot_id());
    let old = get_pricing_snapshot(&conn, cheap.snapshot_id())
        .expect("read")
        .expect("the old snapshot is still there");
    assert_eq!(
        old.pricing_data(),
        &serde_json::json!({"input_per_mtok": "0.14"}),
        "yesterday's invocations still cost yesterday's prices"
    );
}

#[test]
fn a_snapshot_rewritten_behind_the_stores_back_is_refused_on_read() {
    let conn = catalog_conn();
    let honest = PricingSnapshot::mint(
        "deepseek",
        serde_json::json!({"input_per_mtok": "0.14"}),
        None,
        "2026-08-11T00:00:00.000Z",
    );
    upsert_pricing_snapshot(&conn, &honest).expect("write");

    // No accessor in this module can do this; a third-party writer with the
    // db file can. The read path is what has to catch it.
    conn.execute(
        "UPDATE pricing_snapshots SET pricing_data = ?2 WHERE snapshot_id = ?1",
        rusqlite::params![honest.snapshot_id(), r#"{"input_per_mtok":"999.00"}"#],
    )
    .expect("raw rewrite");

    let err = get_pricing_snapshot(&conn, honest.snapshot_id())
        .expect_err("a rewritten content-addressed row must not be handed back as truth");
    assert!(err.to_string().contains("rewritten in place"), "got {err}");
}

#[test]
fn a_deployments_pricing_pointer_is_a_pointer_and_nothing_more() {
    let conn = catalog_conn();
    let snapshot = PricingSnapshot::mint(
        "siliconflow",
        serde_json::json!({"input_per_mtok": "0.07"}),
        None,
        "2026-08-11T00:00:00.000Z",
    );
    upsert_pricing_snapshot(&conn, &snapshot).expect("snapshot write");

    let mut deployment = extract_lane();
    deployment.pricing_snapshot_ref = Some(snapshot.snapshot_id().to_string());
    upsert_model_deployment(&conn, &deployment).expect("import");

    // Repointing the live catalog at a new sheet is an ordinary catalog
    // change; it advances the deployment and leaves both snapshots readable,
    // which is what makes "cost is frozen by copying the id onto the outcome
    // row" (#1681 D6) implementable at all.
    let newer = PricingSnapshot::mint(
        "siliconflow",
        serde_json::json!({"input_per_mtok": "0.09"}),
        None,
        "2026-08-12T00:00:00.000Z",
    );
    upsert_pricing_snapshot(&conn, &newer).expect("newer snapshot");
    deployment.pricing_snapshot_ref = Some(newer.snapshot_id().to_string());
    let write = upsert_model_deployment(&conn, &deployment).expect("repoint");
    assert!(matches!(
        write,
        DeploymentWrite::Advanced { revision: 2, .. }
    ));

    assert!(get_pricing_snapshot(&conn, snapshot.snapshot_id())
        .expect("read")
        .is_some());
    assert!(get_pricing_snapshot(&conn, newer.snapshot_id())
        .expect("read")
        .is_some());
}

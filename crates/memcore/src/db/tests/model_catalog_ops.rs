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
//! - **Discrimination 12** — the catalog domain's own event fold, replayed in
//!   full, equals the same fold advanced incrementally. Asserted against a log
//!   this module's own writers produced, not a synthetic one (the synthetic
//!   half is in `catalog::fold`'s unit tests).
//!
//! Plus the property the env import (item 2) rests on — a re-import of
//! unchanged content is a genuine no-op, not a revision bump and an event —
//! and the staleness rule (item 5): an expired row stays readable and stops
//! being authoritative.

use super::*;

use crate::catalog::health::{
    DeploymentOutcome, RetryAfter, ServerErrorStatus, UnusableResponseStatus,
};
use crate::catalog::{
    CatalogSource, DeploymentCapabilities, DeploymentEventKind, EmbeddingsCapability,
    ModelDeployment, NewModelDeployment, NewModelDeploymentEvent, PricingSnapshot, ProtocolKind,
    DEPLOYMENT_STATUS_ACTIVE, DEPLOYMENT_STATUS_RETIRED,
};
use crate::db::model_catalog::{
    append_model_deployment_event, get_model_deployment, get_model_deployment_health,
    get_pricing_snapshot, list_all_model_deployment_events, list_model_deployment_events,
    list_model_deployments, list_model_deployments_by_source, record_model_deployment_outcome,
    retire_model_deployment, upsert_model_deployment, upsert_pricing_snapshot,
    DeploymentHealthSkip, DeploymentHealthWrite, DeploymentOutcomeTarget, DeploymentWrite,
    PricingSnapshotWrite,
};
use crate::vault::health::EvidenceKind;

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

// ─── discrimination 12, against a real event log ─────────────────────────────

#[test]
fn full_replay_of_the_stored_log_equals_incremental_advance() {
    use crate::db::model_catalog::{advance_catalog_projection, replay_catalog_projection};

    let conn = catalog_conn();

    // A projection that has been following along since the beginning.
    let mut incremental = crate::catalog::fold::CatalogProjection::empty();

    upsert_model_deployment(&conn, &extract_lane()).expect("import extract");
    advance_catalog_projection(&conn, &mut incremental).expect("advance 1");

    let mut summary = extract_lane();
    summary.deployment_id = "env:summary".to_string();
    summary.provider_model_id = "Qwen/Qwen3.5-7B".to_string();
    upsert_model_deployment(&conn, &summary).expect("import summary");

    let mut moved = extract_lane();
    moved.provider_model_id = "Qwen/Qwen3.5-72B".to_string();
    upsert_model_deployment(&conn, &moved).expect("advance extract");
    advance_catalog_projection(&conn, &mut incremental).expect("advance 2");

    retire_model_deployment(&conn, "env:summary").expect("retire summary");
    advance_catalog_projection(&conn, &mut incremental).expect("advance 3");
    // A no-op advance: nothing was appended since the last watermark.
    advance_catalog_projection(&conn, &mut incremental).expect("advance 4");

    let full = replay_catalog_projection(&conn).expect("full replay");
    assert_eq!(
        full.digest(),
        incremental.digest(),
        "a daemon that has been folding events as they land and one that rebuilds from the log \
         at startup must reach the same catalog state"
    );
    assert_eq!(full, incremental);

    // And the fold says something a mutant could get wrong.
    assert_eq!(full.get("env:extract").map(|fold| fold.revision), Some(2));
    assert_eq!(full.get("env:summary").map(|fold| fold.retired), Some(true));
    assert_eq!(full.deployments().len(), 2);
}

#[test]
fn a_projection_rebuilt_mid_stream_catches_up_to_the_same_state() {
    use crate::db::model_catalog::{advance_catalog_projection, replay_catalog_projection};

    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");

    // Follower A has been running the whole time.
    let mut follower = crate::catalog::fold::CatalogProjection::empty();
    advance_catalog_projection(&conn, &mut follower).expect("advance");

    // Follower B restarts here and replays from scratch, then both keep going.
    let mut restarted = replay_catalog_projection(&conn).expect("replay");
    assert_eq!(follower.digest(), restarted.digest());

    let mut moved = extract_lane();
    moved.provider_model_id = "Qwen/Qwen3.5-72B".to_string();
    upsert_model_deployment(&conn, &moved).expect("advance extract");
    advance_catalog_projection(&conn, &mut follower).expect("advance A");
    advance_catalog_projection(&conn, &mut restarted).expect("advance B");

    assert_eq!(
        follower.digest(),
        restarted.digest(),
        "a restart in the middle of a stream must not fork the projection"
    );
}

#[test]
fn a_health_write_folds_into_the_catalog_projection_the_same_way_twice() {
    // Discrimination 12 extended to the health rows (#1681 D7 PR-C item 5),
    // against a log this module's own writers produced: full replay must equal
    // incremental advance *and* the fold's health bundle must agree with the
    // table it projects. A fold that could not see health events would pass
    // the first half while disagreeing about every cooldown.
    use crate::db::model_catalog::{advance_catalog_projection, replay_catalog_projection};

    let conn = catalog_conn();
    let mut incremental = crate::catalog::fold::CatalogProjection::empty();

    upsert_model_deployment(&conn, &extract_lane()).expect("import");
    advance_catalog_projection(&conn, &mut incremental).expect("advance 1");

    record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Throttled {
            retry_after: Some(RetryAfter::DeltaSeconds(45)),
        },
        EvidenceKind::SelfReported,
        instant(0),
    )
    .expect("throttle");

    let mut moved = extract_lane();
    moved.provider_model_id = "Qwen/Qwen3.5-72B".to_string();
    upsert_model_deployment(&conn, &moved).expect("advance the catalog row");
    advance_catalog_projection(&conn, &mut incremental).expect("advance 2");

    record_model_deployment_outcome(
        &conn,
        &DeploymentOutcomeTarget::deployment("env:extract"),
        server_error(502),
        EvidenceKind::Probed,
        instant(90),
    )
    .expect("server error");
    advance_catalog_projection(&conn, &mut incremental).expect("advance 3");

    let full = replay_catalog_projection(&conn).expect("full replay");
    assert_eq!(full.digest(), incremental.digest());
    assert_eq!(full, incremental);

    let folded = full.get("env:extract").expect("folded");
    assert_eq!(
        folded.revision, 2,
        "two health events between catalog events must not move the catalog revision"
    );
    let health_fold = folded.health.as_ref().expect("health folded");
    let health_row = get_model_deployment_health(&conn, "env:extract")
        .expect("read")
        .expect("row");
    assert_eq!(
        health_fold.state, health_row.state,
        "the state replayed from the log and the state in the table are the same fact"
    );
    assert_eq!(health_fold.cooldown_until, health_row.cooldown_until);
    assert_eq!(health_fold.event_count, 2);
}

// ─── staleness at the store boundary ─────────────────────────────────────────

#[test]
fn an_expired_row_is_still_listed_but_never_authoritative() {
    use crate::catalog::NotAuthoritative;
    use crate::db::model_catalog::list_authoritative_deployments;

    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import the env row");

    let mut fetched = NewModelDeployment::observed(
        "provider:deepseek-chat",
        "acct-deepseek",
        ProtocolKind::OpenAiChatCompletions,
        "deepseek-chat",
        CatalogSource::ProviderApi,
        "2026-08-10T00:00:00.000Z",
    );
    fetched.expires_at = Some("2026-08-11T00:00:00.000Z".to_string());
    upsert_model_deployment(&conn, &fetched).expect("import the fetched row");

    let now = "2026-08-11T06:00:00.000Z";
    let (admitted, excluded) = list_authoritative_deployments(&conn, now).expect("partition");

    assert_eq!(
        admitted
            .iter()
            .map(|row| row.get().deployment_id.as_str())
            .collect::<Vec<_>>(),
        vec!["env:extract"],
        "the env row has no declared expiry; the fetched one has passed its"
    );
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].0, "provider:deepseek-chat");
    assert!(matches!(excluded[0].1, NotAuthoritative::Expired { .. }));

    assert_eq!(
        list_model_deployments(&conn).expect("all rows").len(),
        2,
        "staleness is not deletion — the row stays readable, it just stops speaking for the \
         present"
    );
}

// ─── the health single writer (PR-C) ─────────────────────────────────────────

fn instant(seconds: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("fixed test instant")
}

fn iso(instant: chrono::DateTime<chrono::Utc>) -> String {
    instant.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A `5xx` outcome, built through the fallible constructor.
fn server_error(status: u16) -> DeploymentOutcome {
    DeploymentOutcome::ServerError {
        status: ServerErrorStatus::new(status).expect("a 5xx status"),
        retry_after: None,
    }
}

/// The target an `env:extract` request produces — endpoint and model as
/// [`extract_lane`] recorded them.
fn extract_request() -> DeploymentOutcomeTarget<'static> {
    DeploymentOutcomeTarget::request(
        "env:extract",
        "https://api.siliconflow.cn/v1/chat/completions",
        "Qwen/Qwen3.5-27B",
    )
}

#[test]
fn a_throttle_writes_health_and_its_event_and_leaves_the_catalog_row_alone() {
    // Discrimination 5 again, this time against the *sanctioned* writer rather
    // than the raw-SQL stand-in above: the one path a 429 can take must not be
    // able to touch what the deployment is.
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");
    let before = get_model_deployment(&conn, "env:extract")
        .expect("read")
        .expect("row");

    let write = record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Throttled {
            retry_after: Some(RetryAfter::DeltaSeconds(45)),
        },
        EvidenceKind::SelfReported,
        instant(0),
    )
    .expect("record succeeds");

    let DeploymentHealthWrite::Recorded {
        event_id,
        ref state,
        ref cooldown_until,
    } = write
    else {
        panic!("a matching request must be recorded, got {write:?}");
    };
    assert!(event_id > 0);
    assert_eq!(state, "cooldown");
    assert_eq!(cooldown_until.as_deref(), Some(iso(instant(45))).as_deref());

    let after = get_model_deployment(&conn, "env:extract")
        .expect("read")
        .expect("row");
    assert_eq!(
        before, after,
        "the catalog row must be byte-identical: a provider throttling us says nothing about the \
         deployment's capabilities, context window, pricing pointer or revision"
    );

    let health = get_model_deployment_health(&conn, "env:extract")
        .expect("health read")
        .expect("health row exists");
    assert_eq!(health.state, "cooldown");
    assert_eq!(
        health.cooldown_until.as_deref(),
        Some(iso(instant(45))).as_deref(),
        "the cooldown is a health-row state, never a catalog column"
    );
    assert_eq!(health.error_count, 1);
    assert_eq!(health.evidence_kind, Some(EvidenceKind::SelfReported));
    assert_eq!(health.observed_at, iso(instant(0)));

    let events = list_model_deployment_events(&conn, "env:extract").expect("events");
    assert_eq!(events.len(), 2, "import, then exactly one health event");
    assert_eq!(
        events[1].event_kind,
        DeploymentEventKind::HealthCooldown.as_str()
    );
    assert_eq!(
        events[1].revision, 1,
        "a health event carries the catalog revision it observed and does not advance it"
    );
}

/// Every table the credential and account authorities own (#1680 D1/D5/D6).
/// Named rather than derived so that adding one to the schema without adding
/// it here is a decision someone has to make, not an omission that hides.
const CREDENTIAL_AND_ACCOUNT_TABLES: [&str; 5] = [
    "vault_key_health",
    "provider_accounts",
    "provider_account_aliases",
    "provider_account_events",
    "account_custody",
];

/// Every row of `table`, every column, as comparable text.
///
/// `SELECT *` on purpose: naming columns would make this blind to a column
/// added later, and the property under test is that *nothing whatsoever* in
/// these tables moves. Sorted so the comparison does not depend on SQLite's
/// scan order.
fn dump_table(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("SELECT * FROM {table}"))
        .expect("prepare dump");
    let columns = stmt.column_count();
    let rows = stmt
        .query_map([], |row| {
            let mut cells = Vec::with_capacity(columns);
            for index in 0..columns {
                cells.push(format!("{:?}", row.get_ref(index)?));
            }
            Ok(cells.join("|"))
        })
        .expect("dump rows");
    let mut out: Vec<String> = rows.map(|row| row.expect("dump row")).collect();
    out.sort();
    out
}

fn dump_credential_and_account_tables(conn: &Connection) -> Vec<(&'static str, Vec<String>)> {
    CREDENTIAL_AND_ACCOUNT_TABLES
        .iter()
        .map(|table| (*table, dump_table(conn, table)))
        .collect()
}

/// Populate every credential and account table with a row that a forbidden
/// `UPDATE` or `DELETE` could visibly damage.
fn seed_credential_and_account_authorities(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO vault_key_health
            (logical_name, key_id, status, cooldown_until, last_success, last_attempt,
             last_error, error_count, auth_failed, disabled, metadata, updated_at)
         VALUES ('EXTRACT_API_KEY', 'EXTRACT_API_KEY_1', 'rate_limited',
                 '2026-08-11T00:05:00.000Z', '2026-08-10T23:00:00.000Z',
                 '2026-08-11T00:00:00.000Z', 'seeded credential error', 3, 0, 0,
                 '{\"seeded\":true}', '2026-08-11T00:00:00.000Z');

         INSERT INTO provider_accounts
            (account_id, provider_kind, auth_mode, auth_ref, account_fingerprint,
             account_class, capabilities, credential_policy_ref, refresh_authority,
             status, revision, source_refs, created_at, updated_at)
         VALUES ('acct-seeded', 'siliconflow', 'api_key_pool', 'vault:seeded', 'fp-seeded',
                 'model_api', '[\"chat\"]', NULL, 'none', 'active', 4, '[\"seed\"]',
                 '2026-08-10T00:00:00.000Z', '2026-08-10T00:00:00.000Z');

         INSERT INTO provider_account_aliases
            (account_id, alias_name, source_kind, first_seen, last_seen, retired)
         VALUES ('acct-seeded', 'EXTRACT_API_KEY', 'env', '2026-08-10T00:00:00.000Z',
                 '2026-08-11T00:00:00.000Z', 0);

         INSERT INTO provider_account_events
            (account_id, revision, event_kind, plan_digest, evidence, created_at)
         VALUES ('acct-seeded', 4, 'account_imported', 'digest-seeded', '{\"seeded\":true}',
                 '2026-08-10T00:00:00.000Z');

         INSERT INTO account_custody
            (auth_ref, account_id, custody_kind, custody_target, revision, updated_at)
         VALUES ('vault:seeded', 'acct-seeded', 'vault_rotation_pool', 'pool/seeded', 4,
                 '2026-08-10T00:00:00.000Z');",
    )
    .expect("seed the credential and account authorities");
}

#[test]
fn a_deployment_outcome_never_reaches_the_credential_authority() {
    // Discrimination 11 at the store boundary: the deployment writer's whole
    // type face is deployment-shaped, so a storm of outcomes must leave every
    // credential and account table byte-identical. A merged health score would
    // show up here first.
    //
    // The tables are **seeded first**. An earlier version of this test started
    // them empty and asserted `COUNT(*) == 0`, which a forbidden `UPDATE` or
    // `DELETE` would have passed without a murmur — there was nothing there to
    // damage (codex review of PR-C, NEW). Now every table holds a row whose
    // every column is compared before and after.
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");
    seed_credential_and_account_authorities(&conn);
    let before = dump_credential_and_account_tables(&conn);
    for (table, rows) in &before {
        assert!(!rows.is_empty(), "precondition: {table} must be seeded");
    }

    for outcome in [
        DeploymentOutcome::Throttled { retry_after: None },
        DeploymentOutcome::Unreachable,
        server_error(503),
        DeploymentOutcome::Served,
    ] {
        record_model_deployment_outcome(
            &conn,
            &extract_request(),
            outcome,
            EvidenceKind::SelfReported,
            instant(0),
        )
        .expect("record succeeds");
    }
    // The refused paths too: a skip must be as inert as a write.
    for target in [
        extract_request(),
        DeploymentOutcomeTarget::deployment("env:not-in-the-catalog"),
        DeploymentOutcomeTarget::request("env:extract", "https://elsewhere.test", "other-model"),
    ] {
        let _ = record_model_deployment_outcome(
            &conn,
            &target,
            DeploymentOutcome::ServerError {
                status: ServerErrorStatus::refused_for_tests(403),
                retry_after: None,
            },
            EvidenceKind::Probed,
            instant(0),
        );
    }

    assert_eq!(
        dump_credential_and_account_tables(&conn),
        before,
        "recording deployment health must not create, clear, update or delete anything in the \
         credential and account authorities — the three share nothing but a database file"
    );

    // …and the deployment authority did do its job, so the equality above is
    // not the vacuous "nothing happened at all".
    assert!(get_model_deployment_health(&conn, "env:extract")
        .expect("read")
        .is_some());
}

#[test]
fn an_outcome_for_a_deployment_the_catalog_does_not_know_is_skipped() {
    // The fail-safe the lane path depends on: a health record that has nowhere
    // to land is a counted skip, never an error travelling back up the call
    // that produced it.
    let conn = catalog_conn();

    let write = record_model_deployment_outcome(
        &conn,
        &DeploymentOutcomeTarget::deployment("env:reasoning"),
        DeploymentOutcome::Throttled { retry_after: None },
        EvidenceKind::SelfReported,
        instant(0),
    )
    .expect("a missing deployment is not an error");

    assert_eq!(
        write,
        DeploymentHealthWrite::Skipped(DeploymentHealthSkip::NoSuchDeployment)
    );
    assert!(get_model_deployment_health(&conn, "env:reasoning")
        .expect("read")
        .is_none());
    assert!(list_all_model_deployment_events(&conn)
        .expect("events")
        .is_empty());
}

#[test]
fn a_fallback_tiers_throttle_is_not_recorded_against_the_lanes_primary_row() {
    // The mis-attribution this target type exists to prevent: #1197's
    // cross-provider fallback sends the request to a provider the `env:extract`
    // row does not describe. Cooling `env:extract` down for it would be a
    // fabricated health fact about a deployment that throttled nothing.
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");

    let from_fallback = DeploymentOutcomeTarget::request(
        "env:extract",
        "https://api.deepseek.com/v1/chat/completions",
        "deepseek-chat",
    );
    let write = record_model_deployment_outcome(
        &conn,
        &from_fallback,
        DeploymentOutcome::Throttled { retry_after: None },
        EvidenceKind::SelfReported,
        instant(0),
    )
    .expect("a mismatch is not an error");
    assert_eq!(
        write,
        DeploymentHealthWrite::Skipped(DeploymentHealthSkip::DescribesADifferentRequest)
    );

    // Same endpoint, different model — a `model_override` — is the other half
    // of the same trap.
    let overridden = DeploymentOutcomeTarget::request(
        "env:extract",
        "https://api.siliconflow.cn/v1/chat/completions",
        "Qwen/Qwen3.5-72B",
    );
    assert_eq!(
        record_model_deployment_outcome(
            &conn,
            &overridden,
            DeploymentOutcome::Throttled { retry_after: None },
            EvidenceKind::SelfReported,
            instant(0),
        )
        .expect("record"),
        DeploymentHealthWrite::Skipped(DeploymentHealthSkip::DescribesADifferentRequest)
    );

    assert!(
        get_model_deployment_health(&conn, "env:extract")
            .expect("read")
            .is_none(),
        "neither mis-attributed outcome may leave a trace on the primary row's health"
    );
    assert_eq!(
        list_model_deployment_events(&conn, "env:extract")
            .expect("events")
            .len(),
        1,
        "and neither may append an event"
    );
}

/// Make the event append — and only the event append — fail, from inside the
/// database.
///
/// A trigger rather than a dropped table so the assertions afterwards can read
/// both tables and prove the *whole* transition unwound: an injection that
/// removed the events table would leave "no event" untestable and could not
/// show that a pre-existing health row survived byte-identical.
fn break_the_event_append(conn: &Connection) {
    conn.execute_batch(
        "CREATE TRIGGER refuse_event_append BEFORE INSERT ON model_deployment_events
         BEGIN SELECT RAISE(ABORT, 'injected append failure'); END",
    )
    .expect("install the failure injection");
}

#[test]
fn the_store_door_refuses_an_auth_class_status_even_when_handed_one() {
    // Defence in depth, and the reason the test-only unchecked constructors
    // exist. In production `ServerErrorStatus::new(401)` returns `None` and the
    // field is private, so this outcome cannot be built at all — which would
    // leave the door's own re-check unreachable and therefore unproven. Here
    // the seal is deliberately bypassed to prove the *second* lock holds on its
    // own, because that is the lock that survives someone loosening the first.
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");

    let smuggled = [
        DeploymentOutcome::ServerError {
            status: ServerErrorStatus::refused_for_tests(401),
            retry_after: Some(RetryAfter::DeltaSeconds(30)),
        },
        DeploymentOutcome::UnusableResponse {
            status: Some(UnusableResponseStatus::refused_for_tests(403)),
        },
    ];
    for outcome in smuggled {
        assert_eq!(
            record_model_deployment_outcome(
                &conn,
                &extract_request(),
                outcome,
                EvidenceKind::SelfReported,
                instant(0),
            )
            .expect("a refused outcome is a skip, not an error"),
            DeploymentHealthWrite::Skipped(DeploymentHealthSkip::AuthClassStatus),
            "{outcome:?} carries a status that belongs to the credential and account \
             authorities; recording it here would cool down every sibling deployment sharing \
             the rejected key"
        );
    }

    assert!(
        get_model_deployment_health(&conn, "env:extract")
            .expect("read")
            .is_none(),
        "no row"
    );
    assert_eq!(
        list_model_deployment_events(&conn, "env:extract")
            .expect("events")
            .len(),
        1,
        "and no event — only the import's"
    );
}

#[test]
fn an_event_append_failure_rolls_the_health_row_back_with_it() {
    // The atomicity discrimination (#1681 CP4/CP7): the health row and its
    // event land together or not at all. Without one transaction the upsert
    // commits on its own and the table holds a state no event accounts for —
    // which is precisely the equality the catalog fold projects.
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");

    // A prior, successful observation, so the test can distinguish "rolled
    // back to the previous state" from "never wrote anything at all".
    record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Served,
        EvidenceKind::SelfReported,
        instant(0),
    )
    .expect("the first record succeeds");
    let before = get_model_deployment_health(&conn, "env:extract")
        .expect("read")
        .expect("row");
    let events_before = list_model_deployment_events(&conn, "env:extract").expect("events");

    break_the_event_append(&conn);

    let err = record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Throttled {
            retry_after: Some(RetryAfter::DeltaSeconds(45)),
        },
        EvidenceKind::SelfReported,
        instant(60),
    )
    .expect_err("an append the database refuses must surface as an error, not a silent half-write");
    assert!(
        err.to_string().contains("injected append failure"),
        "the caller must see why, got: {err}"
    );

    assert_eq!(
        get_model_deployment_health(&conn, "env:extract")
            .expect("read")
            .expect("the earlier row is still there"),
        before,
        "the failed throttle must leave the health row byte-identical: a cooldown whose event \
         never landed is a state nothing in the log can explain"
    );
    assert_eq!(
        list_model_deployment_events(&conn, "env:extract").expect("events"),
        events_before,
        "and nothing may have been appended either"
    );
}

#[test]
fn a_first_outcome_whose_event_cannot_be_appended_leaves_no_row_at_all() {
    // The other half: with no prior row, the rollback must remove the row the
    // upsert created rather than leaving a fresh event-less one behind.
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");
    break_the_event_append(&conn);

    record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Unreachable,
        EvidenceKind::Probed,
        instant(0),
    )
    .expect_err("the append is refused");

    assert!(
        get_model_deployment_health(&conn, "env:extract")
            .expect("read")
            .is_none(),
        "no event, no row"
    );
}

/// A second deployment, so a caller's own work is distinguishable from the
/// store door's.
fn summary_lane() -> NewModelDeployment {
    let mut lane = extract_lane();
    lane.deployment_id = "env:summary".to_string();
    lane.provider_model_id = "Qwen/Qwen3.5-7B".to_string();
    lane
}

/// A transaction can be opened on this connection — which it cannot be if one
/// is already open, since SQLite has no nested transactions. Sharper than
/// `is_autocommit()` alone: it asserts the state the *next* caller will meet.
fn assert_no_transaction_is_open(conn: &Connection, why: &str) {
    assert!(conn.is_autocommit(), "{why}");
    conn.execute_batch("BEGIN IMMEDIATE")
        .unwrap_or_else(|err| panic!("{why}: {err}"));
    conn.execute_batch("ROLLBACK").expect("close it again");
}

#[test]
fn a_failed_write_returns_a_connection_this_door_opened_to_its_caller_closed() {
    // The owned-transaction half of the failure contract. The door opens its
    // own transaction now (CP5), which makes closing it on every exit the
    // door's job: a caller handed back a connection sitting inside a
    // transaction it never started would enrol every later write in it and see
    // its next `BEGIN` refused.
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");
    break_the_event_append(&conn);

    record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Unreachable,
        EvidenceKind::Probed,
        instant(0),
    )
    .expect_err("the append is refused");

    assert_no_transaction_is_open(
        &conn,
        "a transaction this function opened is this function's to close, on the failure path too",
    );
}

#[test]
fn a_failed_write_inside_a_callers_transaction_leaves_the_caller_in_charge() {
    // The nested half. The door must undo its own work and nothing else: the
    // caller's transaction stays open, its own writes stay in it, and the
    // caller — not this function — decides what a failed inner write means.
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");

    conn.execute_batch("BEGIN IMMEDIATE")
        .expect("the caller opens its own transaction");
    upsert_model_deployment(&conn, &summary_lane()).expect("the caller's own write");
    break_the_event_append(&conn);

    let err = record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Served,
        EvidenceKind::SelfReported,
        instant(0),
    )
    .expect_err("the append is refused");
    assert!(
        err.to_string().contains("injected append failure"),
        "the caller must see why, got: {err}"
    );

    assert!(
        !conn.is_autocommit(),
        "the door must not close a transaction it did not open — the caller's work is still \
         uncommitted and only the caller knows whether that is now wrong"
    );
    assert!(
        get_model_deployment(&conn, "env:summary")
            .expect("read")
            .is_some(),
        "and it must not roll the caller's own work back either"
    );
    assert!(
        get_model_deployment_health(&conn, "env:extract")
            .expect("read")
            .is_none(),
        "its own half is gone, though"
    );

    conn.execute_batch("COMMIT")
        .expect("the caller's transaction is still its own to commit");
    assert!(
        get_model_deployment(&conn, "env:summary")
            .expect("read")
            .is_some(),
        "and what it committed is durable"
    );
}

#[test]
fn a_commit_the_database_refuses_still_closes_the_transaction_it_opened() {
    // NEW2. The success path used to `?` straight out of `RELEASE`, so a
    // refused commit — which SQLite answers by leaving the transaction *open* —
    // skipped the unwind that every closure error got, and the caller received
    // an error plus a connection still inside a transaction.
    //
    // Driven through the door's own commit/unwind pair rather than through a
    // refused commit, because these tables carry no deferred constraint that
    // could refuse one: a `RELEASE` naming a savepoint that is not on the stack
    // fails exactly as a refused commit does, and what is under test is what
    // the connection looks like afterwards.
    let conn = catalog_conn();
    conn.execute_batch("BEGIN IMMEDIATE")
        .expect("the transaction the door would have opened");

    crate::db::model_catalog::commit_or_unwind_outcome_transaction_for_tests(&conn, true)
        .expect_err("a commit the database refuses is an error");

    assert_no_transaction_is_open(
        &conn,
        "a refused commit must still return the connection to its caller closed",
    );
}

#[test]
fn a_success_after_a_cooldown_clears_it_through_the_store() {
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");

    record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Throttled {
            retry_after: Some(RetryAfter::DeltaSeconds(45)),
        },
        EvidenceKind::SelfReported,
        instant(0),
    )
    .expect("throttle");
    record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Served,
        EvidenceKind::SelfReported,
        instant(60),
    )
    .expect("success");

    let health = get_model_deployment_health(&conn, "env:extract")
        .expect("read")
        .expect("row");
    assert_eq!(health.state, "ok");
    assert_eq!(health.cooldown_until, None);
    assert_eq!(health.error_count, 0);
    assert_eq!(
        health.last_success_at.as_deref(),
        Some(iso(instant(60))).as_deref()
    );
    assert_eq!(
        list_model_deployment_events(&conn, "env:extract")
            .expect("events")
            .len(),
        3,
        "one row, two health events — the log keeps both, the row keeps the latest"
    );
}

#[test]
fn two_connections_committing_out_of_order_leave_the_latest_observation_standing() {
    // The ordering discrimination (#1681 CP5). The production shape: a 429 is
    // observed, its write is scheduled off the call path, the lane retries
    // immediately and succeeds — and the throttle's write commits *after* the
    // success's. Two connections onto one file database reproduce exactly that
    // without a race: the writes are separate, and they are deliberately
    // committed in the wrong order.
    //
    // A single-connection version of this test cannot fail: it would be
    // sequential by construction, which is why the codex review called the
    // existing success-after-throttle test blind to this.
    let temp = tempfile::tempdir().expect("temp dir");
    let db_path = temp.path().join("catalog.db");
    let path = db_path.to_str().expect("utf-8 path");

    // A file-backed schema needs the FTS tokenizer and the vector extension
    // registered in this process; the in-memory helper above inherits them
    // from whichever test registered them first, which a single-test run does
    // not. Registration is idempotent (`register_once`).
    let _ = crate::db::enable_simple_auto_extension();
    crate::db::register_sqlite_vec();

    let writer = Connection::open(path).expect("open writer");
    init_schema(&writer).expect("schema initializes");
    upsert_model_deployment(&writer, &extract_lane()).expect("import");

    let throttled_at = instant(0);
    let served_at = instant(5);

    // The later observation commits first.
    let served = Connection::open(path).expect("open the success connection");
    assert!(matches!(
        record_model_deployment_outcome(
            &served,
            &extract_request(),
            DeploymentOutcome::Served,
            EvidenceKind::SelfReported,
            served_at,
        )
        .expect("record the success"),
        DeploymentHealthWrite::Recorded { .. }
    ));

    // The earlier one lands second, from a different connection.
    let throttled = Connection::open(path).expect("open the throttle connection");
    assert_eq!(
        record_model_deployment_outcome(
            &throttled,
            &extract_request(),
            DeploymentOutcome::Throttled {
                retry_after: Some(RetryAfter::DeltaSeconds(45)),
            },
            EvidenceKind::SelfReported,
            throttled_at,
        )
        .expect("a stale observation is not an error"),
        DeploymentHealthWrite::Skipped(DeploymentHealthSkip::StaleObservation),
        "a throttle observed before a success that already landed must be dropped, not applied"
    );

    let health = get_model_deployment_health(&writer, "env:extract")
        .expect("read")
        .expect("row");
    assert_eq!(
        health.state, "ok",
        "the terminal state must be the most recent observation, not the last write to arrive"
    );
    assert_eq!(
        health.cooldown_until, None,
        "a late-arriving 429 must not park a deployment that has since served a request"
    );
    assert_eq!(health.observed_at, iso(served_at));
    assert_eq!(
        list_model_deployment_events(&writer, "env:extract")
            .expect("events")
            .len(),
        2,
        "import plus the one health event that was actually applied — a dropped observation \
         appends nothing either, or the fold would disagree with the row"
    );
}

/// A file-backed catalog database and a connection onto it, with the schema
/// and the process-wide extensions the in-memory helper inherits.
fn catalog_file_db(dir: &std::path::Path) -> (String, Connection) {
    let path = dir
        .join("catalog.db")
        .to_str()
        .expect("utf-8 path")
        .to_string();
    let _ = crate::db::enable_simple_auto_extension();
    crate::db::register_sqlite_vec();
    let conn = open_catalog_connection(&path);
    init_schema(&conn).expect("schema initializes");
    (path, conn)
}

/// Another connection onto the same file, with a busy budget. rusqlite's
/// default is zero, which turns "wait for the writer that holds the lock" into
/// an instant `SQLITE_BUSY` and makes a contended schedule untestable.
fn open_catalog_connection(path: &str) -> Connection {
    let conn = Connection::open(path).expect("open the catalog database");
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .expect("busy budget");
    conn
}

#[test]
fn an_outcome_observed_during_another_writers_transaction_still_sees_its_commit() {
    // The *concurrent* half of CP5. The out-of-order test above cannot reach
    // it: it is sequential by construction — the success has committed before
    // the throttle connection is even opened — so it proves late arrival, not
    // a shared pre-write snapshot (codex re-review of PR-C, CP5).
    //
    // The schedule below is the one WAL actually produces. The throttle
    // connection holds the write lock with its transaction still open; the
    // success connection enters the store door while it is held; the throttle
    // commits underneath it. With a deferred transaction the success reads the
    // pre-throttle snapshot, passes the staleness check against a row that is
    // already obsolete, and then has its upsert refused with
    // `SQLITE_BUSY_SNAPSHOT` — which the busy handler does not retry — leaving
    // the superseded cooldown durable and the newer observation unrecorded.
    // With the write lock taken before the read, it waits, reads the throttle's
    // committed row, and supersedes it.
    let temp = tempfile::tempdir().expect("temp dir");
    let (path, reader) = catalog_file_db(temp.path());
    upsert_model_deployment(&reader, &extract_lane()).expect("import");

    let throttled_at = instant(0);
    let served_at = instant(5);

    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();

    let throttle_path = path.clone();
    let throttle = std::thread::spawn(move || {
        let conn = open_catalog_connection(&throttle_path);
        // The caller's own transaction: the store door nests in it, so nothing
        // this thread writes is visible until the `COMMIT` below.
        conn.execute_batch("BEGIN IMMEDIATE")
            .expect("take the write lock");
        record_model_deployment_outcome(
            &conn,
            &extract_request(),
            DeploymentOutcome::Throttled {
                retry_after: Some(RetryAfter::DeltaSeconds(45)),
            },
            EvidenceKind::SelfReported,
            throttled_at,
        )
        .expect("the throttle records inside the caller's transaction");
        locked_tx.send(()).expect("announce the held lock");
        release_rx
            .recv()
            .expect("wait for the success to be in flight");
        conn.execute_batch("COMMIT").expect("commit the throttle");
    });

    locked_rx
        .recv()
        .expect("the throttle connection holds the write lock");

    let success_path = path.clone();
    let success = std::thread::spawn(move || {
        let conn = open_catalog_connection(&success_path);
        record_model_deployment_outcome(
            &conn,
            &extract_request(),
            DeploymentOutcome::Served,
            EvidenceKind::SelfReported,
            served_at,
        )
    });

    // Long enough for the success connection to be inside the store door —
    // blocked on the write lock now, or (before this fix) already past its
    // reads and blocked on the upsert. Overshooting only makes the test
    // weaker, never flaky: it would merely let the success start after the
    // commit, which is the sequential case the test above already covers.
    std::thread::sleep(std::time::Duration::from_millis(300));
    release_tx.send(()).expect("release the throttle");
    throttle.join().expect("throttle thread");

    let write = success
        .join()
        .expect("success thread")
        .expect("a write that waited for the lock is not a failed write");
    assert!(
        matches!(write, DeploymentHealthWrite::Recorded { .. }),
        "the newer observation must land, got {write:?}"
    );

    let health = get_model_deployment_health(&reader, "env:extract")
        .expect("read")
        .expect("row");
    assert_eq!(
        health.state, "ok",
        "the surviving row must be the most recent observation, not the one that got the lock first"
    );
    assert_eq!(
        health.cooldown_until, None,
        "a cooldown the success superseded must not outlive it"
    );
    assert_eq!(health.observed_at, iso(served_at));
    assert_eq!(
        list_model_deployment_events(&reader, "env:extract")
            .expect("events")
            .len(),
        3,
        "the import, the throttle, and the success that superseded it — both writes are \
         observations and the log keeps both"
    );
}

#[test]
fn an_observation_at_the_same_instant_as_the_row_is_still_recorded() {
    // The guard is strictly-earlier: two outcomes sharing a timestamp are
    // indistinguishable in order, and refusing the second would silently drop
    // a real observation (every test instant in this module is a fixed clock).
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");

    for outcome in [DeploymentOutcome::Served, DeploymentOutcome::Unreachable] {
        assert!(matches!(
            record_model_deployment_outcome(
                &conn,
                &extract_request(),
                outcome,
                EvidenceKind::SelfReported,
                instant(0),
            )
            .expect("record"),
            DeploymentHealthWrite::Recorded { .. }
        ));
    }

    assert_eq!(
        get_model_deployment_health(&conn, "env:extract")
            .expect("read")
            .expect("row")
            .state,
        "error",
        "the second observation at the same instant still wins"
    );
}

#[test]
fn an_unparseable_observed_at_does_not_freeze_the_row() {
    // Fail-safe direction of the ordering guard: a row whose `observed_at`
    // carries no readable instant (a hand-written row, a future writer's
    // format) must not become permanently unwritable. No ordering information
    // means the guard cannot fire, not that everything is stale.
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");
    record_failure_health(&conn, "env:extract", "error", "seeded");
    conn.execute(
        "UPDATE model_deployment_health SET observed_at = 'not-a-timestamp' \
         WHERE deployment_id = 'env:extract'",
        [],
    )
    .expect("scramble the observation time");

    assert!(matches!(
        record_model_deployment_outcome(
            &conn,
            &extract_request(),
            DeploymentOutcome::Served,
            EvidenceKind::SelfReported,
            instant(0),
        )
        .expect("record"),
        DeploymentHealthWrite::Recorded { .. }
    ));
    assert_eq!(
        get_model_deployment_health(&conn, "env:extract")
            .expect("read")
            .expect("row")
            .state,
        "ok"
    );
}

#[test]
fn a_retired_deployment_still_records_what_happened_when_it_was_called() {
    // Deliberate: health is an observation, not an admission decision.
    // Refusing to record because the row is retired would lose the evidence
    // that the retirement was right. Admission is the resolver's gate (PR-D).
    let conn = catalog_conn();
    upsert_model_deployment(&conn, &extract_lane()).expect("import");
    retire_model_deployment(&conn, "env:extract").expect("retire");

    let write = record_model_deployment_outcome(
        &conn,
        &extract_request(),
        DeploymentOutcome::Unreachable,
        EvidenceKind::Probed,
        instant(0),
    )
    .expect("record");
    assert!(matches!(write, DeploymentHealthWrite::Recorded { .. }));

    let health = get_model_deployment_health(&conn, "env:extract")
        .expect("read")
        .expect("row");
    assert_eq!(health.state, "error");
    assert_eq!(health.evidence_kind, Some(EvidenceKind::Probed));
    assert_eq!(
        get_model_deployment(&conn, "env:extract")
            .expect("read")
            .expect("row")
            .revision,
        2,
        "the retirement revision, unmoved by the health write"
    );
}

#[test]
fn an_expiry_written_with_a_numeric_offset_is_honoured_as_an_instant() {
    // The store-level version of the lexical-comparison trap: SQLite would
    // compare these two strings the wrong way round, so the freshness decision
    // is deliberately not a `WHERE expires_at > ?` predicate.
    use crate::db::model_catalog::list_authoritative_deployments;

    let conn = catalog_conn();
    let mut row = NewModelDeployment::observed(
        "provider:withdrawn",
        "acct-provider",
        ProtocolKind::OpenAiChatCompletions,
        "withdrawn-model",
        CatalogSource::ProviderApi,
        "2026-08-10T00:00:00.000Z",
    );
    row.expires_at = Some("2026-08-11T08:00:00+08:00".to_string());
    upsert_model_deployment(&conn, &row).expect("import");

    let now = "2026-08-11T00:30:00.000Z";
    assert!(
        now < "2026-08-11T08:00:00+08:00",
        "precondition: sorts wrong"
    );

    let (admitted, excluded) = list_authoritative_deployments(&conn, now).expect("partition");
    assert!(
        admitted.is_empty(),
        "the row expired half an hour ago in real time"
    );
    assert_eq!(excluded.len(), 1);
}

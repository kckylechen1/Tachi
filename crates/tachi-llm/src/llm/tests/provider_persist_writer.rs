//! The provider persistence writer (audit H1): persistence reuses one opened
//! vault handle instead of paying a full `MemoryStore` open per event, merges
//! only plain-success key-health snapshots, and still lands every auth
//! failure, rate limit, cooldown and credential-generation change, with
//! counters accumulated exactly and `await_provider_health_persistence`
//! covering every write enqueued before the waiter.

use super::*;

use std::sync::Arc;

use memcore::db::model_catalog::get_model_deployment_health;
use memcore::vault::health::{
    credential_generation_from_metadata, EvidenceKind, TypedOutcome, HEALTH_STATUS_ERROR,
};

use crate::llm::catalog_import::{env_deployment_id, DeploymentAttribution};
use crate::llm::chat_lanes::persist_llm_usage_blocking;
use crate::llm::provider_health::{
    install_retained_post_commit_hook_for_tests, success_snapshots_merge, ChatLaneConfig,
    ProviderRuntimeConfig, SelectedProviderSecret, RETAINED_STORE_TTL,
};
use memcore::store::llm_usage::LlmUsageEvent;

const EXTRACT_ENDPOINT: &str = "https://api.siliconflow.cn/v1/chat/completions";
const EXTRACT_MODEL: &str = "Qwen/Qwen3.5-27B";

fn init_vault_db(db_path: &std::path::Path) {
    drop(memcore::MemoryStore::open(db_path.to_str().expect("utf-8 path")).expect("init db"));
}

fn catalog_config(key_env: &'static str) -> ProviderRuntimeConfig {
    let lane = |model: &str| ChatLaneConfig {
        base_url: EXTRACT_ENDPOINT.to_string(),
        model: model.to_string(),
        api_key_envs: vec![key_env],
    };
    ProviderRuntimeConfig {
        extract: lane(EXTRACT_MODEL),
        summary: lane("Qwen/Qwen3.5-7B"),
        reasoning: lane("deepseek-reasoner"),
        distill: lane("deepseek-chat"),
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    }
}

/// A store holding the env chat-lane catalog rows, and a client recording
/// against it.
fn client_with_catalog(db_path: &std::path::Path, key_env: &'static str) -> LlmClient {
    let config = catalog_config(key_env);
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf-8 path"))
        .expect("initialize the store");
    crate::llm::catalog_import::import_env_chat_lanes(
        store.connection(),
        &config,
        "2026-08-13T00:00:00.000Z",
    )
    .expect("import the env lanes");
    drop(store);
    LlmClient::new_with_config(config, Some(db_path)).expect("client initializes")
}

fn extract_attribution() -> DeploymentAttribution<'static> {
    DeploymentAttribution::EnvLane {
        lane: "extract",
        endpoint: EXTRACT_ENDPOINT,
        model: EXTRACT_MODEL,
    }
}

fn selected(key_env: &str, generation: Option<u64>) -> SelectedProviderSecret {
    SelectedProviderSecret {
        logical_name: key_env.to_string(),
        key_id: format!("{key_env}_1"),
        value: String::new(),
        credential_generation: generation,
    }
}

fn stored_key_health(db_path: &std::path::Path, logical: &str, key: &str) -> VaultKeyHealth {
    memcore::MemoryStore::open(db_path.to_str().unwrap())
        .expect("reopen")
        .vault_get_key_health(logical, key)
        .expect("read key health")
        .expect("key health row")
}

fn deployment_event_count(db_path: &std::path::Path, deployment_id: &str) -> i64 {
    rusqlite::Connection::open(db_path)
        .expect("open db")
        .query_row(
            "SELECT COUNT(*) FROM model_deployment_events WHERE deployment_id = ?1",
            [deployment_id],
            |row| row.get(0),
        )
        .expect("count deployment events")
}

/// The persisted row is the in-memory row except for `updated_at`, which the
/// persist path stamps at enqueue time.
fn assert_same_snapshot(stored: &VaultKeyHealth, memory: &VaultKeyHealth) {
    assert_eq!(stored.status, memory.status);
    assert_eq!(stored.cooldown_until, memory.cooldown_until);
    assert_eq!(stored.last_success, memory.last_success);
    assert_eq!(stored.last_attempt, memory.last_attempt);
    assert_eq!(stored.last_error, memory.last_error);
    assert_eq!(stored.error_count, memory.error_count);
    assert_eq!(stored.auth_failed, memory.auth_failed);
    assert_eq!(stored.disabled, memory.disabled);
    assert_eq!(stored.metadata, memory.metadata);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn identical_success_events_pay_one_full_open_and_land_the_final_snapshot() {
    const LOGICAL: &str = "TACHI_TEST_ONLY_H1_IDENTICAL_SUCCESS";
    const KEY: &str = "TACHI_TEST_ONLY_H1_IDENTICAL_SUCCESS_1";
    const N: u64 = 40;
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    init_vault_db(&db_path);
    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client");

    for _ in 0..N {
        client.record_provider_key_result(LOGICAL, KEY, Some(200), None, None, None);
    }
    client
        .await_provider_health_persistence()
        .await
        .expect("every success persists");

    let counts = client.provider_persist_writer.counts();
    assert_eq!(
        counts.full_opens, 1,
        "{N} identical successes must share one full open: {counts:?}"
    );
    let written = counts.full_opens + counts.retained_writes;
    assert_eq!(
        written + counts.coalesced_key_health,
        N,
        "every event is either written or merged into a later write: {counts:?}"
    );
    assert_eq!(
        client.provider_persist_writer.key_health_writes().len() as u64,
        written
    );

    let memory = client
        .provider_key_health_for_tests(LOGICAL, KEY)
        .expect("in-memory row");
    assert_same_snapshot(&stored_key_health(&db_path, LOGICAL, KEY), &memory);
    let status = client.provider_health_status();
    assert!(status.persist_last_error.is_none(), "{status:?}");
    assert!(status.persist_last_success_at.is_some());
}

#[test]
fn counters_accumulate_exactly_through_the_retained_handle() {
    const KEY_ENV: &str = "TACHI_TEST_ONLY_H1_COUNTERS_API_KEY";
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path, KEY_ENV);
    let secret = selected(KEY_ENV, None);
    let deployment_id = env_deployment_id("extract");
    let events_before = deployment_event_count(&db_path, &deployment_id);

    for _ in 0..3 {
        client.apply_key_outcome(
            &secret,
            TypedOutcome::Success,
            EvidenceKind::SelfReported,
            None,
            extract_attribution(),
        );
    }
    for attempt in 0..5 {
        let reason = format!("provider returned HTTP 500 ({attempt})");
        client.apply_key_outcome(
            &secret,
            TypedOutcome::Error,
            EvidenceKind::SelfReported,
            Some(&reason),
            extract_attribution(),
        );
    }
    for _ in 0..4 {
        client.note_deployment_http_status(extract_attribution(), 503, None);
    }

    let counts = client.provider_persist_writer.counts();
    // 8 credential writes + 3 served + 4 deployment-only outcomes.
    assert_eq!(counts.full_opens, 1, "{counts:?}");
    assert_eq!(counts.retained_writes, 14, "{counts:?}");
    assert_eq!(
        counts.coalesced_key_health, 0,
        "synchronous writes never merge"
    );

    let credential = stored_key_health(&db_path, KEY_ENV, &format!("{KEY_ENV}_1"));
    assert_eq!(credential.status, HEALTH_STATUS_ERROR);
    assert_eq!(
        credential.error_count, 5,
        "error counts accumulate, never last-write-wins"
    );

    assert_eq!(client.deployment_health_record_counts().recorded, 7);
    assert_eq!(
        deployment_event_count(&db_path, &deployment_id) - events_before,
        7,
        "every deployment outcome is its own event"
    );
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    let deployment = get_model_deployment_health(store.connection(), &deployment_id)
        .expect("read deployment")
        .expect("deployment row");
    assert_eq!(deployment.error_count, 4, "four 503s after three successes");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn auth_failure_rate_limit_cooldown_and_generation_change_each_reach_the_store() {
    const KEY_ENV: &str = "TACHI_TEST_ONLY_H1_TRANSITIONS_API_KEY";
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    init_vault_db(&db_path);
    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client");
    let key_id = format!("{KEY_ENV}_1");
    let first = selected(KEY_ENV, Some(1));
    let rotated = selected(KEY_ENV, Some(2));
    let apply = |secret: &SelectedProviderSecret, outcome: TypedOutcome| {
        client.apply_key_outcome(
            secret,
            outcome,
            EvidenceKind::SelfReported,
            None,
            DeploymentAttribution::Unattributed,
        )
    };

    // Hold every background write back so all events are pending together:
    // the merge rule, not scheduling luck, decides what is written.
    let held = client.background_persist_lock.lock().await;
    apply(&first, TypedOutcome::Success);
    apply(&first, TypedOutcome::Success);
    let auth_failed = apply(&first, TypedOutcome::AuthFailed);
    apply(&first, TypedOutcome::Success);
    apply(&first, TypedOutcome::Success);
    let rate_limited = apply(
        &first,
        TypedOutcome::RateLimited {
            retry_after_secs: Some(30),
        },
    );
    apply(&first, TypedOutcome::Success);
    let generation_change = apply(&rotated, TypedOutcome::Success);
    apply(&rotated, TypedOutcome::Success);

    let pending = client
        .provider_persist_writer
        .pending_key_health(KEY_ENV, &key_id);
    let pending_statuses: Vec<&str> = pending.iter().map(|row| row.status.as_str()).collect();
    assert_eq!(
        pending_statuses,
        ["ok", "auth_failed", "ok", "rate_limited", "ok", "ok"],
        "only plain successes of one generation merge"
    );
    drop(held);
    client
        .await_provider_health_persistence()
        .await
        .expect("persistence completes");

    let writes = client.provider_persist_writer.key_health_writes();
    let written_statuses: Vec<&str> = writes.iter().map(|row| row.status.as_str()).collect();
    assert_eq!(
        written_statuses,
        ["ok", "auth_failed", "ok", "rate_limited", "ok", "ok"],
        "every non-success transition is written, in order"
    );
    assert_eq!(
        client.provider_persist_writer.counts().coalesced_key_health,
        3
    );

    assert_same_snapshot(&writes[1], &auth_failed);
    assert!(writes[1].auth_failed);
    assert_same_snapshot(&writes[3], &rate_limited);
    assert!(
        writes[3].cooldown_until.is_some(),
        "the cooldown window is persisted"
    );
    assert_eq!(writes[3].error_count, 1);
    assert_eq!(
        credential_generation_from_metadata(&writes[4].metadata),
        Some(1)
    );
    assert_eq!(
        credential_generation_from_metadata(&writes[5].metadata),
        Some(2)
    );
    assert_eq!(
        credential_generation_from_metadata(&generation_change.metadata),
        Some(2)
    );

    let memory = client
        .provider_key_health_for_tests(KEY_ENV, &key_id)
        .expect("in-memory row");
    assert_same_snapshot(&writes[5], &memory);
    assert_same_snapshot(&stored_key_health(&db_path, KEY_ENV, &key_id), &memory);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn a_waiter_observes_every_write_enqueued_before_it() {
    const KEY_ENV: &str = "TACHI_TEST_ONLY_H1_WAITER_API_KEY";
    const N: usize = 10;
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path, KEY_ENV);
    let secret = selected(KEY_ENV, None);
    let events_before = deployment_event_count(&db_path, &env_deployment_id("extract"));

    let held = client.background_persist_lock.lock().await;
    for _ in 0..N {
        client.apply_key_outcome(
            &secret,
            TypedOutcome::Success,
            EvidenceKind::SelfReported,
            None,
            extract_attribution(),
        );
    }
    let waiter_client = client.clone();
    let waiter =
        tokio::spawn(async move { waiter_client.await_provider_health_persistence().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !waiter.is_finished(),
        "the waiter must not return while enqueued writes are held back"
    );
    drop(held);
    waiter
        .await
        .expect("join waiter")
        .expect("every enqueued write succeeds");

    // Everything enqueued before the waiter is durable once it returns.
    let memory = client
        .provider_key_health_for_tests(KEY_ENV, &format!("{KEY_ENV}_1"))
        .expect("in-memory row");
    assert_same_snapshot(
        &stored_key_health(&db_path, KEY_ENV, &format!("{KEY_ENV}_1")),
        &memory,
    );
    assert_eq!(client.deployment_health_record_counts().recorded, N as u64);
    assert_eq!(
        deployment_event_count(&db_path, &env_deployment_id("extract")) - events_before,
        N as i64
    );
    let counts = client.provider_persist_writer.counts();
    assert_eq!(counts.full_opens, 1, "{counts:?}");
}

#[test]
fn the_retained_handle_is_dropped_on_schema_change_ttl_failure_and_path_replacement() {
    const LOGICAL: &str = "TACHI_TEST_ONLY_H1_INVALIDATION";
    const KEY: &str = "TACHI_TEST_ONLY_H1_INVALIDATION_1";
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    init_vault_db(&db_path);
    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client");
    let writer = Arc::clone(&client.provider_persist_writer);
    let succeed = || client.record_provider_key_result(LOGICAL, KEY, Some(200), None, None, None);
    let full_opens = || writer.counts().full_opens;

    succeed();
    succeed();
    assert_eq!(full_opens(), 1, "the second write reuses the first open");
    if !writer.has_retained_store() {
        // No stable file identity on this target: every write keeps its own
        // open, which is the pre-H1 behavior. Nothing below applies.
        return;
    }

    // Another connection's full open (schema init included) of the same file
    // does not invalidate the handle.
    init_vault_db(&db_path);
    succeed();
    assert_eq!(full_opens(), 1, "a peer open is not a schema change");

    // Any DDL by any connection forces a re-validating open.
    rusqlite::Connection::open(&db_path)
        .expect("peer connection")
        .execute_batch("CREATE TABLE tachi_test_only_h1_schema_bump(x)")
        .expect("peer DDL");
    succeed();
    assert_eq!(full_opens(), 2, "a schema change forces a full open");
    succeed();
    assert_eq!(full_opens(), 2);

    // The TTL bounds how long one open is trusted.
    writer.age_retained_for_tests(RETAINED_STORE_TTL);
    succeed();
    assert_eq!(full_opens(), 3, "an expired handle is re-opened");

    // A failed write drops the handle.
    let gate = rusqlite::Connection::open(&db_path).expect("writer gate");
    gate.execute_batch("BEGIN IMMEDIATE")
        .expect("hold the writer");
    succeed();
    let status = client.provider_health_status();
    let error = status
        .persist_last_error
        .expect("the held writer fails the write");
    assert!(
        error.contains(PROVIDER_HEALTH_PERSIST_SQLITE_DEADLINE_CAUSE),
        "{error}"
    );
    assert!(
        !writer.has_retained_store(),
        "a failed write drops the handle"
    );
    gate.execute_batch("COMMIT").expect("release the writer");
    succeed();
    assert_eq!(full_opens(), 4, "the write after a failure opens afresh");
    assert!(client.provider_health_status().persist_last_error.is_none());

    // Replacing the file at the path retargets the next write.
    for suffix in ["", "-wal", "-shm"] {
        let path = temp.path().join(format!("vault.db{suffix}"));
        if path.exists() {
            std::fs::remove_file(&path).expect("remove old db file");
        }
    }
    init_vault_db(&db_path);
    client.record_provider_key_result(LOGICAL, KEY, Some(429), None, Some(30), None);
    assert_eq!(full_opens(), 5, "a replaced path is re-opened");
    let replaced = stored_key_health(&db_path, LOGICAL, KEY);
    assert_eq!(
        replaced.status, HEALTH_RATE_LIMITED,
        "the row lands in the new file"
    );
}

/// Replace the database at `live` with a snapshot of itself that already
/// includes every committed transaction (so a just-committed row is present
/// in the replacement), as a backup or snapshot taken right after that
/// commit would be. The installed file is a new physical identity at the
/// same path, which is exactly what the post-write check must notice.
fn replace_path_with_snapshot_of_the_committed_db(live: &std::path::Path) {
    use rusqlite::backup::Backup;

    let snapshot_dir = tempfile::tempdir().expect("snapshot dir");
    let snapshot = snapshot_dir.path().join("committed-snapshot.db");
    {
        let source = rusqlite::Connection::open(live).expect("peer open of the committed db");
        let mut target = rusqlite::Connection::open(&snapshot).expect("snapshot target db");
        let backup = Backup::new(&source, &mut target).expect("backup handle");
        backup
            .run_to_completion(
                5,
                Duration::from_millis(1),
                None::<fn(rusqlite::backup::Progress)>,
            )
            .expect("snapshot the committed database");
    }
    for suffix in ["", "-wal", "-shm"] {
        let mut sidecar = live.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = std::path::PathBuf::from(sidecar);
        if sidecar.exists() {
            std::fs::remove_file(&sidecar).expect("remove the live db file set");
        }
    }
    std::fs::rename(&snapshot, live).expect("install the snapshot at the live path");
}

#[test]
fn a_deployment_event_committed_before_its_path_is_replaced_is_not_replayed() {
    const KEY_ENV: &str = "TACHI_TEST_ONLY_H1_SNAPSHOT_DEPLOY_API_KEY";
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path, KEY_ENV);
    let writer = Arc::clone(&client.provider_persist_writer);
    let deployment_id = env_deployment_id("extract");
    let events_before = deployment_event_count(&db_path, &deployment_id);

    client.note_deployment_http_status(extract_attribution(), 200, None);
    assert_eq!(
        writer.counts().full_opens,
        1,
        "the first outcome pays the full open and admits the handle"
    );
    if !writer.has_retained_store() {
        // No stable file identity on this target: every write keeps its
        // own open, so the retained-write window below cannot exist.
        return;
    }

    // While the next outcome's row is between its commit and the writer's
    // post-write identity check, replace the path with a snapshot that
    // already includes that commit.
    let replaced_path = db_path.clone();
    let _hook = install_retained_post_commit_hook_for_tests(&db_path, move || {
        replace_path_with_snapshot_of_the_committed_db(&replaced_path);
    });

    client.note_deployment_http_status(extract_attribution(), 503, None);

    assert_eq!(
        deployment_event_count(&db_path, &deployment_id) - events_before,
        2,
        "one 200 and one 503: the snapshot already includes the committed 503, so replaying the write would duplicate the event"
    );
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open the snapshot");
    let deployment = get_model_deployment_health(store.connection(), &deployment_id)
        .expect("read deployment")
        .expect("deployment row");
    assert_eq!(
        deployment.error_count, 1,
        "the single 503 advances the deployment once, not once per replay"
    );

    // The handle addressed the detached file and is gone: the next write
    // re-opens whatever the path names now and lands there.
    client.note_deployment_http_status(extract_attribution(), 200, None);
    let counts = writer.counts();
    assert_eq!(
        counts.full_opens, 2,
        "the write after the replacement re-opens the new identity: {counts:?}"
    );
    assert!(writer.has_retained_store(), "the new handle is admitted");
    assert_eq!(
        deployment_event_count(&db_path, &deployment_id) - events_before,
        3
    );
}

#[test]
fn an_llm_usage_row_committed_before_its_path_is_replaced_is_not_replayed() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    init_vault_db(&db_path);
    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client");
    let writer = Arc::clone(&client.provider_persist_writer);
    let usage_rows = || -> i64 {
        rusqlite::Connection::open(&db_path)
            .expect("open db")
            .query_row("SELECT COUNT(*) FROM llm_usage", [], |row| row.get(0))
            .expect("count usage rows")
    };
    let usage_event = |second: i64| LlmUsageEvent {
        timestamp: format!("2026-09-27T00:00:{second:02}.000Z"),
        lane: "extract".to_string(),
        model: EXTRACT_MODEL.to_string(),
        provider_host: "api.siliconflow.cn".to_string(),
        provider_logical_name: "TACHI_TEST_ONLY_H1_SNAPSHOT_USAGE".to_string(),
        provider_key_id: "TACHI_TEST_ONLY_H1_SNAPSHOT_USAGE_1".to_string(),
        prompt_tokens: Some(10 + second),
        completion_tokens: Some(second),
        total_tokens: Some(10 + 2 * second),
        max_tokens: 300,
        request_chars: 512,
        response_chars: 128,
        duration_ms: second,
    };
    let persist_usage = |second: i64| {
        persist_llm_usage_blocking(
            &writer,
            db_path.clone(),
            client.vault_db_migration.clone(),
            usage_event(second),
        )
    };

    persist_usage(0).expect("the first usage row pays the full open");
    assert_eq!(writer.counts().full_opens, 1);
    if !writer.has_retained_store() {
        // No stable file identity on this target: every write keeps its
        // own open, so the retained-write window below cannot exist.
        return;
    }

    let replaced_path = db_path.clone();
    let _hook = install_retained_post_commit_hook_for_tests(&db_path, move || {
        replace_path_with_snapshot_of_the_committed_db(&replaced_path);
    });

    persist_usage(1).expect("the committed usage row is reported as written");
    assert_eq!(
        usage_rows(),
        2,
        "the snapshot already includes the committed row; replaying the insert would duplicate the ledger entry"
    );

    // The handle addressed the detached file and is gone: the next usage
    // write re-opens the new identity and lands there.
    persist_usage(2).expect("the next usage row re-opens the new identity");
    assert_eq!(
        writer.counts().full_opens,
        2,
        "the write after the replacement re-opens the new identity"
    );
    assert!(writer.has_retained_store(), "the new handle is admitted");
    assert_eq!(usage_rows(), 3);
}

#[test]
fn only_plain_success_snapshots_of_one_evidence_merge() {
    let base = memcore::vault::health::record_key_outcome_for_generation(
        None,
        "L",
        "K",
        TypedOutcome::Success,
        EvidenceKind::SelfReported,
        None,
        Utc::now(),
        Some(7),
    )
    .health;
    let next = |outcome: TypedOutcome, evidence: EvidenceKind, generation: Option<u64>| {
        memcore::vault::health::record_key_outcome_for_generation(
            Some(&base),
            "L",
            "K",
            outcome,
            evidence,
            None,
            Utc::now() + chrono::Duration::seconds(1),
            generation,
        )
        .health
    };
    let success = next(TypedOutcome::Success, EvidenceKind::SelfReported, Some(7));
    assert!(success_snapshots_merge(&base, &success));

    let other_generation = next(TypedOutcome::Success, EvidenceKind::SelfReported, Some(8));
    assert!(!success_snapshots_merge(&base, &other_generation));
    let no_generation = next(TypedOutcome::Success, EvidenceKind::SelfReported, None);
    assert!(!success_snapshots_merge(&base, &no_generation));
    let probed = next(TypedOutcome::Success, EvidenceKind::Probed, Some(7));
    assert!(!success_snapshots_merge(&base, &probed));
    for outcome in [
        TypedOutcome::AuthFailed,
        TypedOutcome::ProbedUnauthorized,
        TypedOutcome::RateLimited {
            retry_after_secs: Some(5),
        },
        TypedOutcome::Exhausted,
        TypedOutcome::Error,
        TypedOutcome::Unknown,
    ] {
        let row = next(outcome, EvidenceKind::SelfReported, Some(7));
        assert!(
            !success_snapshots_merge(&base, &row),
            "{outcome:?} must not replace a pending success"
        );
        assert!(
            !success_snapshots_merge(&row, &success),
            "{outcome:?} must be written before a later success"
        );
    }
    let mut other_key = success.clone();
    other_key.key_id = "K2".to_string();
    assert!(!success_snapshots_merge(&base, &other_key));
}

/// #1680 D6 on the retained handle: a key-health write that reuses the
/// retained handle holds process startup ownership exactly like the
/// open-then-write it replaces, so no open in this process crosses the
/// startup boundary while that write is in progress.
#[test]
fn a_retained_key_health_write_keeps_startup_ownership() {
    const LOGICAL: &str = "TACHI_TEST_ONLY_H1_RETAINED_STARTUP_OWNERSHIP";
    const KEY: &str = "TACHI_TEST_ONLY_H1_RETAINED_STARTUP_OWNERSHIP_A";
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    init_vault_db(&db_path);
    let client = LlmClient::new_with_vault_db(Some(&db_path)).expect("client");

    client.record_provider_key_result(LOGICAL, KEY, Some(200), None, None, None);
    if !client.provider_persist_writer.has_retained_store() {
        // No stable file identity on this target: no handle is retained and
        // every write keeps its own open (covered by the pre-H1 D6 test).
        return;
    }

    let (write_held_tx, write_held_rx) = std::sync::mpsc::channel();
    let (release_write_tx, release_write_rx) = std::sync::mpsc::channel::<()>();
    let _write_hook =
        memcore::db::install_vault_key_health_write_hook_for_tests(LOGICAL, KEY, move || {
            write_held_tx.send(()).expect("report held health write");
            release_write_rx.recv().expect("release held health write");
        });
    let writer_client = client.clone();
    let writer = std::thread::spawn(move || {
        writer_client.record_provider_key_result(LOGICAL, KEY, Some(429), None, Some(30), None);
    });
    write_held_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the retained write must hold the real vault health transaction");

    let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
    let (owned_tx, owned_rx) = std::sync::mpsc::channel();
    let _startup_hook = memcore::MemoryStore::install_startup_ownership_hook_for_tests(
        db_path.to_str().expect("utf8 db path"),
        move || attempted_tx.send(()).expect("report open arriving"),
        move || owned_tx.send(()).expect("report open crossing"),
    );
    let open_path = db_path.clone();
    let opener = std::thread::spawn(move || {
        drop(memcore::MemoryStore::open(open_path.to_str().unwrap()).expect("peer open"));
    });
    attempted_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the peer open must reach the startup boundary");
    let crossed_while_held = owned_rx.recv_timeout(Duration::from_millis(100)).is_ok();

    release_write_tx.send(()).expect("release the held write");
    writer.join().expect("join writer");
    if !crossed_while_held {
        owned_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the peer open proceeds once the write releases ownership");
    }
    opener.join().expect("join opener");

    assert!(
        !crossed_while_held,
        "a peer open crossed the startup boundary during a retained key-health write"
    );
    let counts = client.provider_persist_writer.counts();
    assert_eq!(
        counts.full_opens, 1,
        "the held write was a retained write: {counts:?}"
    );
    assert_eq!(counts.retained_writes, 1, "{counts:?}");
    assert!(client.provider_health_status().persist_last_error.is_none());
    assert_eq!(
        stored_key_health(&db_path, LOGICAL, KEY).status,
        HEALTH_RATE_LIMITED
    );
}

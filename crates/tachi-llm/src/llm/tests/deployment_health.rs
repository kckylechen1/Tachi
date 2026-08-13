//! Production-seam discriminators for deployment health (tachi#1681 D4, PR-C
//! item 4).
//!
//! The one the design names: **429 is dual-recorded and 401/403 is not**. Both
//! halves are asserted against real rows in a real store, because the failure
//! mode this leaf exists to prevent — an auth failure cooling down a
//! deployment, or a deployment cooldown quietly rewriting a credential row —
//! is invisible at the type level once a seam decides to call the writer.

use super::*;

use memcore::db::model_catalog::get_model_deployment_health;
use memcore::vault::health::{EvidenceKind, TypedOutcome};

use crate::llm::catalog_import::{env_deployment_id, DeploymentAttribution};
use crate::llm::provider_health::{ChatLaneConfig, ProviderRuntimeConfig, SelectedProviderSecret};
use crate::{RerankConfig, RerankProviderKind};

const EXTRACT_ENDPOINT: &str = "https://api.siliconflow.cn/v1/chat/completions";
const EXTRACT_MODEL: &str = "Qwen/Qwen3.5-27B";
const KEY_ENV: &str = "TACHI_TEST_ONLY_DEPLOYMENT_HEALTH_API_KEY";

fn config() -> ProviderRuntimeConfig {
    let lane = |model: &str| ChatLaneConfig {
        base_url: EXTRACT_ENDPOINT.to_string(),
        model: model.to_string(),
        api_key_envs: vec![KEY_ENV],
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

/// A store with the four `env:` chat-lane rows the import produces, and the
/// client that will record against them.
fn client_with_catalog(db_path: &std::path::Path) -> LlmClient {
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf-8 path"))
        .expect("initialize the store");
    crate::llm::catalog_import::import_env_chat_lanes(
        store.connection(),
        &config(),
        "2026-08-13T00:00:00.000Z",
    )
    .expect("import the env lanes");
    drop(store);
    LlmClient::new_with_config(config(), Some(db_path)).expect("client initializes")
}

fn selected() -> SelectedProviderSecret {
    SelectedProviderSecret {
        logical_name: KEY_ENV.to_string(),
        key_id: format!("{KEY_ENV}_1"),
        value: String::new(),
    }
}

fn extract_attribution() -> DeploymentAttribution<'static> {
    DeploymentAttribution::EnvLane {
        lane: "extract",
        endpoint: EXTRACT_ENDPOINT,
        model: EXTRACT_MODEL,
    }
}

// ─── the attribution rule, at the seam ───────────────────────────────────────

#[test]
fn a_lane_throttle_is_recorded_on_the_credential_and_the_deployment() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    client.apply_key_outcome(
        &selected(),
        TypedOutcome::RateLimited {
            retry_after_secs: Some(30),
        },
        EvidenceKind::SelfReported,
        None,
        extract_attribution(),
    );

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");

    // Authority one: the credential that was throttled, unchanged behaviour.
    let credential = store
        .vault_get_key_health(KEY_ENV, &format!("{KEY_ENV}_1"))
        .expect("read credential health")
        .expect("the credential row still lands");
    assert_eq!(credential.status, HEALTH_RATE_LIMITED);
    assert!(credential.cooldown_until.is_some());

    // Authority two: the deployment that throttled it.
    let deployment = get_model_deployment_health(store.connection(), &env_deployment_id("extract"))
        .expect("read deployment health")
        .expect("the deployment row lands too — this is the dual record");
    assert_eq!(deployment.state, "cooldown");
    assert!(
        deployment.cooldown_until.is_some(),
        "a 429 must cool the deployment down, not only the key that carried it"
    );
    assert_eq!(deployment.evidence_kind, Some(EvidenceKind::SelfReported));
    assert_eq!(client.deployment_health_record_counts().recorded, 1);

    // …and the two cooldowns are separate facts: no sibling lane was touched.
    for lane in ["summary", "reasoning", "distill"] {
        assert!(
            get_model_deployment_health(store.connection(), &env_deployment_id(lane))
                .expect("read")
                .is_none(),
            "a throttle on one deployment must not write health for another"
        );
    }
}

#[test]
fn an_auth_failure_never_reaches_the_deployment_authority() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    // Attributed on purpose: the seam is *told* which deployment served the
    // request, and still must not record an auth outcome against it. A weaker
    // test (passing `Unattributed`) would pass for the wrong reason.
    client.apply_key_outcome(
        &selected(),
        TypedOutcome::AuthFailed,
        EvidenceKind::SelfReported,
        Some("401 unauthorized"),
        extract_attribution(),
    );

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    assert_eq!(
        store
            .vault_get_key_health(KEY_ENV, &format!("{KEY_ENV}_1"))
            .expect("read")
            .expect("the credential row is where an auth failure belongs")
            .status,
        HEALTH_AUTH_FAILED
    );
    assert!(
        get_model_deployment_health(store.connection(), &env_deployment_id("extract"))
            .expect("read")
            .is_none(),
        "an auth failure says nothing about the deployment; recording one here would cool down \
         every sibling deployment that shares the rejected key"
    );
    assert_eq!(client.deployment_health_record_counts().recorded, 0);
}

#[test]
fn an_outcome_from_a_tier_the_catalog_does_not_describe_is_counted_not_written() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    // A #1197 cross-provider fallback tier: same lane, different provider.
    client.apply_key_outcome(
        &selected(),
        TypedOutcome::RateLimited {
            retry_after_secs: Some(30),
        },
        EvidenceKind::SelfReported,
        None,
        DeploymentAttribution::EnvLane {
            lane: "extract",
            endpoint: "https://api.deepseek.com/chat/completions",
            model: "deepseek-chat",
        },
    );
    // A lane with no catalog row at all.
    client.apply_key_outcome(
        &selected(),
        TypedOutcome::RateLimited {
            retry_after_secs: Some(30),
        },
        EvidenceKind::SelfReported,
        None,
        DeploymentAttribution::EnvLane {
            lane: "embedding",
            endpoint: "https://api.voyageai.com/v1/embeddings",
            model: "voyage-4",
        },
    );

    let counts = client.deployment_health_record_counts();
    assert_eq!(counts.recorded, 0);
    assert_eq!(
        counts.skipped_different_request, 1,
        "a fallback tier's throttle must be counted, not attributed to the primary deployment"
    );
    assert_eq!(counts.skipped_unknown_deployment, 1);

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    assert!(
        get_model_deployment_health(store.connection(), &env_deployment_id("extract"))
            .expect("read")
            .is_none()
    );
    // The credential half still happened: a skipped health record must never
    // change what the existing path does.
    assert_eq!(
        store
            .vault_get_key_health(KEY_ENV, &format!("{KEY_ENV}_1"))
            .expect("read")
            .expect("row")
            .status,
        HEALTH_RATE_LIMITED
    );
}

#[test]
fn a_channel_with_no_catalog_row_records_nothing_and_counts_nothing() {
    // The MCP/CLI `record-key-result` channel and the rerank lane: no
    // deployment was named, so this is not a skip to investigate — it is a
    // path that never had one.
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    client.record_provider_key_result_blocking(
        KEY_ENV,
        &format!("{KEY_ENV}_1"),
        Some(429),
        None,
        Some(30),
        None,
    );

    assert_eq!(
        client.deployment_health_record_counts(),
        crate::DeploymentHealthRecordCounts::default()
    );
}

#[test]
fn a_success_after_a_throttle_clears_the_deployment_cooldown() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    client.apply_key_outcome(
        &selected(),
        TypedOutcome::RateLimited {
            retry_after_secs: Some(30),
        },
        EvidenceKind::SelfReported,
        None,
        extract_attribution(),
    );
    client.apply_key_outcome(
        &selected(),
        TypedOutcome::Success,
        EvidenceKind::SelfReported,
        None,
        extract_attribution(),
    );

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    let deployment = get_model_deployment_health(store.connection(), &env_deployment_id("extract"))
        .expect("read")
        .expect("row");
    assert_eq!(deployment.state, "ok");
    assert_eq!(deployment.cooldown_until, None);
    assert_eq!(client.deployment_health_record_counts().recorded, 2);
}

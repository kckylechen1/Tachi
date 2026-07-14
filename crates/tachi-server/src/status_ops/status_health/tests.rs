use super::*;
use crate::test_support::EnvRestore;
use serde_json::json;

fn api_key_row<'a>(rows: &'a [ApiKeyStatus], name: &str) -> &'a ApiKeyStatus {
    rows.iter()
        .find(|row| row.name == name)
        .expect("api key row should exist")
}

#[test]
fn vault_plaintext_duplicate_with_same_value_is_not_drift() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let _voyage = EnvRestore::set("VOYAGE_API_KEY", "same-secret");

    let rows = collect_api_key_status_from_sources(
        HashSet::from(["VOYAGE_API_KEY".to_string()]),
        HashMap::from([("VOYAGE_API_KEY".to_string(), "same-secret".to_string())]),
        HashMap::new(),
        HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    let voyage = api_key_row(&rows, "VOYAGE_API_KEY");

    assert_eq!(voyage.status, "configured");
    assert_eq!(voyage.source, "vault+env(same)");
    assert!(voyage
        .drift_warning
        .as_deref()
        .is_some_and(|warning| warning.starts_with("redundant:")));
}

#[test]
fn vault_plaintext_duplicate_with_different_value_is_drift() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let _voyage = EnvRestore::set("VOYAGE_API_KEY", "env-secret");

    let rows = collect_api_key_status_from_sources(
        HashSet::from(["VOYAGE_API_KEY".to_string()]),
        HashMap::from([("VOYAGE_API_KEY".to_string(), "vault-secret".to_string())]),
        HashMap::new(),
        HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    let voyage = api_key_row(&rows, "VOYAGE_API_KEY");

    assert_eq!(voyage.status, "drift");
    assert_eq!(voyage.source, "vault+env");
    assert!(voyage
        .drift_warning
        .as_deref()
        .is_some_and(|warning| warning.starts_with("drift:")));
}

#[test]
fn vault_plaintext_duplicate_without_decrypted_value_is_unverified_not_drift() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let _voyage = EnvRestore::set("VOYAGE_API_KEY", "env-secret");

    let rows = collect_api_key_status_from_sources(
        HashSet::from(["VOYAGE_API_KEY".to_string()]),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    let voyage = api_key_row(&rows, "VOYAGE_API_KEY");

    assert_eq!(voyage.status, "configured");
    assert_eq!(voyage.source, "vault+env(unverified)");
    assert!(voyage
        .drift_warning
        .as_deref()
        .is_some_and(|warning| warning.starts_with("duplicate-unverified:")));
}

#[test]
fn deprecated_configured_key_reports_canonical_cleanup_hint() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let _reasoning = EnvRestore::set("REASONING_API_KEY", "legacy-secret");

    let rows = collect_api_key_status_from_sources(
        HashSet::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    let reasoning = api_key_row(&rows, "REASONING_API_KEY");

    assert!(reasoning.deprecated);
    assert_eq!(reasoning.canonical_name, "DEEPSEEK_API_KEY");
    assert_eq!(reasoning.status, "configured");
    assert!(reasoning
        .cleanup_hint
        .as_deref()
        .is_some_and(|hint| hint.contains("migrate this secret to DEEPSEEK_API_KEY")));
}

#[test]
fn deprecated_unset_key_has_no_cleanup_hint() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let _reasoning = EnvRestore::remove("REASONING_API_KEY");

    let rows = collect_api_key_status_from_sources(
        HashSet::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    let reasoning = api_key_row(&rows, "REASONING_API_KEY");

    assert!(reasoning.deprecated);
    assert_eq!(reasoning.status, "deprecated-unset");
    assert!(reasoning.cleanup_hint.is_none());
}

#[test]
fn rotation_members_configure_their_logical_provider_key() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let _voyage = EnvRestore::remove("VOYAGE_API_KEY");

    let rows = collect_api_key_status_from_sources(
        HashSet::from([
            "VOYAGE_API_KEY_1".to_string(),
            "VOYAGE_API_KEY_2".to_string(),
        ]),
        HashMap::new(),
        HashMap::new(),
        HashMap::from([(
            "VOYAGE_API_KEY".to_string(),
            RotationSourceStatus {
                total_keys: 2,
                current_index: 1,
                strategy: "round_robin".to_string(),
                members: vec![
                    "VOYAGE_API_KEY_1".to_string(),
                    "VOYAGE_API_KEY_2".to_string(),
                ],
            },
        )]),
        &HashMap::new(),
        &HashMap::new(),
    );
    let voyage = api_key_row(&rows, "VOYAGE_API_KEY");

    assert_eq!(voyage.status, "configured");
    assert_eq!(voyage.source, "vault");
    assert_eq!(
        voyage.rotation.as_ref().map(|rotation| rotation.total_keys),
        Some(2)
    );
    assert_eq!(
        voyage
            .rotation
            .as_ref()
            .map(|rotation| rotation.configured_keys),
        Some(2)
    );
    assert_eq!(
        voyage
            .rotation
            .as_ref()
            .and_then(|rotation| rotation.healthy_keys),
        None
    );
    assert_eq!(
        voyage
            .rotation
            .as_ref()
            .map(|rotation| {
                rotation
                    .members
                    .iter()
                    .map(|member| member.name.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        vec!["VOYAGE_API_KEY_1", "VOYAGE_API_KEY_2"]
    );

    let rotation_sources = HashMap::from([(
        "VOYAGE_API_KEY".to_string(),
        RotationSourceStatus {
            total_keys: 2,
            current_index: 1,
            strategy: "round_robin".to_string(),
            members: vec![
                "VOYAGE_API_KEY_1".to_string(),
                "VOYAGE_API_KEY_2".to_string(),
            ],
        },
    )]);
    let probed = ProviderRotationGroupProbe {
        logical_name: "VOYAGE_API_KEY".to_string(),
        total_keys: 2,
        configured_keys: 2,
        healthy_keys: 1,
        rate_limited_keys: 1,
        auth_failed_keys: 0,
        current_index: 1,
        strategy: "round_robin".to_string(),
        next_retry_at: None,
        keys: vec![
            ApiKeyRotationMemberStatus {
                name: "VOYAGE_API_KEY_1".to_string(),
                status: "ok".to_string(),
                message: Some("1024 dims".to_string()),
                last_probe_at: Some("2026-06-08T00:00:00Z".to_string()),
            },
            ApiKeyRotationMemberStatus {
                name: "VOYAGE_API_KEY_2".to_string(),
                status: "rate_limited".to_string(),
                message: Some("429".to_string()),
                last_probe_at: Some("2026-06-08T00:00:00Z".to_string()),
            },
        ],
    };
    let rotation_probes = HashMap::from([("VOYAGE_API_KEY".to_string(), &probed)]);
    let probed_rows = collect_api_key_status_from_sources(
        HashSet::from([
            "VOYAGE_API_KEY_1".to_string(),
            "VOYAGE_API_KEY_2".to_string(),
        ]),
        HashMap::new(),
        HashMap::new(),
        rotation_sources,
        &HashMap::new(),
        &rotation_probes,
    );
    let probed_voyage = api_key_row(&probed_rows, "VOYAGE_API_KEY");
    let rotation = probed_voyage.rotation.as_ref().expect("rotation");
    assert_eq!(rotation.healthy_keys, Some(1));
    assert_eq!(rotation.rate_limited_keys, 1);
    assert_eq!(rotation.members[1].status, "rate_limited");
}

#[test]
fn provider_probe_cache_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let global_db = dir.path().join("global").join("memory.db");
    let other_global_db = dir.path().join("other").join("memory.db");
    let cache = write_provider_probe_cache_report(
        dir.path(),
        &global_db,
        ProviderProbeReport {
            probes: vec![ProviderProbeResult {
                name: "voyage_embed".to_string(),
                status: "ok".to_string(),
                message: Some("1024 dims".to_string()),
            }],
            rotation_groups: vec![ProviderRotationGroupProbe {
                logical_name: "VOYAGE_API_KEY".to_string(),
                total_keys: 2,
                configured_keys: 2,
                healthy_keys: 1,
                rate_limited_keys: 1,
                auth_failed_keys: 0,
                current_index: 1,
                strategy: "round_robin".to_string(),
                next_retry_at: None,
                keys: vec![
                    ApiKeyRotationMemberStatus {
                        name: "VOYAGE_API_KEY_1".to_string(),
                        status: "ok".to_string(),
                        message: Some("1024 dims".to_string()),
                        last_probe_at: Some("2026-06-08T00:00:00Z".to_string()),
                    },
                    ApiKeyRotationMemberStatus {
                        name: "VOYAGE_API_KEY_2".to_string(),
                        status: "rate_limited".to_string(),
                        message: Some("429".to_string()),
                        last_probe_at: Some("2026-06-08T00:00:00Z".to_string()),
                    },
                ],
            }],
        },
    )
    .expect("write cache");

    assert!(
        read_provider_probe_cache(dir.path(), &other_global_db).is_none(),
        "probe cache must be scoped by global DB"
    );
    let loaded = read_provider_probe_cache(dir.path(), &global_db).expect("read cache");
    assert_eq!(loaded.last_probe_at, cache.last_probe_at);
    assert_eq!(loaded.ttl_seconds, 24 * 60 * 60);
    assert!(!loaded.is_stale());
    assert_eq!(loaded.probes.len(), 1);
    assert_eq!(loaded.probes[0].status, "ok");
    assert_eq!(loaded.rotation_groups.len(), 1);
    assert_eq!(loaded.rotation_groups[0].logical_name, "VOYAGE_API_KEY");
    assert_eq!(loaded.rotation_groups[0].healthy_keys, 1);
    assert_eq!(loaded.rotation_groups[0].rate_limited_keys, 1);
    assert_eq!(loaded.rotation_groups[0].keys[1].status, "rate_limited");
}

#[test]
fn provider_probe_client_loads_target_db_key_health() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("global.db");
    const KEY: &str = "TACHI_TEST_ONLY_API_KEY_PROBE_HEALTH";

    let writer = tachi_llm::LlmClient::new_with_vault_db(Some(&db_path))
        .expect("writer client should initialize");
    writer.record_provider_key_result_blocking(
        KEY,
        KEY,
        Some(401),
        None,
        None,
        Some("forced auth failure"),
    );

    let probe = super::probes::probe_llm_client_for_tests(&db_path)
        .expect("probe client should initialize");
    probe.set_provider_secret_pool(
        KEY,
        vec![tachi_llm::ProviderSecret {
            key_id: KEY.to_string(),
            value: "secret".to_string(),
        }],
    );

    assert_eq!(
        probe.provider_key_id_for_tests(&[KEY]),
        None,
        "provider probes must honor fresh auth_failed health from the target global DB"
    );
}

#[test]
fn model_lanes_reports_default_voyage_rerank_provider() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider = EnvRestore::set(tachi_llm::RERANK_PROVIDER_ENV, "");
    // Empty string is treated as unset by RerankProviderKind::parse → voyage.
    // Also clear any leftover local endpoint so the lane shape stays default.
    let _endpoint = EnvRestore::remove(tachi_llm::RERANK_LOCAL_ENDPOINT_ENV);
    // EnvRestore with empty string still sets the var; remove for true default.
    std::env::remove_var(tachi_llm::RERANK_PROVIDER_ENV);

    let lanes = model_lanes_json();
    assert_eq!(lanes["rerank"]["provider"], json!("voyage"));
    assert_eq!(lanes["rerank"]["model"], json!("rerank-2.5"));
    assert_eq!(
        lanes["recall_rerank_cache"]["rerank_provider"],
        json!("voyage")
    );
}

#[test]
fn model_lanes_reports_configured_local_rerank_provider() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _provider = EnvRestore::set(tachi_llm::RERANK_PROVIDER_ENV, "local");
    let _endpoint = EnvRestore::set(
        tachi_llm::RERANK_LOCAL_ENDPOINT_ENV,
        "http://127.0.0.1:9/rerank",
    );

    let lanes = model_lanes_json();
    assert_eq!(lanes["rerank"]["provider"], json!("local"));
    assert_eq!(
        lanes["rerank"]["local_endpoint"],
        json!("http://127.0.0.1:9/rerank")
    );
    assert_eq!(
        lanes["recall_rerank_cache"]["rerank_provider"],
        json!("local")
    );
    assert!(lanes["rerank"]["model"].is_null());
}

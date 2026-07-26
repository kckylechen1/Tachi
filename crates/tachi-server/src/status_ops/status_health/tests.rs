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

// tachi#1287 fix 1: the health-probe seam (`probes::run_provider_probe_report`)
// used to discard `MaterializeReport.skipped_aliases` on the `Ok` branch — a
// skip was only visible in tracing output, never in the probe report itself
// (which is what `status doctor`/health snapshots actually read). Assert the
// conversion helper surfaces it as a queryable `ProviderProbeResult`, not just
// a log line.
#[test]
fn skipped_alias_surfaces_as_a_probe_result_not_only_a_log_line() {
    let report = tachi_llm::MaterializeReport {
        skipped_aliases: vec![(
            "OPENAI_API_KEY".to_string(),
            "Config key 'OPENAI_API_KEY' references Vault alias 'MISSING_ALIAS' but the secret is missing or Vault is locked.".to_string(),
        )],
        ..Default::default()
    };

    let probe = super::probes::skipped_alias_probe_result(&report)
        .expect("a non-empty skipped_aliases must surface as a probe result");

    assert_eq!(probe.name, "provider_secret_materialization");
    assert_eq!(probe.status, "degraded");
    let message = probe.message.expect("probe message present");
    assert!(message.contains("OPENAI_API_KEY") || message.contains("MISSING_ALIAS"));
    assert!(message.contains("skipped during materialization"));
}

#[test]
fn no_skipped_aliases_yields_no_probe_result() {
    let report = tachi_llm::MaterializeReport::default();
    assert!(super::probes::skipped_alias_probe_result(&report).is_none());
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

#[test]
fn model_lanes_distinguish_api_only_distill_from_cli_first_reasoning() {
    let lanes = model_lanes_json();

    assert_eq!(
        lanes["distill"]["provider"],
        json!(
            "openai-compatible API only; FOUNDRY_DISTILL_BACKEND=claude_cli is a legacy selector (no Claude subprocess)"
        )
    );
    assert_eq!(
        lanes["reasoning"]["provider"],
        json!("claude-cli-first, openai-compatible fallback")
    );
}

#[test]
fn xai_and_zai_are_recognized_provider_env_names() {
    // #1355: the grok/xai opencode lane provider uses `{env:XAI_API_KEY}`
    // substitution in opencode.json so vault "收权" can never blank it (no
    // literal on disk). That only works end-to-end if this name is in the
    // provider-key filter that gates which unlocked-vault secrets get
    // injected into the lane subprocess env
    // (`load_unlocked_provider_env_secrets` -> `provider_api_key_env_names`).
    let names = provider_api_key_env_names();
    assert!(
        names.contains("XAI_API_KEY"),
        "XAI_API_KEY must be a recognized provider env name so an unlocked-vault \
         xAI secret materializes into the grok lane child env"
    );
    // Alternate ecosystem name is admitted too (whichever the owner stored).
    assert!(
        names.contains("GROK_API_KEY"),
        "GROK_API_KEY alias must be admitted by the provider-key filter"
    );
    // Zhipu/BigModel family already covered before #1355.
    assert!(names.contains("ZAI_API_KEY"));
    assert!(names.contains("BIGMODEL_API_KEY"));
}

#[test]
fn zhipuai_is_a_recognized_provider_env_name() {
    // #1355(b): the opencode `zhipuai-coding-plan` GLM lane provider uses
    // `{env:ZHIPUAI_API_KEY}` substitution in opencode.json. `ZAI_API_KEY`
    // (asserted above) does NOT cover this — the vault stores a distinct
    // `ZHIPUAI_API_KEY` secret (verified by SHA match against the working
    // opencode literal at `zhipuai-coding-plan.options.apiKey`) holding a
    // different value than `ZAI_API_KEY`. Without `ZHIPUAI_API_KEY` in the
    // provider-key filter, the GLM lane subprocess never receives its
    // vault secret and `{env:ZHIPUAI_API_KEY}` resolves to nothing.
    let names = provider_api_key_env_names();
    assert!(
        names.contains("ZHIPUAI_API_KEY"),
        "ZHIPUAI_API_KEY must be a recognized provider env name so an unlocked-vault \
         zhipuai secret materializes into the opencode GLM lane child env"
    );
}

#[test]
fn kimi_is_a_recognized_provider_env_name() {
    // #1355 follow-up: the `kimi-for-coding`/K3 opencode lane provider uses
    // `{env:KIMI_API_KEY}` substitution (or direct env read) so vault
    // "收权" can never blank it (no literal on disk). That only works
    // end-to-end if this name is in the provider-key filter that gates
    // which unlocked-vault secrets get injected into the lane subprocess
    // env (`load_unlocked_provider_env_secrets` -> `provider_api_key_env_names`).
    let names = provider_api_key_env_names();
    assert!(
        names.contains("KIMI_API_KEY"),
        "KIMI_API_KEY must be a recognized provider env name so an unlocked-vault \
         Kimi secret materializes into the kimi-for-coding lane child env"
    );
    // Alternate ecosystem name is admitted too (whichever the owner stored;
    // Moonshot AI is Kimi's vendor).
    assert!(
        names.contains("MOONSHOT_API_KEY"),
        "MOONSHOT_API_KEY alias must be admitted by the provider-key filter"
    );
}

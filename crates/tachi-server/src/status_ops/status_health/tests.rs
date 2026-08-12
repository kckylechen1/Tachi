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

    let _minimax = EnvRestore::set("MINIMAX_API_KEY", "legacy-secret");

    let rows = collect_api_key_status_from_sources(
        HashSet::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    let minimax = api_key_row(&rows, "MINIMAX_API_KEY");

    assert!(minimax.deprecated);
    assert_eq!(minimax.canonical_name, "DEEPSEEK_API_KEY");
    assert_eq!(minimax.status, "configured");
    assert!(minimax
        .cleanup_hint
        .as_deref()
        .is_some_and(|hint| hint.contains("migrate this secret to DEEPSEEK_API_KEY")));
}

#[test]
fn live_lane_keys_are_not_deprecated_or_remove_migrate_targets() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let _reasoning = EnvRestore::set("REASONING_API_KEY", "reasoning-secret");
    let _distill = EnvRestore::set("DISTILL_API_KEY", "distill-secret");

    let rows = collect_api_key_status_from_sources(
        HashSet::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );

    for key in ["REASONING_API_KEY", "DISTILL_API_KEY"] {
        let row = api_key_row(&rows, key);
        assert!(
            !row.deprecated,
            "live resolver input {key} is not deprecated"
        );
        assert_eq!(row.canonical_name, key);
        assert_eq!(row.status, "configured");
        let hint = row.cleanup_hint.as_deref().unwrap_or_default();
        for forbidden in ["deprecated", "migrate", "remove"] {
            assert!(
                !hint.contains(forbidden),
                "live resolver input {key} must not emit {forbidden:?} guidance: {hint}"
            );
        }
    }

    let reasoning = api_key_row(&rows, "REASONING_API_KEY");
    assert!(reasoning
        .cleanup_hint
        .as_deref()
        .is_some_and(|hint| hint.contains("accepted aliases/fallbacks")));
}

#[test]
fn unset_live_lane_keys_are_missing_not_deprecated() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let _reasoning = EnvRestore::remove("REASONING_API_KEY");
    let _distill = EnvRestore::remove("DISTILL_API_KEY");
    let _zai = EnvRestore::remove("ZAI_API_KEY");
    let _bigmodel = EnvRestore::remove("BIGMODEL_API_KEY");

    let rows = collect_api_key_status_from_sources(
        HashSet::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    );

    for key in ["REASONING_API_KEY", "DISTILL_API_KEY"] {
        let row = api_key_row(&rows, key);
        assert!(
            !row.deprecated,
            "live resolver input {key} is not deprecated"
        );
        assert_eq!(row.status, "missing");
        let hint = row.cleanup_hint.as_deref().unwrap_or_default();
        assert!(!hint.contains("deprecated"));
        assert!(!hint.contains("migrate"));
        assert!(!hint.contains("remove"));
    }
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
    assert!(message.contains("OPENAI_API_KEY"));
    assert!(message.contains("no last-known-good provider pool retained"));
    assert!(!message.contains("MISSING_ALIAS"));
    assert!(message.contains("skipped during materialization"));
}

#[test]
fn skipped_alias_probe_aggregate_distinguishes_retained_without_raw_reasons() {
    let report = tachi_llm::MaterializeReport {
        skipped_aliases: vec![
            (
                "VOYAGE_API_KEY".to_string(),
                "RAW_ALIAS_TARGET_ONE VALUE_SENTINEL_ONE".to_string(),
            ),
            (
                "OPENAI_API_KEY".to_string(),
                "RAW_ALIAS_TARGET_TWO VALUE_SENTINEL_TWO".to_string(),
            ),
        ],
        retained_from_last_known_good: vec!["VOYAGE_API_KEY".to_string()],
        ..Default::default()
    };

    let probe = super::probes::skipped_alias_probe_result(&report)
        .expect("skipped aliases must produce a degraded aggregate");
    let message = probe.message.expect("probe message present");

    assert!(message.contains("VOYAGE_API_KEY: retained last-known-good provider pool"));
    assert!(message.contains("OPENAI_API_KEY: no last-known-good provider pool retained"));
    for sentinel in [
        "RAW_ALIAS_TARGET_ONE",
        "VALUE_SENTINEL_ONE",
        "RAW_ALIAS_TARGET_TWO",
        "VALUE_SENTINEL_TWO",
    ] {
        assert!(
            !message.contains(sentinel),
            "aggregate leaked {sentinel}: {message}"
        );
    }
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
    // (`load_unlocked_provider_env_secrets` -> `admitted_env_secret_names`,
    // #1680/D3).
    let names = admitted_env_secret_names();
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
    let names = admitted_env_secret_names();
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
    // env (`load_unlocked_provider_env_secrets` -> `admitted_env_secret_names`,
    // #1680/D3).
    let names = admitted_env_secret_names();
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

/// #1680/D3 env-injection regression, codex NEEDS-FIXES BUG-3: the legacy
/// comparison set below is a **hardcoded literal**, copied by hand from
/// `git show 6800f09d:crates/tachi-server/src/status_ops/status_health/api_keys.rs`
/// (the frozen base commit this whole PR branches from) — every primary key
/// and every alias in that pre-#1680 `API_KEY_DEFS`, flattened. It is
/// deliberately NOT derived from `HEAD`'s `API_KEY_DEFS` (that would let a
/// regression prove itself correct by re-deriving its own expected answer
/// from the same registry it just changed).
///
/// `admitted_env_secret_names()` (the surface that gates lane env injection,
/// providers-doctor admission, and the plaintext secret scanner) is asserted
/// to equal that frozen legacy set **plus exactly one new name**:
/// `GOOGLE_SEARCH_API_KEY`. That growth is an intended, explicit consequence
/// of this PR (#1680/D3: `GOOGLE_SEARCH_API_KEY` becomes its own independent
/// SearchApi registry entry instead of being unreachable outside intake's
/// old alias table) — not an accidental widening. If lane env injection ever
/// admits anything beyond that one explicit addition, this test goes red.
#[test]
fn admitted_env_secret_names_matches_frozen_base_legacy_set_plus_google_search() {
    let legacy_set_at_6800f09d: HashSet<String> = [
        "VOYAGE_API_KEY",
        "VOYAGE_RERANK_API_KEY",
        "SILICONFLOW_API_KEY",
        "EXTRACT_API_KEY",
        "SUMMARY_API_KEY",
        "DEEPSEEK_API_KEY",
        "DISTILL_API_KEY",
        "REASONING_API_KEY",
        "ZAI_API_KEY",
        "BIGMODEL_API_KEY",
        "XAI_API_KEY",
        "GROK_API_KEY",
        "ZHIPUAI_API_KEY",
        "KIMI_API_KEY",
        "MOONSHOT_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
        "EXA_API_KEY",
        "TAVILY_API_KEY",
        "MINIMAX_API_KEY",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        legacy_set_at_6800f09d.len(),
        22,
        "the hardcoded legacy literal itself must have 22 unique names — a \
         miscount here would silently weaken this discriminator"
    );

    let mut expected_admitted_set = legacy_set_at_6800f09d.clone();
    expected_admitted_set.insert("GOOGLE_SEARCH_API_KEY".to_string());

    assert_eq!(
        admitted_env_secret_names(),
        expected_admitted_set,
        "env-injection admitted set must grow by exactly GOOGLE_SEARCH_API_KEY \
         over the frozen pre-#1680 base — no other name may appear or vanish"
    );

    // And the narrowed materialization view must be a strict subset that
    // drops at least the SearchApi names — never equal to the admitted set,
    // or the split accomplished nothing.
    assert!(model_provider_env_names().is_subset(&admitted_env_secret_names()));
    assert!(model_provider_env_names().len() < admitted_env_secret_names().len());
}

/// #1680 D2/D3: the fingerprint domain input must be unambiguous.
///
/// `fp1` is keyed by `provider_kind`, so if one env-var name could resolve to
/// two different kinds — say `BIGMODEL_API_KEY` reading as `zai` through one
/// registry row and as something else through another — the same secret would
/// fingerprint two ways depending on which row a caller happened to match, and
/// accounts would split or merge by declaration order. Every admitted name is
/// checked against every row it appears in, as a primary key or as an alias.
#[test]
fn provider_kind_assignment_is_unambiguous_across_the_registry() {
    for name in admitted_env_secret_names() {
        let kinds: HashSet<&'static str> = API_KEY_DEFS
            .iter()
            .filter(|def| def.key == name || def.aliases.contains(&name.as_str()))
            .map(|def| def.provider_kind)
            .collect();
        assert_eq!(
            kinds.len(),
            1,
            "env name {name} resolves to {kinds:?}; a name may belong to exactly one provider \
             family or #1680's key fingerprints become order-dependent"
        );
        assert_eq!(
            provider_kind_for_env_name(&name),
            kinds.into_iter().next(),
            "the derived view must agree with the registry rows it reads"
        );
    }
}

/// The alias half, spelled out on the case that motivates it: the two names
/// that hold one DeepSeek account's key resolve to one family, and an unknown
/// name resolves to nothing rather than to a guess.
#[test]
fn provider_kind_resolves_aliases_and_refuses_unknown_names() {
    assert_eq!(
        provider_kind_for_env_name("DEEPSEEK_API_KEY"),
        Some("deepseek")
    );
    assert_eq!(
        provider_kind_for_env_name("DISTILL_API_KEY"),
        Some("deepseek")
    );
    assert_eq!(provider_kind_for_env_name("GEMINI_API_KEY"), Some("google"));
    assert_eq!(provider_kind_for_env_name("MOONSHOT_API_KEY"), Some("kimi"));
    assert_eq!(provider_kind_for_env_name("NOT_A_PROVIDER_KEY"), None);
    assert_eq!(provider_kind_for_env_name(""), None);
}

/// The discrimination-2 boundary restated in the fingerprint domain: search
/// credentials must not share a provider family with the model accounts they
/// were historically folded into (`intake::alias_family` put
/// `GOOGLE_SEARCH_API_KEY` in the google/gemini family). Same family would mean
/// same fingerprint domain, which would put a search key one merge away from a
/// model account.
#[test]
fn search_keys_get_their_own_provider_kinds() {
    assert_eq!(
        provider_kind_for_env_name("GOOGLE_SEARCH_API_KEY"),
        Some("google-search")
    );
    assert_ne!(
        provider_kind_for_env_name("GOOGLE_SEARCH_API_KEY"),
        provider_kind_for_env_name("GOOGLE_API_KEY")
    );
    assert_eq!(provider_kind_for_env_name("EXA_API_KEY"), Some("exa"));
    assert_eq!(provider_kind_for_env_name("TAVILY_API_KEY"), Some("tavily"));
}

/// #1680 D6: there is one probe-target table, and it is `tachi_llm`'s. The
/// registry's job is to say *which env-var names* may be probed and as which
/// family; the hosts and endpoints themselves stay compile-time constants in
/// the module that dials them, because that is the anti-SSRF boundary. This
/// pins the two halves against the drift the duplicated table used to invite.
#[test]
fn registry_probe_targets_match_the_probe_table() {
    for def in API_KEY_DEFS {
        let Some(descriptor) = def.probe else {
            continue;
        };
        assert_eq!(
            descriptor.provider_kind, def.provider_kind,
            "{} points at a {} probe target",
            def.key, descriptor.provider_kind
        );
        assert!(
            tachi_llm::AUTH_PROBE_DESCRIPTORS.contains(descriptor),
            "{} points at a descriptor outside the probe table",
            def.key
        );
        assert_eq!(
            tachi_llm::auth_probe_descriptor_for_host(descriptor.host),
            Some(descriptor),
            "{} names a host the probe table does not admit",
            def.key
        );
    }

    // The other direction: a host the probe table is willing to dial that no
    // registry entry can reach would be an unreachable exception to the
    // admission boundary.
    for descriptor in tachi_llm::AUTH_PROBE_DESCRIPTORS {
        assert!(
            API_KEY_DEFS
                .iter()
                .any(|def| def.provider_kind == descriptor.provider_kind),
            "probe table host {} belongs to no registry family",
            descriptor.host
        );
    }
}

/// The probe surface is per env-var name, alias names included, and it is
/// closed: a name the registry does not recognize, or a family with no
/// documented non-generating endpoint, is never probeable.
#[test]
fn auth_probe_targets_resolve_by_env_name_and_refuse_everything_else() {
    assert_eq!(
        auth_probe_descriptor_for_env_name("DEEPSEEK_API_KEY").and_then(|probe| probe.endpoint),
        Some("https://api.deepseek.com/models")
    );
    // An alias is another name for the same account, so it probes the same
    // family.
    assert_eq!(
        auth_probe_descriptor_for_env_name("EXTRACT_API_KEY"),
        auth_probe_descriptor_for_env_name("SILICONFLOW_API_KEY")
    );
    assert_eq!(
        auth_probe_descriptor_for_env_name("SILICONFLOW_API_KEY").map(|probe| probe.provider_kind),
        Some("siliconflow")
    );
    // Recognized family, no documented probe: probeable-by-name, never dialed.
    assert_eq!(
        auth_probe_descriptor_for_env_name("ZAI_API_KEY").map(|probe| probe.endpoint),
        Some(None)
    );
    // Search credentials and unknown names have no probe target at all.
    assert_eq!(auth_probe_descriptor_for_env_name("EXA_API_KEY"), None);
    assert_eq!(auth_probe_descriptor_for_env_name("TAVILY_API_KEY"), None);
    assert_eq!(
        auth_probe_descriptor_for_env_name("NOT_A_PROVIDER_KEY"),
        None
    );
    assert_eq!(auth_probe_descriptor_for_env_name(""), None);
}

// ─── model lanes are a projection, not a mirror (tachi#1681 D7 PR-B, item 3) ──

/// The mirror-is-dead discriminator. A hand-maintained JSON copy would keep
/// reporting the compiled-in default no matter what the env chain resolved;
/// the projection cannot.
#[test]
fn model_lanes_report_the_resolved_extract_lane_not_a_hardcoded_literal() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _model = EnvRestore::set("EXTRACT_MODEL", "Qwen/Qwen3.5-480B-status-projection");
    let _base = EnvRestore::set("EXTRACT_BASE_URL", "https://status-projection.test/v1/chat");

    let lanes = model_lanes_json();

    assert_eq!(
        lanes["extract"]["model"],
        json!("Qwen/Qwen3.5-480B-status-projection"),
        "status must report the model the env chain actually resolved"
    );
    assert_eq!(
        lanes["extract"]["endpoint"],
        json!("https://status-projection.test/v1/chat")
    );
    assert_eq!(
        lanes["extract"]["catalog_source"],
        json!("env"),
        "and say where that came from"
    );
    assert_eq!(lanes["extract"]["deployment_id"], json!("env:extract"));
    assert_eq!(
        lanes["extract"]["keys"],
        json!(["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"]),
        "the key precedence chain keeps the shape its consumers read"
    );
}

/// Status and catalog are one derivation, so they cannot disagree. Asserted
/// against rows actually written to a store, not against the projection twice.
#[test]
fn model_lane_status_equals_the_catalog_rows_the_same_config_imports() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let config = tachi_llm::ProviderRuntimeConfig::from_env().expect("lanes resolve");
    let embedding = tachi_llm::EmbeddingConfig::from_env().expect("embedding config resolves");
    let observed_at = memcore::db::now_utc_iso();
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
    memcore::db::init_schema(&conn).expect("schema");
    tachi_llm::import_env_chat_lanes(&conn, &config, &observed_at).expect("import succeeds");
    // The embedding lane is imported into the same store and compared the same
    // way. Comparing only the four chat lanes left the embedding half of the
    // projection — the one carrying the dimension declaration the #1681 D3
    // escape hatch turns on — free to drift out of the status surface unseen.
    tachi_llm::import_env_embedding_lane(
        &conn,
        &embedding,
        &tachi_llm::voyage_embeddings_endpoint(),
        &observed_at,
    )
    .expect("embedding import succeeds");

    let stored = memcore::db::model_catalog::list_model_deployments_by_source(
        &conn,
        memcore::catalog::CatalogSource::Env,
    )
    .expect("rows read");
    assert_eq!(stored.len(), 5, "four chat lanes plus the embedding lane");

    let lanes = model_lanes_json();
    for lane in ["extract", "summary", "reasoning", "distill"] {
        let row = stored
            .iter()
            .find(|row| row.deployment_id == format!("env:{lane}"))
            .unwrap_or_else(|| panic!("catalog is missing lane {lane}"));

        assert_eq!(
            lanes[lane]["model"],
            json!(row.provider_model_id),
            "lane {lane}: status and catalog must report the same model"
        );
        assert_eq!(
            lanes[lane]["endpoint"],
            json!(row.endpoint_ref),
            "lane {lane}: status and catalog must report the same endpoint"
        );
        assert_eq!(
            lanes[lane]["deployment_id"],
            json!(row.deployment_id),
            "lane {lane}: status must name the catalog row it is projecting"
        );
        assert_eq!(
            lanes[lane]["provider_account_ref"],
            json!(row.provider_account_id),
            "lane {lane}: status and catalog must agree on the account handle"
        );
    }

    let embedding_row = stored
        .iter()
        .find(|row| row.deployment_id == "env:embedding")
        .expect("catalog is missing the embedding lane");
    assert_eq!(
        lanes["embedding"]["model"],
        json!(embedding_row.provider_model_id),
        "the embedding lane's model must be the one the catalog recorded"
    );
    assert_eq!(
        lanes["embedding"]["endpoint"],
        json!(embedding_row.endpoint_ref),
        "and the endpoint a request actually uses"
    );
    assert_eq!(
        lanes["embedding"]["deployment_id"],
        json!(embedding_row.deployment_id)
    );
    assert_eq!(
        lanes["embedding"]["provider_account_ref"],
        json!(embedding_row.provider_account_id)
    );
    assert_eq!(
        lanes["embedding"]["catalog_source"],
        json!(embedding_row.catalog_source.as_str())
    );
    assert_eq!(
        lanes["embedding"]["expected_dimension"],
        json!(
            embedding_row
                .capabilities
                .embeddings
                .as_ref()
                .expect("the embedding row declares a dimension")
                .dimension
        ),
        "the width status reports and the width the catalog declares are one value"
    );
}

#[test]
fn model_lanes_report_a_deliberate_embedding_swap_as_an_override() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _model = EnvRestore::set(tachi_llm::EMBEDDING_MODEL_ENV, "voyage-3-large");
    let stored_width = tachi_llm::STORED_INDEX_DIMENSION.to_string();
    let _dimension = EnvRestore::set(tachi_llm::EMBEDDING_DIMENSION_ENV, &stored_width);

    let lanes = model_lanes_json();
    assert_eq!(lanes["embedding"]["model"], json!("voyage-3-large"));
    assert_eq!(lanes["embedding"]["model_source"], json!("env_override"));
    assert_eq!(
        lanes["embedding"]["expected_dimension"],
        json!(tachi_llm::STORED_INDEX_DIMENSION)
    );
    assert_eq!(
        lanes["embedding"]["deployment_id"],
        json!("env:embedding"),
        "the embedding lane is a catalog row like any other"
    );
    assert!(
        lanes["embedding"]["config_error"].is_null(),
        "a valid same-width swap is not an error"
    );
}

#[test]
fn model_lanes_report_a_refused_embedding_config_instead_of_a_plausible_model() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _model = EnvRestore::set(tachi_llm::EMBEDDING_MODEL_ENV, "some-2048-dim-model");
    let _dimension = EnvRestore::set(tachi_llm::EMBEDDING_DIMENSION_ENV, "2048");

    let lanes = model_lanes_json();
    assert!(
        lanes["embedding"]["config_error"]
            .as_str()
            .is_some_and(|err| err.contains("2048")),
        "a refused embedding configuration must surface as an error: {}",
        lanes["embedding"]
    );
    assert!(
        lanes["embedding"]["model"].is_null(),
        "status must not report a model it refused to configure — that is the mirror's habit \
         this projection exists to end"
    );
}

#[test]
fn model_lanes_report_the_embedding_default_against_the_stored_index_width() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _model = EnvRestore::remove(tachi_llm::EMBEDDING_MODEL_ENV);
    let _dimension = EnvRestore::remove(tachi_llm::EMBEDDING_DIMENSION_ENV);

    let lanes = model_lanes_json();
    assert_eq!(lanes["embedding"]["model"], json!("voyage-4"));
    assert_eq!(lanes["embedding"]["model_source"], json!("default"));
    assert_eq!(
        lanes["embedding"]["expected_dimension"], lanes["embedding"]["stored_index_dimension"],
        "an operator has to be able to see both numbers and that they agree"
    );
}

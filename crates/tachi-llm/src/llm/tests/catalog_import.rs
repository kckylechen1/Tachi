//! Env-chain → catalog import discriminators (tachi#1681 D7 PR-B, item 2).
//!
//! The one the design names: **catalog contents ≡ env-resolution results**.
//! Written so it can actually fail — a mutant that drops a lane, swaps a
//! model, forgets the key chain, or re-reads env instead of projecting the
//! config in hand is caught by name.

use memcore::catalog::{CatalogSource, ProtocolKind};
use memcore::db::model_catalog::{list_model_deployments_by_source, DeploymentWrite};
use rusqlite::Connection;

use super::EnvRestore;
use crate::llm::catalog_import::{
    env_chat_lane_deployments, env_deployment_id, import_env_chat_lanes, ENV_CHAT_LANES,
};
use crate::llm::provider_health::ChatLaneConfig;
use crate::llm::ProviderRuntimeConfig;
use crate::{RerankConfig, RerankProviderKind};

const OBSERVED_AT: &str = "2026-08-11T00:00:00.000Z";

fn catalog_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    memcore::db::init_schema(&conn).expect("schema initializes");
    conn
}

/// Four deliberately *distinct* lanes. Distinctness is the point: a mutant
/// that projects the extract lane four times, or reads the wrong field, has
/// nowhere to hide.
fn distinct_config() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url: "https://api.siliconflow.cn/v1/chat/completions".to_string(),
            model: "Qwen/Qwen3.5-27B".to_string(),
            api_key_envs: vec!["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        summary: ChatLaneConfig {
            base_url: "https://api.siliconflow.cn/v1/chat/completions".to_string(),
            model: "Qwen/Qwen3.5-7B".to_string(),
            api_key_envs: vec!["SUMMARY_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        reasoning: ChatLaneConfig {
            base_url: "https://api.deepseek.com/chat/completions".to_string(),
            model: "deepseek-reasoner".to_string(),
            api_key_envs: vec!["DEEPSEEK_API_KEY", "REASONING_API_KEY"],
        },
        distill: ChatLaneConfig {
            base_url: "https://api.deepseek.com/chat/completions".to_string(),
            model: "deepseek-chat".to_string(),
            api_key_envs: vec!["DISTILL_API_KEY", "DEEPSEEK_API_KEY"],
        },
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    }
}

fn lane_config<'a>(config: &'a ProviderRuntimeConfig, lane: &str) -> &'a ChatLaneConfig {
    match lane {
        "extract" => &config.extract,
        "summary" => &config.summary,
        "reasoning" => &config.reasoning,
        "distill" => &config.distill,
        other => panic!("unknown lane {other}"),
    }
}

// ─── the discriminator: catalog ≡ env resolution ─────────────────────────────

#[test]
fn stored_catalog_rows_equal_the_env_resolution_field_for_field() {
    let conn = catalog_conn();
    let config = distinct_config();

    let writes = import_env_chat_lanes(&conn, &config, OBSERVED_AT).expect("import succeeds");
    assert_eq!(writes.len(), 4);
    assert!(writes
        .iter()
        .all(|(_, write)| matches!(write, DeploymentWrite::Created { .. })));

    let stored = list_model_deployments_by_source(&conn, CatalogSource::Env).expect("rows read");
    assert_eq!(
        stored.len(),
        ENV_CHAT_LANES.len(),
        "every chat lane the config resolved must appear, and nothing else"
    );

    for lane in ENV_CHAT_LANES {
        let expected = lane_config(&config, lane);
        let row = stored
            .iter()
            .find(|row| row.deployment_id == env_deployment_id(lane))
            .unwrap_or_else(|| panic!("lane {lane} is missing from the catalog"));

        assert_eq!(
            row.provider_model_id, expected.model,
            "lane {lane}: catalog model must be the model the env chain resolved"
        );
        assert_eq!(
            row.endpoint_ref.as_deref(),
            Some(expected.base_url.as_str()),
            "lane {lane}: catalog endpoint must be the base_url the env chain resolved"
        );
        assert_eq!(
            row.source_refs,
            expected
                .api_key_envs
                .iter()
                .map(|name| format!("env_api_key:{name}"))
                .collect::<Vec<_>>(),
            "lane {lane}: the whole key precedence chain is provenance, not just the winner"
        );
        assert_eq!(row.catalog_source, CatalogSource::Env);
        assert_eq!(row.protocol_kind, ProtocolKind::OpenAiChatCompletions);
        assert!(row.capabilities.chat);
        assert_eq!(row.revision, 1);
        assert_eq!(
            row.expires_at, None,
            "an env row is re-resolved every process start, so it has no declared expiry"
        );
    }
}

#[test]
fn every_stored_row_matches_the_projection_by_content_digest() {
    // The field-by-field test above says what a reader needs to know; this one
    // is the version that cannot rot: a field added to `ModelDeployment`
    // tomorrow is covered without anyone remembering to extend an assertion
    // list.
    let conn = catalog_conn();
    let config = distinct_config();
    import_env_chat_lanes(&conn, &config, OBSERVED_AT).expect("import");

    let projected = env_chat_lane_deployments(&config, OBSERVED_AT);
    let stored = list_model_deployments_by_source(&conn, CatalogSource::Env).expect("rows");

    for lane in projected {
        let row = stored
            .iter()
            .find(|row| row.deployment_id == lane.deployment.deployment_id)
            .expect("projected lane is stored");
        assert_eq!(
            row.content_digest(),
            lane.deployment.content_digest(),
            "lane {}: the stored row and the projection must be the same content",
            lane.lane
        );
    }
}

#[test]
fn a_changed_env_resolution_moves_the_catalog() {
    // Without this, "catalog ≡ env" could be satisfied by a catalog that is
    // always empty and a comparison that always passes.
    let conn = catalog_conn();
    let mut config = distinct_config();
    import_env_chat_lanes(&conn, &config, OBSERVED_AT).expect("first import");

    config.extract.model = "Qwen/Qwen3.5-72B".to_string();
    let writes =
        import_env_chat_lanes(&conn, &config, "2026-08-12T00:00:00.000Z").expect("second import");

    let by_lane = |lane: &str| {
        writes
            .iter()
            .find(|(name, _)| *name == lane)
            .map(|(_, write)| write.clone())
            .expect("lane present")
    };
    assert!(
        matches!(
            by_lane("extract"),
            DeploymentWrite::Advanced { revision: 2, .. }
        ),
        "the lane whose model moved must advance"
    );
    for untouched in ["summary", "reasoning", "distill"] {
        assert_eq!(
            by_lane(untouched),
            DeploymentWrite::Unchanged { revision: 1 },
            "lane {untouched} did not move and must not have been rewritten"
        );
    }

    let stored = list_model_deployments_by_source(&conn, CatalogSource::Env).expect("rows");
    let extract = stored
        .iter()
        .find(|row| row.deployment_id == env_deployment_id("extract"))
        .expect("extract row");
    assert_eq!(extract.provider_model_id, "Qwen/Qwen3.5-72B");
}

#[test]
fn re_importing_an_unchanged_config_appends_nothing() {
    let conn = catalog_conn();
    let config = distinct_config();
    import_env_chat_lanes(&conn, &config, OBSERVED_AT).expect("first import");

    let writes = import_env_chat_lanes(&conn, &config, "2026-08-12T00:00:00.000Z")
        .expect("restart re-import");
    assert!(
        writes
            .iter()
            .all(|(_, write)| matches!(write, DeploymentWrite::Unchanged { revision: 1 })),
        "a process restart that resolves the same chains is not a catalog change: {writes:?}"
    );

    let events =
        memcore::db::model_catalog::list_all_model_deployment_events(&conn).expect("events read");
    assert_eq!(
        events.len(),
        4,
        "one import event per lane and nothing more — otherwise every daemon restart writes \
         four rows of audit noise forever"
    );
}

// ─── the import projects the config it is given, it does not re-read env ─────

#[test]
fn the_projection_ignores_the_ambient_environment() {
    // If this module re-resolved env instead of projecting the config the
    // client is actually running on, "catalog ≡ env" would be a statement
    // about two calls to the same env reader, and a client running on an
    // injected config (every `new_with_config` caller) would get a catalog
    // describing somebody else's process.
    //
    // `EXTRACT_MODEL` / `EXTRACT_BASE_URL` are shared with the lane-resolution
    // tests rather than uniquified, because the whole point is to set the
    // names the real chain reads — so this takes the process-wide env lock,
    // the same one `ProviderRuntimeConfig::from_env` takes under `cfg(test)`.
    let _env_lock = crate::test_support::global_test_lock().lock();
    let _extract_model = EnvRestore::set("EXTRACT_MODEL", "__catalog-import-must-not-see-this");
    let _extract_base = EnvRestore::set("EXTRACT_BASE_URL", "https://must-not-see.test/v1");

    let config = distinct_config();
    let projected = env_chat_lane_deployments(&config, OBSERVED_AT);
    let extract = projected
        .iter()
        .find(|lane| lane.lane == "extract")
        .expect("extract lane");

    assert_eq!(extract.deployment.provider_model_id, "Qwen/Qwen3.5-27B");
    assert_eq!(
        extract.deployment.endpoint_ref.as_deref(),
        Some("https://api.siliconflow.cn/v1/chat/completions")
    );
}

#[test]
fn the_projection_is_a_pure_function_of_the_config() {
    let config = distinct_config();
    assert_eq!(
        env_chat_lane_deployments(&config, OBSERVED_AT),
        env_chat_lane_deployments(&config, OBSERVED_AT)
    );
}

// ─── provenance is public-safe ───────────────────────────────────────────────

#[test]
fn a_base_url_carrying_userinfo_never_becomes_durable_provenance() {
    // A `base_url` should never look like this. The rule that catalog rows are
    // public-safe metadata (#1680, inherited here) is not allowed to depend on
    // that never happening.
    let mut config = distinct_config();
    config.extract.base_url =
        "https://svc-account:sk-live-SECRET@proxy.internal:8443/v1/chat".to_string();

    let projected = env_chat_lane_deployments(&config, OBSERVED_AT);
    let extract = projected
        .iter()
        .find(|lane| lane.lane == "extract")
        .expect("extract lane");

    assert_eq!(
        extract.deployment.provider_account_id, "env:proxy.internal:8443",
        "the account handle must be the endpoint authority with userinfo stripped"
    );
    assert!(
        !extract.deployment.provider_account_id.contains("SECRET"),
        "credential material must never reach the account handle"
    );
    assert!(
        !extract
            .deployment
            .provider_account_id
            .contains("svc-account"),
        "nor the user half of it"
    );
}

#[test]
fn source_refs_carry_env_var_names_and_nothing_else() {
    let config = distinct_config();
    let projected = env_chat_lane_deployments(&config, OBSERVED_AT);
    for lane in projected {
        for source_ref in &lane.deployment.source_refs {
            assert!(
                source_ref.starts_with("env_api_key:"),
                "lane {}: unexpected source_ref shape {source_ref}",
                lane.lane
            );
            let name = source_ref
                .strip_prefix("env_api_key:")
                .expect("prefix checked");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "lane {}: {name} is not an env-var name — a *value* must never land here",
                lane.lane
            );
        }
    }
}

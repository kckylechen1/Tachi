//! Env-chain → catalog import discriminators (tachi#1681 D7 PR-B, item 2).
//!
//! The one the design names: **catalog contents ≡ env-resolution results**.
//! Written so it can actually fail — a mutant that drops a lane, swaps a
//! model, forgets the key chain, or re-reads env instead of projecting the
//! config in hand is caught by name.

use memcore::catalog::endpoint::EndpointCredentialLeak;
use memcore::catalog::{CatalogSource, ProtocolKind};
use memcore::db::model_catalog::{list_model_deployments_by_source, DeploymentWrite};
use rusqlite::Connection;

use super::EnvRestore;
use crate::llm::catalog_import::{
    env_chat_lane_deployments, env_deployment_id, import_env_chat_lanes, CatalogImportError,
    ENV_CHAT_LANES,
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

    let projected = env_chat_lane_deployments(&config, OBSERVED_AT).expect("projection");
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
    let projected = env_chat_lane_deployments(&config, OBSERVED_AT).expect("projection");
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
        env_chat_lane_deployments(&config, OBSERVED_AT).expect("projection"),
        env_chat_lane_deployments(&config, OBSERVED_AT).expect("projection")
    );
}

// ─── provenance is public-safe ───────────────────────────────────────────────

/// The secret string used by the refusal tests. Distinctive enough that a
/// substring search over a whole error message, JSON blob or table dump is a
/// real assertion rather than a coincidence.
const SMUGGLED_SECRET: &str = "sk-live-SECRET";
const SMUGGLED_USER: &str = "svc-account";
const SMUGGLED_BASE_URL: &str = "https://svc-account:sk-live-SECRET@proxy.internal:8443/v1/chat";

#[test]
fn a_base_url_carrying_userinfo_is_refused_and_lands_nothing_in_the_catalog() {
    // An earlier revision scrubbed userinfo out of the derived account handle
    // and stored the raw URL in `endpoint_ref` — the credential survived in
    // the same durable, operator-visible row the scrubbing existed to protect.
    // The rule is refusal, and it has to hold for *every* field at once, which
    // is only checkable by looking at what reached the store.
    let conn = catalog_conn();
    let mut config = distinct_config();
    config.extract.base_url = SMUGGLED_BASE_URL.to_string();

    let refusal = import_env_chat_lanes(&conn, &config, OBSERVED_AT)
        .expect_err("a userinfo-bearing base_url must not import");
    assert!(
        matches!(
            refusal,
            CatalogImportError::EndpointCarriesCredential {
                lane: "extract",
                leak: EndpointCredentialLeak::Userinfo
            }
        ),
        "the refusal must be typed and name the offending lane: {refusal:?}"
    );

    // Not one row, not even the three clean lanes: a partially imported
    // catalog would be a caller's problem to unwind, and the projection is
    // built in full before the first write precisely so it never is.
    let stored = list_model_deployments_by_source(&conn, CatalogSource::Env).expect("rows read");
    assert!(
        stored.is_empty(),
        "a refused import must leave the catalog untouched, found {} row(s)",
        stored.len()
    );
    let events =
        memcore::db::model_catalog::list_all_model_deployment_events(&conn).expect("events read");
    assert!(
        events.is_empty(),
        "nor may it append events: {} event(s)",
        events.len()
    );
}

#[test]
fn a_base_url_carrying_a_credential_shaped_query_key_is_refused_the_same_way() {
    // Userinfo is not the only place a URL smuggles a credential, and this is
    // the hole the shared rule closed: `?api_key=` reaches the same durable
    // `endpoint_ref` column, the same status output and the same logs, and
    // this side of the boundary used to check only the authority. The deny
    // list is now memcore's, shared with the request path's own check, so
    // extending it tightens both surfaces at once.
    let conn = catalog_conn();
    let mut config = distinct_config();
    config.summary.base_url = "https://proxy.internal/v1/chat?api_key=sk-live-SECRET".to_string();

    let refusal = import_env_chat_lanes(&conn, &config, OBSERVED_AT)
        .expect_err("a query-string credential must not import either");
    assert!(
        matches!(
            &refusal,
            CatalogImportError::EndpointCarriesCredential {
                lane: "summary",
                leak: EndpointCredentialLeak::QueryKey { key }
            } if key == "api_key"
        ),
        "the refusal must name the lane and the offending key: {refusal:?}"
    );
    for rendering in [refusal.to_string(), format!("{refusal:?}")] {
        assert!(
            !rendering.contains("sk-live-SECRET"),
            "the refusal repeated the credential: {rendering}"
        );
    }
    assert!(
        list_model_deployments_by_source(&conn, CatalogSource::Env)
            .expect("rows read")
            .is_empty(),
        "a refused import must leave the catalog untouched"
    );
}

#[test]
fn an_ordinary_query_string_still_imports() {
    // The rule is about credential keys, not about query strings. An endpoint
    // that pins an API version is an ordinary endpoint.
    let conn = catalog_conn();
    let mut config = distinct_config();
    config.summary.base_url = "https://proxy.internal/v1/chat?api-version=2026-01-01".to_string();

    import_env_chat_lanes(&conn, &config, OBSERVED_AT).expect("an ordinary query string imports");
    let stored = list_model_deployments_by_source(&conn, CatalogSource::Env).expect("rows read");
    assert_eq!(stored.len(), 4);
}

#[test]
fn a_refusal_never_repeats_the_credential_it_refused() {
    // The refusal is logged by the daemon and rendered into status JSON, which
    // is exactly the audience the credential must not reach. `Display` and
    // `Debug` both count: a `{err:?}` in some future log line is one keystroke
    // away.
    let mut config = distinct_config();
    config.extract.base_url = SMUGGLED_BASE_URL.to_string();

    let refusal = env_chat_lane_deployments(&config, OBSERVED_AT)
        .expect_err("a userinfo-bearing base_url must not project");
    for rendering in [refusal.to_string(), format!("{refusal:?}")] {
        assert!(
            !rendering.contains(SMUGGLED_SECRET),
            "the refusal repeated the password: {rendering}"
        );
        assert!(
            !rendering.contains(SMUGGLED_USER),
            "the refusal repeated the username: {rendering}"
        );
        assert!(
            !rendering.contains("proxy.internal"),
            "the refusal repeated the endpoint: {rendering}"
        );
        assert!(
            rendering.contains("extract"),
            "but it must still say which lane to fix: {rendering}"
        );
    }
}

#[test]
fn every_chat_lane_is_gated_not_just_the_first() {
    // A gate applied only where the loop happens to start is the classic
    // half-fix. Each lane gets its own turn at carrying the credential.
    for lane in ENV_CHAT_LANES {
        let conn = catalog_conn();
        let mut config = distinct_config();
        match lane {
            "extract" => config.extract.base_url = SMUGGLED_BASE_URL.to_string(),
            "summary" => config.summary.base_url = SMUGGLED_BASE_URL.to_string(),
            "reasoning" => config.reasoning.base_url = SMUGGLED_BASE_URL.to_string(),
            "distill" => config.distill.base_url = SMUGGLED_BASE_URL.to_string(),
            other => panic!("unknown lane {other}"),
        }

        let refusal = import_env_chat_lanes(&conn, &config, OBSERVED_AT)
            .err()
            .unwrap_or_else(|| panic!("lane {lane} carried userinfo and must be refused"));
        assert!(
            matches!(
                refusal,
                CatalogImportError::EndpointCarriesCredential {
                    lane: refused,
                    leak: EndpointCredentialLeak::Userinfo
                } if refused == lane
            ),
            "lane {lane}: expected a userinfo refusal naming it, got {refusal:?}"
        );

        let stored =
            list_model_deployments_by_source(&conn, CatalogSource::Env).expect("rows read");
        assert!(
            stored.is_empty(),
            "lane {lane}: a refused import must leave the catalog untouched"
        );
    }
}

#[test]
fn an_at_sign_outside_the_authority_is_not_a_credential() {
    // Refusing every URL containing `@` would break scoped model paths. The
    // gate reads the authority, the same substring the account handle is
    // derived from.
    let mut config = distinct_config();
    config.extract.base_url = "https://api.siliconflow.cn/v1/@scope/chat/completions".to_string();

    let projected = env_chat_lane_deployments(&config, OBSERVED_AT)
        .expect("an `@` in the path is not userinfo");
    let extract = projected
        .iter()
        .find(|lane| lane.lane == "extract")
        .expect("extract lane");
    assert_eq!(
        extract.deployment.provider_account_id, "env:api.siliconflow.cn",
        "the account handle is still the authority"
    );
}

#[test]
fn source_refs_carry_env_var_names_and_nothing_else() {
    let config = distinct_config();
    let projected = env_chat_lane_deployments(&config, OBSERVED_AT).expect("projection");
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

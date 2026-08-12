//! Model-lane status, as a **projection** rather than a mirror (tachi#1681 D3).
//!
//! This file used to hand-maintain a JSON copy of every lane's provider, base
//! URL, model and key chain. Nothing kept it in step with
//! `provider_health/config.rs`, which is where those values actually come
//! from, so it was guaranteed to drift — and it had already half-converted
//! itself for rerank (the old comment at line 14: "report the actually
//! configured rerank provider, not a hardcoded 'voyage'"). Same disease, same
//! cure, now applied to the other five lanes: every value below is read from
//! the live `ProviderRuntimeConfig` / `EmbeddingConfig` / `RerankConfig`, and
//! the chat and embedding lanes are rendered from the *same* catalog
//! projection (`tachi_llm::catalog_import`) that produces the
//! `catalog_source='env'` deployment rows. Status and catalog cannot disagree,
//! because they are one derivation.
//!
//! # What is still prose, and why
//!
//! Each lane keeps a `provider` string describing its **call strategy**
//! ("claude-cli-first, openai-compatible fallback"). That is not a resolved
//! value and is not something the catalog knows — it is a statement about
//! which code path runs — so it stays a literal here, pinned by the existing
//! `model_lanes_distinguish_api_only_distill_from_cli_first_reasoning` test.
//! The drift risk this file existed to create was in the *values*, and those
//! are gone.
//!
//! # Failure reporting
//!
//! A config that does not resolve surfaces as a `config_error` field rather
//! than a plausible-looking default, keeping the rule the rerank half already
//! followed: status must not lie about what is configured.

use std::path::Path;

use serde_json::{json, Value};
use tachi_llm::{
    env_chat_lane_deployments, env_embedding_deployment, voyage_embeddings_endpoint,
    EmbeddingConfig, ProviderRuntimeConfig, RerankConfig,
};

use super::api_keys::collect_api_key_status;
use crate::status_ops::EXPECTED_EMBEDDING_DIM;

/// Prefix `catalog_import` puts on an api-key provenance entry. Stripped here
/// so the `keys` array keeps the shape consumers already read.
const ENV_API_KEY_SOURCE_PREFIX: &str = "env_api_key:";

/// Per-lane call-strategy descriptions. Not resolved values — see the module
/// note.
const EXTRACT_STRATEGY: &str = "openai-compatible";
const SUMMARY_STRATEGY: &str = "openai-compatible";
const DISTILL_STRATEGY: &str = "openai-compatible API only; FOUNDRY_DISTILL_BACKEND=claude_cli is a legacy selector (no Claude subprocess)";
const REASONING_STRATEGY: &str = "claude-cli-first, openai-compatible fallback";

pub(crate) fn provider_key_status_json(global_db_path: &Path) -> serde_json::Value {
    json!(collect_api_key_status(global_db_path))
}

pub(crate) fn model_lanes_json() -> serde_json::Value {
    let rerank_cfg = RerankConfig::from_env();
    let (rerank_provider, rerank_model, rerank_keys, local_endpoint, rerank_config_error) =
        match &rerank_cfg {
            Ok(cfg) => {
                let keys: serde_json::Value = match cfg.provider {
                    tachi_llm::RerankProviderKind::Voyage => {
                        json!(["VOYAGE_RERANK_API_KEY", "VOYAGE_API_KEY"])
                    }
                    tachi_llm::RerankProviderKind::Local => json!([]),
                };
                (
                    cfg.provider_name(),
                    cfg.model_name().map(str::to_string),
                    keys,
                    cfg.local_endpoint.clone(),
                    None::<String>,
                )
            }
            Err(err) => ("invalid", None, json!([]), None, Some(err.clone())),
        };

    let mut rerank_lane = json!({
        "provider": rerank_provider,
        "model": rerank_model,
        "keys": rerank_keys,
    });
    if let Some(endpoint) = local_endpoint {
        rerank_lane
            .as_object_mut()
            .expect("rerank_lane object")
            .insert("local_endpoint".into(), json!(endpoint));
    }
    if let Some(err) = rerank_config_error {
        rerank_lane
            .as_object_mut()
            .expect("rerank_lane object")
            .insert("config_error".into(), json!(err));
    }

    let auth_failure_hint = match rerank_cfg.as_ref().map(|c| c.provider) {
        Ok(tachi_llm::RerankProviderKind::Local) => {
            "403 during query generation points to SILICONFLOW_API_KEY; local rerank uses TACHI_RERANK_LOCAL_ENDPOINT (no Voyage key)"
        }
        _ => {
            "403 during query generation points to SILICONFLOW_API_KEY; 403 during Voyage rerank points to VOYAGE_RERANK_API_KEY or VOYAGE_API_KEY"
        }
    };

    let chat_lanes = chat_lane_projection();
    let lane = |name: &str, strategy: &str| -> Value {
        match &chat_lanes {
            Ok(lanes) => lanes
                .get(name)
                .cloned()
                .unwrap_or_else(|| json!({ "provider": strategy })),
            Err(err) => json!({ "provider": strategy, "config_error": err }),
        }
    };

    json!({
        "embedding": embedding_lane_json(),
        "rerank": rerank_lane,
        "recall_rerank_cache": {
            "query_generation_provider": "extract/SiliconFlow",
            "rerank_provider": rerank_provider,
            "auth_failure_hint": auth_failure_hint,
        },
        "extract": lane("extract", EXTRACT_STRATEGY),
        "summary": lane("summary", SUMMARY_STRATEGY),
        "distill": lane("distill", DISTILL_STRATEGY),
        "reasoning": lane("reasoning", REASONING_STRATEGY),
    })
}

/// Resolve the four chat lanes and render each as the catalog row it would be
/// imported as. Keyed by lane name so the caller can pair each with its
/// strategy prose without re-deriving the order.
fn chat_lane_projection() -> Result<std::collections::BTreeMap<String, Value>, String> {
    let config = ProviderRuntimeConfig::from_env()?;
    let observed_at = memcore::db::now_utc_iso();
    Ok(env_chat_lane_deployments(&config, &observed_at)
        .into_iter()
        .map(|lane| {
            let strategy = match lane.lane {
                "extract" => EXTRACT_STRATEGY,
                "summary" => SUMMARY_STRATEGY,
                "distill" => DISTILL_STRATEGY,
                "reasoning" => REASONING_STRATEGY,
                other => other,
            };
            (
                lane.lane.to_string(),
                json!({
                    "provider": strategy,
                    "deployment_id": lane.deployment.deployment_id,
                    "catalog_source": lane.deployment.catalog_source.as_str(),
                    "provider_account_ref": lane.deployment.provider_account_id,
                    "endpoint": lane.deployment.endpoint_ref,
                    "model": lane.deployment.provider_model_id,
                    "keys": key_names(&lane.deployment.source_refs),
                }),
            )
        })
        .collect())
}

fn embedding_lane_json() -> Value {
    let config = match EmbeddingConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            // A refused embedding configuration is the loud failure #1681 D3
            // asks for; status reports the refusal rather than the model it
            // would have used.
            return json!({
                "provider": "voyage",
                "config_error": err,
                "stored_index_dimension": EXPECTED_EMBEDDING_DIM,
                "key": "VOYAGE_API_KEY",
            });
        }
    };

    let endpoint = voyage_embeddings_endpoint();
    let observed_at = memcore::db::now_utc_iso();
    let row = env_embedding_deployment(&config, &endpoint, &observed_at).deployment;

    json!({
        "provider": "voyage",
        "model": row.provider_model_id,
        "model_source": config.source.as_str(),
        "expected_dimension": config.dimension,
        "stored_index_dimension": EXPECTED_EMBEDDING_DIM,
        "endpoint": row.endpoint_ref,
        "deployment_id": row.deployment_id,
        "catalog_source": row.catalog_source.as_str(),
        "provider_account_ref": row.provider_account_id,
        "key": "VOYAGE_API_KEY",
    })
}

/// `["env_api_key:EXTRACT_API_KEY", …]` → `["EXTRACT_API_KEY", …]`.
///
/// Entries that are not api-key provenance (an override's variable name, say)
/// are dropped rather than rendered, so `keys` stays what its consumers read
/// it as: the credential precedence chain, in order.
fn key_names(source_refs: &[String]) -> Vec<String> {
    source_refs
        .iter()
        .filter_map(|source_ref| source_ref.strip_prefix(ENV_API_KEY_SOURCE_PREFIX))
        .map(str::to_string)
        .collect()
}

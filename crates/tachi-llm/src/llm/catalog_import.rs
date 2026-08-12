//! Env-chain → catalog import (tachi#1681 D3 compatibility window, D7 PR-B).
//!
//! # Read, not cut
//!
//! The four chat-lane env precedence chains are live production routing. This
//! module **observes** what they resolved to and records it as
//! `catalog_source='env'` deployment rows; it changes nothing about how
//! `LlmClient` routes. `lane_calls.rs` is untouched, `ProviderRuntimeConfig`
//! is untouched, and no code in this crate reads the catalog back. Consumer
//! cutover is #1685, and the discriminating test here — catalog contents ≡
//! env-resolution results — is that leaf's launchpad.
//!
//! # Why this is a pure function of an already-resolved config
//!
//! [`env_chat_lane_deployments`] takes `&ProviderRuntimeConfig` and reads no
//! environment of its own. If it re-resolved env, "the catalog equals the env
//! resolution" would be a tautology about two calls to the same reader rather
//! than a statement about the config the client is actually running on — and
//! a drift between what the client holds and what the catalog says would be
//! invisible. The import is a projection of the *live client's* config, which
//! is exactly the object #1685 has to cut over.
//!
//! # Deployment identity
//!
//! One row per lane, `env:{lane}`. Two lanes that collapse onto the same
//! endpoint and model (the ordinary state when `DISTILL_*` is unset — the
//! client logs about it at construction) produce two rows with identical
//! endpoint/model, which is truthful: env gives us *lane configurations*, not
//! deployments, and the lane→row mapping is precisely what #1685 needs.
//! Folding them into one deployment with two aliases is alias governance,
//! which is #1681 D2's reviewed plan/apply path (PR-D), not something this
//! import should decide on its own.

use memcore::catalog::{
    CatalogSource, DeploymentCapabilities, NewModelDeployment, ProtocolKind,
    DEPLOYMENT_STATUS_ACTIVE,
};
use memcore::db::model_catalog::{upsert_model_deployment, DeploymentWrite};
use memcore::error::MemoryError;
use rusqlite::Connection;

use super::provider_health::{ChatLaneConfig, ProviderRuntimeConfig};

/// Prefix every env-derived identifier carries, so "which rows did the env
/// chains produce" is answerable by inspection as well as by
/// `catalog_source`.
pub const ENV_CATALOG_PREFIX: &str = "env:";

/// The four chat lanes, in the order `ProviderRuntimeConfig` resolves them.
/// Public and ordered because the status projection and the #1685 cutover
/// both need a stable lane list that cannot drift from this module's output.
pub const ENV_CHAT_LANES: [&str; 4] = ["extract", "summary", "reasoning", "distill"];

/// The deployment id an env-imported lane row carries.
pub fn env_deployment_id(lane: &str) -> String {
    format!("{ENV_CATALOG_PREFIX}{lane}")
}

/// One lane's env resolution, as a catalog row.
///
/// The lane name travels beside the row rather than being parsed back out of
/// `deployment_id`: a caller that has to re-derive structure from a string it
/// was just handed is one refactor away from disagreeing with this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvLaneDeployment {
    pub lane: &'static str,
    pub deployment: NewModelDeployment,
}

/// Project an already-resolved [`ProviderRuntimeConfig`] into catalog rows,
/// one per chat lane, in [`ENV_CHAT_LANES`] order.
///
/// Reads no environment. `observed_at` is supplied by the caller (rather than
/// stamped here) so the projection is a pure function — which is what lets
/// the discriminating test compare catalog contents against the config
/// without a clock in the middle.
pub fn env_chat_lane_deployments(
    config: &ProviderRuntimeConfig,
    observed_at: &str,
) -> Vec<EnvLaneDeployment> {
    let lanes: [(&'static str, &ChatLaneConfig); 4] = [
        ("extract", &config.extract),
        ("summary", &config.summary),
        ("reasoning", &config.reasoning),
        ("distill", &config.distill),
    ];
    lanes
        .into_iter()
        .map(|(lane, lane_config)| EnvLaneDeployment {
            lane,
            deployment: chat_lane_deployment(lane, lane_config, observed_at),
        })
        .collect()
}

fn chat_lane_deployment(
    lane: &str,
    lane_config: &ChatLaneConfig,
    observed_at: &str,
) -> NewModelDeployment {
    let mut row = NewModelDeployment::observed(
        env_deployment_id(lane),
        env_provider_account_id(&lane_config.base_url),
        ProtocolKind::OpenAiChatCompletions,
        lane_config.model.clone(),
        CatalogSource::Env,
        observed_at,
    )
    .with_endpoint_ref(lane_config.base_url.clone())
    .with_capabilities(DeploymentCapabilities {
        chat: true,
        ..DeploymentCapabilities::default()
    })
    .with_source_refs(
        lane_config
            .api_key_envs
            .iter()
            .map(|name| format!("env_api_key:{name}"))
            .collect(),
    );
    row.status = DEPLOYMENT_STATUS_ACTIVE.to_string();
    // No `expires_at`: an env row is re-resolved from scratch on every process
    // start, so it cannot go stale behind the operator's back the way a
    // fetched provider catalog can. Staleness is a property of rows whose
    // source is not re-read (#1681 D7 PR-B).
    row
}

/// The account handle an env-derived row references, until #1680's reconcile
/// maps it to a real `provider_accounts.account_id`.
///
/// Derived from the endpoint authority (`env:api.siliconflow.cn`) because
/// that is the only account-ish fact an env chain actually carries: the chain
/// names *key env vars*, and which of them won is a live-process fact, not a
/// property of the resolved config.
///
/// **Userinfo is stripped, not preserved.** A `base_url` should never carry
/// `https://user:pass@host/`, but this row is a serialized public-safe
/// surface (#1680's rule for `ProviderAccount`, inherited here), so the one
/// place a credential could smuggle itself into durable operator-visible
/// provenance is closed at the derivation rather than trusted not to happen.
fn env_provider_account_id(base_url: &str) -> String {
    let without_scheme = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host = authority
        .rsplit_once('@')
        .map(|(_userinfo, host)| host)
        .unwrap_or(authority)
        .trim()
        .to_ascii_lowercase();
    if host.is_empty() {
        format!("{ENV_CATALOG_PREFIX}unknown-endpoint")
    } else {
        format!("{ENV_CATALOG_PREFIX}{host}")
    }
}

/// Write the env-chain projection into the catalog.
///
/// Idempotent by construction: `upsert_model_deployment` returns `Unchanged`
/// for a row whose content digest already matches, so running this on every
/// process start costs one comparison per lane and appends nothing.
///
/// Opens no transaction of its own (the memcore accessors' rule), so a caller
/// that wants all four lanes to land atomically wraps the call.
pub fn import_env_chat_lanes(
    conn: &Connection,
    config: &ProviderRuntimeConfig,
    observed_at: &str,
) -> Result<Vec<(&'static str, DeploymentWrite)>, MemoryError> {
    env_chat_lane_deployments(config, observed_at)
        .into_iter()
        .map(|lane| upsert_model_deployment(conn, &lane.deployment).map(|write| (lane.lane, write)))
        .collect()
}

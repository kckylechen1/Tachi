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
//!
//! # Public-safe by refusal, not by scrubbing
//!
//! Every row this module writes is durable, operator-visible provenance
//! (#1680's rule for `ProviderAccount`, inherited here). A `base_url` carrying
//! `https://user:pass@host/` therefore cannot be imported at all: the
//! projection **fails loudly** with [`CatalogImportError::EndpointCarriesUserinfo`]
//! before a single row is built, so nothing lands in the catalog and no caller
//! is left holding a half-written import.
//!
//! An earlier revision scrubbed userinfo out of the derived account handle and
//! called that enough. It was not — `endpoint_ref` still stored the raw URL, so
//! the credential survived in the same durable row the scrubbing existed to
//! protect. Refusing at the door is the only version of this rule that covers
//! every field at once, and it matches the precedent set for endpoint URLs on
//! the request path (#1682 slice-1's `EndpointUrl::new`), which likewise
//! refuses rather than silently stripping: quietly removing the credential
//! would send an unauthenticated request to an endpoint whose operator plainly
//! expected one.

use std::fmt;

use memcore::catalog::{
    CatalogSource, DeploymentCapabilities, EmbeddingsCapability, NewModelDeployment, ProtocolKind,
    DEPLOYMENT_STATUS_ACTIVE,
};
use memcore::db::model_catalog::{upsert_model_deployment, DeploymentWrite};
use memcore::error::MemoryError;
use rusqlite::Connection;

use super::embedding_config::{
    EmbeddingConfig, EmbeddingModelSource, EMBEDDING_DIMENSION_ENV, EMBEDDING_MODEL_ENV,
};
use super::provider_health::{ChatLaneConfig, ProviderRuntimeConfig};

/// Prefix every env-derived identifier carries, so "which rows did the env
/// chains produce" is answerable by inspection as well as by
/// `catalog_source`.
pub const ENV_CATALOG_PREFIX: &str = "env:";

/// The four chat lanes, in the order `ProviderRuntimeConfig` resolves them.
/// Public and ordered because the status projection and the #1685 cutover
/// both need a stable lane list that cannot drift from this module's output.
pub const ENV_CHAT_LANES: [&str; 4] = ["extract", "summary", "reasoning", "distill"];

/// The embedding lane's name. Not a chat lane — it speaks a different
/// protocol, produces no receipts, and carries the dimension declaration the
/// escape hatch gates on (#1681 D3) — so it is imported separately rather than
/// smuggled into [`ENV_CHAT_LANES`] where a caller iterating chat lanes would
/// silently pick it up.
pub const ENV_EMBEDDING_LANE: &str = "embedding";

/// The deployment id an env-imported lane row carries.
pub fn env_deployment_id(lane: &str) -> String {
    format!("{ENV_CATALOG_PREFIX}{lane}")
}

/// Why an env resolution could not become catalog rows.
///
/// **Carries no endpoint, no URL and no secret material** — only the lane
/// name. An import refusal is reported on status surfaces and in daemon logs,
/// which is precisely the audience the credential must never reach, so the
/// error value itself is metadata-only by construction rather than by every
/// caller remembering to redact it.
#[derive(Debug)]
pub enum CatalogImportError {
    /// A lane's configured endpoint carried `user:password@`. Refused before
    /// any row is built — see the module note on refusal versus scrubbing.
    EndpointCarriesUserinfo { lane: &'static str },
    /// The catalog store rejected a write.
    Store(MemoryError),
}

impl fmt::Display for CatalogImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndpointCarriesUserinfo { lane } => write!(
                formatter,
                "lane '{lane}': the configured endpoint carries userinfo credentials, and a \
                 catalog deployment row is durable public-safe provenance. Refusing to import \
                 this lane. Remove the credentials from the URL and supply them through the \
                 lane's API key environment variable instead."
            ),
            Self::Store(err) => write!(formatter, "catalog store write failed: {err}"),
        }
    }
}

impl std::error::Error for CatalogImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EndpointCarriesUserinfo { .. } => None,
            Self::Store(err) => Some(err),
        }
    }
}

impl From<MemoryError> for CatalogImportError {
    fn from(err: MemoryError) -> Self {
        Self::Store(err)
    }
}

/// Does this endpoint's authority carry a `user[:password]@` prefix?
///
/// Deliberately parsed the same way [`env_provider_account_id`] parses the
/// authority — scheme off, first `/`, `?` or `#` ends it — so the gate and the
/// derivation can never disagree about which substring is the authority. A
/// later `@` in a path or query (`/v1/@scope/model`) is not userinfo and is
/// not refused.
fn endpoint_carries_userinfo(endpoint: &str) -> bool {
    endpoint_authority(endpoint).contains('@')
}

fn endpoint_authority(endpoint: &str) -> &str {
    let without_scheme = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
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
///
/// **All-or-nothing.** One lane carrying userinfo fails the whole projection,
/// so a caller can never write three clean rows and then discover the fourth
/// was refusable. That is what makes [`import_env_chat_lanes`] safe without a
/// transaction of its own: the refusal happens before any row exists.
pub fn env_chat_lane_deployments(
    config: &ProviderRuntimeConfig,
    observed_at: &str,
) -> Result<Vec<EnvLaneDeployment>, CatalogImportError> {
    let lanes: [(&'static str, &ChatLaneConfig); 4] = [
        ("extract", &config.extract),
        ("summary", &config.summary),
        ("reasoning", &config.reasoning),
        ("distill", &config.distill),
    ];
    lanes
        .into_iter()
        .map(|(lane, lane_config)| {
            Ok(EnvLaneDeployment {
                lane,
                deployment: chat_lane_deployment(lane, lane_config, observed_at)?,
            })
        })
        .collect()
}

fn chat_lane_deployment(
    lane: &'static str,
    lane_config: &ChatLaneConfig,
    observed_at: &str,
) -> Result<NewModelDeployment, CatalogImportError> {
    if endpoint_carries_userinfo(&lane_config.base_url) {
        return Err(CatalogImportError::EndpointCarriesUserinfo { lane });
    }
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
    Ok(row)
}

/// The account handle an env-derived row references, until #1680's reconcile
/// maps it to a real `provider_accounts.account_id`.
///
/// Derived from the endpoint authority (`env:api.siliconflow.cn`) because
/// that is the only account-ish fact an env chain actually carries: the chain
/// names *key env vars*, and which of them won is a live-process fact, not a
/// property of the resolved config.
///
/// Userinfo is **refused upstream**, by [`endpoint_carries_userinfo`], before
/// this function is ever reached with a credential-bearing URL. The strip
/// below is therefore defence in depth, not the rule: it exists so a future
/// caller that reaches the derivation without going through the gate — an
/// accidentally reordered validation, a new entry point — still cannot mint a
/// credential-bearing account handle. It is deliberately *not* sufficient on
/// its own, which is the whole lesson of this file's first revision: stripping
/// here left `endpoint_ref` holding the raw URL in the same durable row.
fn env_provider_account_id(base_url: &str) -> String {
    let authority = endpoint_authority(base_url);
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
/// that wants all four lanes to land atomically wraps the call. A refusable
/// config needs no such wrapping: the projection is built (and can fail) in
/// full before the first write.
pub fn import_env_chat_lanes(
    conn: &Connection,
    config: &ProviderRuntimeConfig,
    observed_at: &str,
) -> Result<Vec<(&'static str, DeploymentWrite)>, CatalogImportError> {
    env_chat_lane_deployments(config, observed_at)?
        .into_iter()
        .map(|lane| {
            upsert_model_deployment(conn, &lane.deployment)
                .map(|write| (lane.lane, write))
                .map_err(CatalogImportError::from)
        })
        .collect()
}

/// Project the resolved embedding configuration into a catalog row.
///
/// This is where the catalog carries the **dimension declaration** #1681 D3
/// requires: `capabilities.embeddings.dimension` is the width the configured
/// model emits, as declared and already validated against the stored index by
/// [`EmbeddingConfig::from_env`]. A bare capability flag would have left the
/// catalog unable to answer the one question the escape hatch turns on.
///
/// Pure, like the chat-lane projection: `endpoint` and `observed_at` come from
/// the caller (`voyage_embeddings_endpoint()` is the endpoint a request
/// actually uses) rather than being re-derived here.
///
/// Refuses a credential-bearing `endpoint` for the same reason the chat lanes
/// do — `VOYAGE_BASE_URL` is operator-supplied and lands in `endpoint_ref`
/// verbatim.
pub fn env_embedding_deployment(
    embedding: &EmbeddingConfig,
    endpoint: &str,
    observed_at: &str,
) -> Result<EnvLaneDeployment, CatalogImportError> {
    if endpoint_carries_userinfo(endpoint) {
        return Err(CatalogImportError::EndpointCarriesUserinfo {
            lane: ENV_EMBEDDING_LANE,
        });
    }
    let mut source_refs = vec!["env_api_key:VOYAGE_API_KEY".to_string()];
    if embedding.source() == EmbeddingModelSource::EnvOverride {
        // Provenance for a deliberate operator swap: the *name* of the
        // variable that carried it, never its value.
        source_refs.push(format!("env_model:{EMBEDDING_MODEL_ENV}"));
        source_refs.push(format!("env_dimension:{EMBEDDING_DIMENSION_ENV}"));
    }

    let mut deployment = NewModelDeployment::observed(
        env_deployment_id(ENV_EMBEDDING_LANE),
        env_provider_account_id(endpoint),
        ProtocolKind::VoyageEmbeddings,
        embedding.model().to_string(),
        CatalogSource::Env,
        observed_at,
    )
    .with_endpoint_ref(endpoint.to_string())
    .with_capabilities(DeploymentCapabilities {
        embeddings: Some(EmbeddingsCapability {
            dimension: embedding.dimension(),
        }),
        ..DeploymentCapabilities::default()
    })
    .with_source_refs(source_refs);
    deployment.status = DEPLOYMENT_STATUS_ACTIVE.to_string();

    Ok(EnvLaneDeployment {
        lane: ENV_EMBEDDING_LANE,
        deployment,
    })
}

/// Write the embedding lane's row. Idempotent for the same reason
/// [`import_env_chat_lanes`] is.
pub fn import_env_embedding_lane(
    conn: &Connection,
    embedding: &EmbeddingConfig,
    endpoint: &str,
    observed_at: &str,
) -> Result<DeploymentWrite, CatalogImportError> {
    let row = env_embedding_deployment(embedding, endpoint, observed_at)?;
    upsert_model_deployment(conn, &row.deployment).map_err(CatalogImportError::from)
}

//! Model-broker catalog row types (tachi#1681 D1/D7).
//!
//! The typed half of the six tables PR-A added to `db::schema::ddl`; their SQL
//! lives in [`crate::db::model_catalog`]. The split mirrors the one #1680
//! already uses for provider accounts (`vault::accounts` holds the types,
//! `db::vault_accounts` holds the SQL) — a reader looking for "what is a
//! deployment" and a reader looking for "how is it written" go to different
//! files on purpose.
//!
//! `admin`-gated for the same reason `vault::accounts` is: all six tables are
//! `SchemaScope::Product`, so a `portable-kernel` build (which resolves
//! `memcore` with `default-features = false`) has neither the tables nor any
//! business owning operator-surface types.
//!
//! # What the type system carries here, and why
//!
//! - **Pricing immutability is a constructor property** (D1, review finding
//!   3 + discrimination 10). [`PricingSnapshot`] keeps `snapshot_id` and
//!   `pricing_data` private and offers no setter: the only way to obtain one
//!   is [`PricingSnapshot::mint`], which *computes* the id from the data. A
//!   caller cannot change the prices of an existing snapshot, so "catalog
//!   updates never rewrite historical invocation facts" cannot be forgotten —
//!   there is no code shape in which it would be forgotten.
//! - **Staleness is not advisory** (D7 PR-B). An expired row is not just
//!   flagged; [`AuthoritativeDeployment`] is the only type a truth-consuming
//!   caller should accept, and its sole constructor refuses an expired or
//!   retired row. Gates cut the set, they do not report beside it (the #1675
//!   PR2 precedent).
//! - **Health is a different type in a different table.** There is no health
//!   field on [`ModelDeployment`]; the four authorities (#1681 D4) stay
//!   separate by table boundary, the way `account_custody` split custody off
//!   the account row (ddl.rs:776-782).

pub mod alias_plan;
pub mod alias_policy;
pub mod endpoint;
pub mod fold;
pub mod health;
pub mod resolver;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::canonical_digest::canonical_json_digest_hex;
use crate::error::MemoryError;
use crate::vault::health::EvidenceKind;

/// Scheme prefix for a content-addressed pricing snapshot id. Versioned so a
/// future canonicalization change can coexist with `ps1:` ids already stored.
pub const PRICING_SNAPSHOT_SCHEME: &str = "ps1";

pub const DEPLOYMENT_STATUS_ACTIVE: &str = "active";
pub const DEPLOYMENT_STATUS_RETIRED: &str = "retired";

pub const ALIAS_STATUS_ACTIVE: &str = "active";
pub const ALIAS_STATUS_RETIRED: &str = "retired";

// ─── Closed vocabularies ─────────────────────────────────────────────────────

/// Where a catalog row came from. Closed because "which rows did the env
/// chains produce" is a query the #1685 cutover has to be able to ask
/// exactly, and a free-text column cannot answer it exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    /// Imported from the live env precedence chains (#1681 D3's compatibility
    /// window). These rows describe what `ProviderRuntimeConfig::from_env`
    /// resolved; they are read-only truth *about* env, never a second place
    /// routing is decided from.
    Env,
    /// Fetched from a provider's own model-listing API.
    ProviderApi,
    /// Entered by a reviewed operator plan.
    Manual,
}

impl CatalogSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::ProviderApi => "provider_api",
            Self::Manual => "manual",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "env" => Some(Self::Env),
            "provider_api" => Some(Self::ProviderApi),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// The wire protocol a deployment speaks. Closed on purpose: an unknown
/// protocol is a refusal, not a row that silently sits in the catalog looking
/// callable. New variants are added when a caller is added, not speculatively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolKind {
    /// `POST {endpoint}` with an OpenAI chat-completions body — every one of
    /// the four chat lanes.
    OpenAiChatCompletions,
    /// `POST {base}/v1/embeddings` with a Voyage body.
    VoyageEmbeddings,
}

impl ProtocolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::VoyageEmbeddings => "voyage_embeddings",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "openai_chat_completions" => Some(Self::OpenAiChatCompletions),
            "voyage_embeddings" => Some(Self::VoyageEmbeddings),
            _ => None,
        }
    }
}

/// What a deployment can do. Serialized into the `capabilities` JSON column.
///
/// `embeddings` carries the **output dimension** rather than a bare flag
/// (#1681 D3's guarded escape hatch): changing the embedding model changes
/// vector dimensionality and silently corrupts comparability with the stored
/// index, so a deployment that claims the embeddings capability is required
/// by the type to say at what width. A bare `bool` here is exactly the
/// footgun the escape hatch exists to close.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeploymentCapabilities {
    pub chat: bool,
    pub tools: bool,
    pub streaming: bool,
    pub structured_output: bool,
    pub media: bool,
    pub rerank: bool,
    pub embeddings: Option<EmbeddingsCapability>,
}

/// The embeddings capability and the width it emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingsCapability {
    /// Vectors this deployment returns, in dimensions. The escape-hatch gate
    /// compares this against the dimension the stored index was built at.
    pub dimension: u32,
}

/// Attachment limits, serialized into the `attachment_bounds` JSON column.
/// Empty for every row PR-B writes; the type exists so the column has one
/// shape rather than whatever each future writer invents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttachmentBounds {
    pub max_attachment_bytes: Option<i64>,
    pub max_attachment_count: Option<u32>,
    pub accepted_media_types: Vec<String>,
}

// ─── model_deployments ───────────────────────────────────────────────────────

/// One concrete deployment of a model behind a provider account.
///
/// Public-safe metadata only, by the same rule as [`crate::vault::accounts::ProviderAccount`]:
/// this row is a serialized operator surface, so nothing that could carry key
/// material or Vault layout belongs on it. `endpoint_ref` is a documented
/// provider URL, `provider_account_id` an opaque account handle,
/// `source_refs` env-var *names*.
///
/// Deliberately no health field — see the module note and #1681 D4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDeployment {
    pub deployment_id: String,
    /// References `provider_accounts.account_id` (#1680) by convention, the
    /// way every other cross-table reference in this schema does.
    pub provider_account_id: String,
    pub endpoint_ref: Option<String>,
    pub protocol_kind: ProtocolKind,
    pub provider_model_id: String,
    pub effective_version: Option<String>,
    pub capabilities: DeploymentCapabilities,
    pub context_window: Option<i64>,
    pub max_output: Option<i64>,
    pub attachment_bounds: AttachmentBounds,
    pub region: Option<String>,
    pub data_policy: Option<String>,
    /// Live-catalog pointer at `pricing_snapshots.snapshot_id`. A completed
    /// invocation's cost is frozen by copying the id onto the outcome row at
    /// write time (#1681 D6), never by dereferencing this after the fact.
    pub pricing_snapshot_ref: Option<String>,
    pub catalog_source: CatalogSource,
    pub fetched_at: String,
    pub effective_at: String,
    /// When this row stops being authoritative. `None` = no declared
    /// expiry (an env-chain row is re-resolved every process start, so it
    /// cannot go stale behind the operator's back).
    pub expires_at: Option<String>,
    pub status: String,
    pub revision: i64,
    pub source_refs: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The caller-supplied half of a deployment row.
///
/// Separate from [`ModelDeployment`] for the reason `NewProviderAccount` is
/// separate from `ProviderAccount`: `revision`, `created_at` and `updated_at`
/// are the store's, and a caller that could pass its own `revision` could
/// rewind the counter every later slice binds its preconditions to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewModelDeployment {
    pub deployment_id: String,
    pub provider_account_id: String,
    pub endpoint_ref: Option<String>,
    pub protocol_kind: ProtocolKind,
    pub provider_model_id: String,
    pub effective_version: Option<String>,
    pub capabilities: DeploymentCapabilities,
    pub context_window: Option<i64>,
    pub max_output: Option<i64>,
    pub attachment_bounds: AttachmentBounds,
    pub region: Option<String>,
    pub data_policy: Option<String>,
    pub pricing_snapshot_ref: Option<String>,
    pub catalog_source: CatalogSource,
    pub fetched_at: String,
    pub effective_at: String,
    pub expires_at: Option<String>,
    pub status: String,
    pub source_refs: Vec<String>,
}

impl NewModelDeployment {
    /// The common case: an active deployment whose `fetched_at` and
    /// `effective_at` are the same observation instant.
    pub fn observed(
        deployment_id: impl Into<String>,
        provider_account_id: impl Into<String>,
        protocol_kind: ProtocolKind,
        provider_model_id: impl Into<String>,
        catalog_source: CatalogSource,
        observed_at: impl Into<String>,
    ) -> Self {
        let observed_at = observed_at.into();
        Self {
            deployment_id: deployment_id.into(),
            provider_account_id: provider_account_id.into(),
            endpoint_ref: None,
            protocol_kind,
            provider_model_id: provider_model_id.into(),
            effective_version: None,
            capabilities: DeploymentCapabilities::default(),
            context_window: None,
            max_output: None,
            attachment_bounds: AttachmentBounds::default(),
            region: None,
            data_policy: None,
            pricing_snapshot_ref: None,
            catalog_source,
            fetched_at: observed_at.clone(),
            effective_at: observed_at,
            expires_at: None,
            status: DEPLOYMENT_STATUS_ACTIVE.to_string(),
            source_refs: Vec::new(),
        }
    }

    pub fn with_endpoint_ref(mut self, endpoint_ref: impl Into<String>) -> Self {
        self.endpoint_ref = Some(endpoint_ref.into());
        self
    }

    pub fn with_capabilities(mut self, capabilities: DeploymentCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_source_refs(mut self, source_refs: Vec<String>) -> Self {
        self.source_refs = source_refs;
        self
    }

    pub fn with_expires_at(mut self, expires_at: impl Into<String>) -> Self {
        self.expires_at = Some(expires_at.into());
        self
    }

    /// The fields that make a re-import a *change* rather than a no-op.
    ///
    /// Compared by [`crate::db::model_catalog::upsert_model_deployment`] to
    /// decide between `Unchanged` and `Advanced`. `fetched_at` is excluded on
    /// purpose: re-observing the same deployment a second later is not a
    /// catalog change, and treating it as one would turn every process start
    /// into a revision bump and an event row.
    pub(crate) fn identity_payload(&self) -> Value {
        json!({
            "provider_account_id": self.provider_account_id,
            "endpoint_ref": self.endpoint_ref,
            "protocol_kind": self.protocol_kind.as_str(),
            "provider_model_id": self.provider_model_id,
            "effective_version": self.effective_version,
            "capabilities": self.capabilities,
            "context_window": self.context_window,
            "max_output": self.max_output,
            "attachment_bounds": self.attachment_bounds,
            "region": self.region,
            "data_policy": self.data_policy,
            "pricing_snapshot_ref": self.pricing_snapshot_ref,
            "catalog_source": self.catalog_source.as_str(),
            "expires_at": self.expires_at,
            "status": self.status,
            "source_refs": self.source_refs,
        })
    }

    /// Content digest of everything that distinguishes this deployment from a
    /// different one, used as the change detector on re-import.
    pub fn content_digest(&self) -> String {
        canonical_json_digest_hex(&self.identity_payload())
    }
}

impl ModelDeployment {
    /// The same digest [`NewModelDeployment::content_digest`] computes, over
    /// the stored row — so "did the env chains move" is one comparison, not a
    /// field-by-field walk a future field can be forgotten from.
    pub fn content_digest(&self) -> String {
        canonical_json_digest_hex(&json!({
            "provider_account_id": self.provider_account_id,
            "endpoint_ref": self.endpoint_ref,
            "protocol_kind": self.protocol_kind.as_str(),
            "provider_model_id": self.provider_model_id,
            "effective_version": self.effective_version,
            "capabilities": self.capabilities,
            "context_window": self.context_window,
            "max_output": self.max_output,
            "attachment_bounds": self.attachment_bounds,
            "region": self.region,
            "data_policy": self.data_policy,
            "pricing_snapshot_ref": self.pricing_snapshot_ref,
            "catalog_source": self.catalog_source.as_str(),
            "expires_at": self.expires_at,
            "status": self.status,
            "source_refs": self.source_refs,
        }))
    }
}

// ─── model_deployment_events ─────────────────────────────────────────────────

/// What a deployment event says happened. A closed vocabulary because
/// [`fold`] folds on it; an unrecognized kind is surfaced in the projection
/// rather than silently skipped (see [`fold::CatalogProjection`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentEventKind {
    /// The row was created — the first event every deployment has.
    DeploymentImported,
    /// A re-import found different content and advanced the revision.
    DeploymentUpdated,
    /// The deployment left `active`.
    DeploymentRetired,
    /// The deployment served a request ([`health::DeploymentOutcome::Served`]).
    HealthServed,
    /// The deployment was throttled and is cooling down.
    HealthCooldown,
    /// The deployment failed in a way that is its own (unreachable, `5xx`,
    /// unusable response). Never an auth failure — see
    /// [`health::DeploymentOutcome`].
    HealthError,
}

impl DeploymentEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeploymentImported => "deployment_imported",
            Self::DeploymentUpdated => "deployment_updated",
            Self::DeploymentRetired => "deployment_retired",
            Self::HealthServed => "deployment_health_served",
            Self::HealthCooldown => "deployment_health_cooldown",
            Self::HealthError => "deployment_health_error",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "deployment_imported" => Some(Self::DeploymentImported),
            "deployment_updated" => Some(Self::DeploymentUpdated),
            "deployment_retired" => Some(Self::DeploymentRetired),
            "deployment_health_served" => Some(Self::HealthServed),
            "deployment_health_cooldown" => Some(Self::HealthCooldown),
            "deployment_health_error" => Some(Self::HealthError),
            _ => None,
        }
    }

    /// Whether this kind belongs to the health authority (#1681 D4) rather
    /// than to catalog metadata.
    ///
    /// The distinction is load-bearing for the fold: a health event records how
    /// a deployment is *behaving* and must never be read as a change to what it
    /// *is*. Two authorities, one append-only log, and the fold keeps them in
    /// separate fields (#1681 D1's table-boundary rule, applied to the
    /// projection).
    pub fn is_health(self) -> bool {
        self.health_state().is_some()
    }

    /// The `model_deployment_health.state` a health event of this kind put the
    /// row in; `None` for the catalog-metadata kinds.
    ///
    /// This is the mapping that lets the fold reconstruct health state from the
    /// log alone — which is what makes replaying the log a check on the table
    /// rather than a second copy of it. It is the same mapping
    /// [`health::DeploymentOutcome::state`] applies when writing, and a test
    /// pins the two together.
    pub fn health_state(self) -> Option<&'static str> {
        match self {
            Self::HealthServed => Some(health::DEPLOYMENT_HEALTH_STATE_OK),
            Self::HealthCooldown => Some(health::DEPLOYMENT_HEALTH_STATE_COOLDOWN),
            Self::HealthError => Some(health::DEPLOYMENT_HEALTH_STATE_ERROR),
            Self::DeploymentImported | Self::DeploymentUpdated | Self::DeploymentRetired => None,
        }
    }
}

/// One append-only deployment audit row, shaped like `ProviderAccountEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDeploymentEvent {
    pub id: i64,
    pub deployment_id: String,
    /// The deployment revision this event produced.
    pub revision: i64,
    /// Free-form at the row level so an event written by a newer build is
    /// still readable; [`DeploymentEventKind::parse`] is where it becomes
    /// typed, and the fold reports what it could not type.
    pub event_kind: String,
    pub plan_digest: Option<String>,
    /// JSON. Public-safe by the same rule as the deployment row: digests,
    /// names and counts, never key material or provider error text.
    pub evidence: String,
    pub created_at: String,
}

/// The caller-supplied half of an event row (`id` and `created_at` are the
/// store's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewModelDeploymentEvent {
    pub deployment_id: String,
    pub revision: i64,
    pub event_kind: String,
    pub plan_digest: Option<String>,
    pub evidence: String,
}

impl NewModelDeploymentEvent {
    pub fn new(
        deployment_id: impl Into<String>,
        revision: i64,
        event_kind: DeploymentEventKind,
    ) -> Self {
        Self {
            deployment_id: deployment_id.into(),
            revision,
            event_kind: event_kind.as_str().to_string(),
            plan_digest: None,
            evidence: "{}".to_string(),
        }
    }

    pub fn with_plan_digest(mut self, plan_digest: impl Into<String>) -> Self {
        self.plan_digest = Some(plan_digest.into());
        self
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = evidence.into();
        self
    }
}

// ─── model_aliases + model_alias_bindings ────────────────────────────────────

/// A stable `ModelRef` alias (`reasoning.high`, `coding.review`).
///
/// PR-B reads these; the reviewed plan/apply write path is #1681 D2 / PR-D.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAlias {
    pub alias_name: String,
    /// JSON object of binding conditions candidate deployments must satisfy.
    pub required_capabilities: String,
    /// JSON object of data/region/budget constraints.
    pub constraints: String,
    pub status: String,
    pub revision: i64,
    /// Canonical-JSON content digest of the alias set, the
    /// `route_policy_source_revision` mechanism reused rather than reinvented.
    pub policy_digest: Option<String>,
    pub source_refs: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One alias → deployment candidacy, in priority order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAliasBinding {
    pub alias_name: String,
    pub deployment_id: String,
    pub priority: i64,
    pub retired: bool,
    pub created_at: String,
    pub updated_at: String,
}

// ─── model_alias_events ──────────────────────────────────────────────────────

/// What an alias event says happened, shaped after [`DeploymentEventKind`].
///
/// A closed vocabulary for the same reason: the log is what an auditor reads
/// to answer "who moved this alias, and under which approved plan", and a
/// free-form kind string makes that question unanswerable by grep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasEventKind {
    /// The alias row was created — the first event every alias has.
    AliasDeclared,
    /// A declaration changed the alias's shape and advanced its revision.
    AliasUpdated,
    /// The alias left `active`.
    AliasRetired,
    /// A deployment was bound (or re-prioritized, or revived) as a candidate.
    AliasBindingBound,
    /// A candidacy was retired. The binding row stays.
    AliasBindingRetired,
}

impl AliasEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AliasDeclared => "alias_declared",
            Self::AliasUpdated => "alias_updated",
            Self::AliasRetired => "alias_retired",
            Self::AliasBindingBound => "alias_binding_bound",
            Self::AliasBindingRetired => "alias_binding_retired",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "alias_declared" => Some(Self::AliasDeclared),
            "alias_updated" => Some(Self::AliasUpdated),
            "alias_retired" => Some(Self::AliasRetired),
            "alias_binding_bound" => Some(Self::AliasBindingBound),
            "alias_binding_retired" => Some(Self::AliasBindingRetired),
            _ => None,
        }
    }
}

/// One append-only alias audit row, shaped like [`ModelDeploymentEvent`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAliasEvent {
    pub id: i64,
    pub alias_name: String,
    /// The alias revision this event produced. Every event this store writes
    /// is written by the plan/apply door immediately after the write that
    /// moved the revision, so an alias whose current revision is not the
    /// revision of its newest event is the signature of a write that came from
    /// somewhere else.
    pub revision: i64,
    /// Free-form at the row level so an event written by a newer build is
    /// still readable; [`AliasEventKind::parse`] is where it becomes typed.
    pub event_kind: String,
    /// The `bp1:` digest of the approved plan that caused this event. Never
    /// `None` for an event this crate writes: an alias only moves through an
    /// approved plan, and the digest is what binds the row to the artifact an
    /// operator read.
    pub plan_digest: Option<String>,
    /// JSON. Public-safe by the same rule as the alias row: names, ids and
    /// numbers, never credentials or provider error text.
    pub evidence: String,
    pub created_at: String,
}

/// The caller-supplied half of an alias event row (`id` and `created_at` are
/// the store's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewModelAliasEvent {
    pub alias_name: String,
    pub revision: i64,
    pub event_kind: String,
    pub plan_digest: Option<String>,
    pub evidence: String,
}

impl NewModelAliasEvent {
    pub fn new(alias_name: impl Into<String>, revision: i64, event_kind: AliasEventKind) -> Self {
        Self {
            alias_name: alias_name.into(),
            revision,
            event_kind: event_kind.as_str().to_string(),
            plan_digest: None,
            evidence: "{}".to_string(),
        }
    }

    pub fn with_plan_digest(mut self, plan_digest: impl Into<String>) -> Self {
        self.plan_digest = Some(plan_digest.into());
        self
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = evidence.into();
        self
    }
}

// ─── pricing_snapshots ───────────────────────────────────────────────────────

/// A content-addressed price sheet: `snapshot_id` **is** the digest of the
/// prices (#1681 D1, discrimination 10).
///
/// Both identity-bearing fields are private and there is no setter. That is
/// the whole mechanism: re-importing an unchanged sheet dedupes onto the same
/// id, any price change necessarily mints a new one, and no code path exists
/// that could edit the prices under an id somebody already recorded on a
/// historical outcome row. Immutability is a property of the constructor and
/// the primary key, not a discipline a future writer has to remember.
///
/// `Serialize` but deliberately **not** `Deserialize`: a derived
/// `Deserialize` would hand any caller a back door that sets `snapshot_id`
/// and `pricing_data` independently — exactly the "prices changed under an id
/// somebody already recorded" state the private fields exist to make
/// unrepresentable. Reading one back out of the store goes through
/// [`PricingSnapshot::from_stored`], which re-derives the id and refuses a
/// mismatch.
///
/// Both halves of that are compile-time, following the receipt-field
/// precedent (`tachi-llm` `types.rs:142-218`) rather than resting on review
/// catching it. Prices cannot be edited under an existing id:
///
/// ```compile_fail
/// use memcore::catalog::PricingSnapshot;
/// let mut snapshot = PricingSnapshot::mint(
///     "deepseek",
///     serde_json::json!({"input": "0.14"}),
///     None,
///     "2026-08-11T00:00:00.000Z",
/// );
/// // error[E0616]: field `pricing_data` of struct `PricingSnapshot` is private
/// snapshot.pricing_data = serde_json::json!({"input": "999.00"});
/// ```
///
/// …and a snapshot cannot be conjured from JSON, which would set the id and
/// the prices independently:
///
/// ```compile_fail
/// use memcore::catalog::PricingSnapshot;
/// // error[E0277]: the trait bound `PricingSnapshot: Deserialize<'_>` is not satisfied
/// let _: PricingSnapshot = serde_json::from_str(
///     r#"{"snapshot_id":"ps1:0","provider_kind":"deepseek","pricing_data":{},
///         "catalog_source":null,"fetched_at":"","created_at":""}"#,
/// )
/// .unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PricingSnapshot {
    snapshot_id: String,
    provider_kind: String,
    pricing_data: Value,
    pub catalog_source: Option<CatalogSource>,
    pub fetched_at: String,
    pub created_at: String,
}

impl PricingSnapshot {
    /// Mint a snapshot from prices. The id is computed, never accepted.
    ///
    /// `fetched_at`/`created_at` are deliberately **outside** the digest: the
    /// same price sheet observed an hour later is the same prices, and
    /// letting the clock into the id would mint a new snapshot on every
    /// refresh — turning the dedupe property into noise and the immutability
    /// property into an accident.
    pub fn mint(
        provider_kind: impl Into<String>,
        pricing_data: Value,
        catalog_source: Option<CatalogSource>,
        observed_at: impl Into<String>,
    ) -> Self {
        let provider_kind = provider_kind.into();
        let observed_at = observed_at.into();
        let snapshot_id = Self::compute_id(&provider_kind, &pricing_data);
        Self {
            snapshot_id,
            provider_kind,
            pricing_data,
            catalog_source,
            fetched_at: observed_at.clone(),
            created_at: observed_at,
        }
    }

    /// Rebuild a snapshot read back out of the store, **verifying** that the
    /// stored id still equals the digest of the stored prices.
    ///
    /// A mismatch is a refusal, not a repair: it means something wrote prices
    /// under an id that historical outcome rows already point at, which is
    /// precisely the rewrite the content-addressed key exists to make
    /// impossible. Silently returning the row would let the corrupted cost
    /// travel onward.
    pub fn from_stored(
        snapshot_id: impl Into<String>,
        provider_kind: impl Into<String>,
        pricing_data: Value,
        catalog_source: Option<CatalogSource>,
        fetched_at: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let snapshot_id = snapshot_id.into();
        let provider_kind = provider_kind.into();
        let recomputed = Self::compute_id(&provider_kind, &pricing_data);
        if recomputed != snapshot_id {
            return Err(MemoryError::InvalidArg(format!(
                "pricing snapshot '{snapshot_id}' does not match the digest of its own prices \
                 ({recomputed}): a content-addressed snapshot was rewritten in place"
            )));
        }
        Ok(Self {
            snapshot_id,
            provider_kind,
            pricing_data,
            catalog_source,
            fetched_at: fetched_at.into(),
            created_at: created_at.into(),
        })
    }

    fn compute_id(provider_kind: &str, pricing_data: &Value) -> String {
        format!(
            "{PRICING_SNAPSHOT_SCHEME}:{}",
            canonical_json_digest_hex(&json!({
                "provider_kind": provider_kind,
                "pricing_data": pricing_data,
            }))
        )
    }

    pub fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    pub fn provider_kind(&self) -> &str {
        &self.provider_kind
    }

    pub fn pricing_data(&self) -> &Value {
        &self.pricing_data
    }
}

// ─── model_deployment_health ─────────────────────────────────────────────────

/// Deployment-level operational health (#1681 D4) — the *type*; the single
/// writer is [`health::record_deployment_outcome`].
///
/// One of four authorities that are never merged into one score. 401/403
/// never reach this table: an auth failure says nothing about the deployment.
/// That is enforced in the writer's type face — [`health::DeploymentOutcome`]
/// has no auth variant to construct — not by a runtime check, and the
/// discriminating tests for it live with the writer and with the catalog store.
///
/// No serde derive: `EvidenceKind` (#1680 D6) carries none, and inventing a
/// serialization for it here would fork that vocabulary's wire form away from
/// its owner. Health reaches a status surface through a projection, not by
/// serializing the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDeploymentHealth {
    pub deployment_id: String,
    pub state: String,
    pub cooldown_until: Option<String>,
    pub last_success_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_error: Option<String>,
    pub error_count: i64,
    /// Whether the row was produced by Tachi's own probe or reported by a
    /// consumer — the #1680 D6 vocabulary, reused verbatim.
    pub evidence_kind: Option<EvidenceKind>,
    pub observed_at: String,
    pub metadata: String,
    pub updated_at: String,
}

// ─── staleness ───────────────────────────────────────────────────────────────

/// Whether a catalog row is still speaking for the present (#1681 D7 PR-B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFreshness {
    Fresh,
    /// `expires_at` has passed. The row is still readable — history is not
    /// deleted — but it is no longer authoritative.
    Stale,
}

impl ModelDeployment {
    /// Fresh vs. stale at `now`.
    ///
    /// Both timestamps are **parsed** and compared as instants rather than
    /// compared as strings: `now_utc_iso` happens to sort lexically, but
    /// `expires_at` can arrive from an import that used a different RFC3339
    /// rendering (offset form, second precision), and a string comparison
    /// would then silently call an expired row fresh. An unparseable
    /// `expires_at` is an error, never a shrug in either direction.
    pub fn freshness_at(&self, now: &str) -> Result<CatalogFreshness, MemoryError> {
        let Some(expires_at) = self.expires_at.as_deref() else {
            return Ok(CatalogFreshness::Fresh);
        };
        let expires_at = parse_instant("expires_at", expires_at)?;
        let now = parse_instant("now", now)?;
        if now >= expires_at {
            Ok(CatalogFreshness::Stale)
        } else {
            Ok(CatalogFreshness::Fresh)
        }
    }

    /// Consume this row into the only type a truth-consuming caller should
    /// accept, or refuse.
    ///
    /// Refuses when the row is retired or expired. This is the gate shape
    /// #1675 PR2 established: the caller cannot hold an authoritative
    /// deployment it did not pass through the gate, so "we forgot to check
    /// staleness at this call site" is not expressible.
    pub fn into_authoritative_at(
        self,
        now: &str,
    ) -> Result<AuthoritativeDeployment, NotAuthoritative> {
        if self.status != DEPLOYMENT_STATUS_ACTIVE {
            return Err(NotAuthoritative::Status {
                status: self.status,
            });
        }
        match self.freshness_at(now) {
            Ok(CatalogFreshness::Fresh) => Ok(AuthoritativeDeployment(self)),
            Ok(CatalogFreshness::Stale) => Err(NotAuthoritative::Expired {
                expires_at: self.expires_at.unwrap_or_default(),
                now: now.to_string(),
            }),
            Err(err) => Err(NotAuthoritative::UnreadableTimestamp {
                detail: err.to_string(),
            }),
        }
    }
}

/// Why a deployment row is not usable as present-tense truth. Typed so the
/// resolver (#1681 D5/PR-D) can report per-candidate exclusion reasons rather
/// than a silently shorter list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotAuthoritative {
    Status { status: String },
    Expired { expires_at: String, now: String },
    UnreadableTimestamp { detail: String },
}

impl std::fmt::Display for NotAuthoritative {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status { status } => write!(f, "deployment status is '{status}', not active"),
            Self::Expired { expires_at, now } => {
                write!(f, "deployment expired at {expires_at} (now {now})")
            }
            Self::UnreadableTimestamp { detail } => {
                write!(f, "deployment freshness is unreadable: {detail}")
            }
        }
    }
}

/// A deployment row that was active and unexpired at a stated instant.
///
/// No public constructor and no `DerefMut`: the only way to obtain one is
/// [`ModelDeployment::into_authoritative_at`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoritativeDeployment(ModelDeployment);

impl AuthoritativeDeployment {
    pub fn get(&self) -> &ModelDeployment {
        &self.0
    }

    pub fn into_inner(self) -> ModelDeployment {
        self.0
    }
}

/// The two halves of a freshness partition: what survived, and the typed
/// reason each excluded `deployment_id` did not.
pub type AuthoritativePartition = (
    Vec<AuthoritativeDeployment>,
    Vec<(String, NotAuthoritative)>,
);

/// Partition rows into the authoritative ones and the typed reasons the rest
/// were cut. Both halves are returned because a resolver that reports only
/// what survived cannot explain an abstain (#1681 D5).
pub fn partition_authoritative_at(rows: Vec<ModelDeployment>, now: &str) -> AuthoritativePartition {
    let mut admitted = Vec::new();
    let mut excluded = Vec::new();
    for row in rows {
        let deployment_id = row.deployment_id.clone();
        match row.into_authoritative_at(now) {
            Ok(authoritative) => admitted.push(authoritative),
            Err(reason) => excluded.push((deployment_id, reason)),
        }
    }
    (admitted, excluded)
}

fn parse_instant(field: &str, raw: &str) -> Result<chrono::DateTime<chrono::Utc>, MemoryError> {
    chrono::DateTime::parse_from_rfc3339(raw.trim())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|err| {
            MemoryError::InvalidArg(format!("catalog {field} '{raw}' is not RFC3339: {err}"))
        })
}

#[cfg(test)]
mod tests;

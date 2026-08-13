//! The bound alias plan and its digest (tachi#1681 D2) — the reviewed write
//! path for `model_aliases` / `model_alias_bindings`.
//!
//! Shaped after `vault::apply`'s [`BoundAccountPlan`], and for the same reason:
//! a plan is not a list of intentions, it is a list of intentions **plus the
//! exact state they were decided against**. Every alias the plan touches
//! carries the revision it was planned against, every deployment it binds
//! carries the revision it was read at, and the whole set carries the
//! alias-set policy revision. Apply re-reads all of it inside one write
//! transaction and refuses the whole plan if any single binding moved.
//!
//! [`BoundAccountPlan`]: crate::vault::apply::BoundAccountPlan
//!
//! # Why aliases get a plan at all, instead of a `tachi_tune` verb
//!
//! An alias is the name a caller routes by. Changing what it binds re-routes
//! live traffic, and doing that through an ordinary mutation verb would mean a
//! routing change lands with no artifact anyone reviewed. `tachi_tune` was the
//! other candidate and was rejected on authority grounds (#1681 D2): that
//! facade is #1675's semantic-routing surface, and merging the two would erase
//! exactly the operational/semantic authority boundary this leaf exists to
//! draw.
//!
//! # What is deliberately absent from the vocabulary
//!
//! - **No delete.** Aliases and bindings retire; the rows stay, so
//!   "which deployments did this alias ever name" survives, and
//!   `model_alias_bindings.deployment_id` stays an indexed reverse lookup for
//!   "who references this deployment" even after a retirement (#1681 D1).
//! - **No deployment writes.** A plan binds deployment revisions as
//!   *preconditions* and never mints, edits or retires a deployment — that is
//!   the catalog import's authority, and a governance path that could also
//!   create the thing it governs is not a governance path.
//! - **No prose.** The artifact carries no rendered description, for the reason
//!   `vault::apply` states: a human-readable summary stored beside the digest
//!   is a lie surface, since editing only the prose leaves the digest valid
//!   while the operator approves a paragraph the plan does not implement.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Scheme prefix of an alias plan digest: `bp1:<hex64>`. Distinct from
/// `vault::apply`'s `pd1:` so a provider-account plan can never be handed to
/// alias apply and verify by accident.
pub const ALIAS_PLAN_DIGEST_SCHEME: &str = "bp1";

/// One alias plan, with the preconditions it was decided against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundAliasPlan {
    pub bindings: AliasPlanBindings,
    pub actions: Vec<AliasAction>,
}

/// Everything apply re-reads before it writes anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasPlanBindings {
    /// The alias-set policy revision (`ar1:…`) at plan time.
    ///
    /// The strongest precondition here, and the one that catches what the
    /// per-row revisions cannot: a *third* alias being rebound between plan and
    /// apply changes routing the operator reviewed nothing about, and no
    /// binding on the aliases this plan happens to touch would notice it.
    pub policy_revision: String,
    /// Aliases the plan's actions touch.
    #[serde(default)]
    pub aliases: Vec<AliasRevisionBinding>,
    /// Deployments the plan's actions bind to.
    #[serde(default)]
    pub deployments: Vec<DeploymentRevisionBinding>,
}

/// A `model_aliases` row and the revision the plan read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasRevisionBinding {
    pub alias_name: String,
    /// `None` binds **absence**: the plan was decided on the fact that no
    /// alias existed under this name, and one appearing since is drift exactly
    /// as a moved revision is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
}

/// A `model_deployments` row and the revision the plan read.
///
/// Deployments are never written by an alias plan; they are bound because
/// binding an alias to a deployment is a statement about *that* deployment,
/// and a deployment retired between plan and apply would make the statement
/// false.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentRevisionBinding {
    pub deployment_id: String,
    /// `None` binds absence, the same way [`AliasRevisionBinding::revision`]
    /// does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
}

/// The closed action vocabulary of an alias plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AliasAction {
    /// Create the alias, or bring its declared shape to this one.
    ///
    /// `required_capabilities` and `constraints` are JSON objects as text, the
    /// shape the columns hold.
    DeclareAlias {
        alias_name: String,
        #[serde(default = "empty_object")]
        required_capabilities: String,
        #[serde(default = "empty_object")]
        constraints: String,
        #[serde(default)]
        source_refs: Vec<String>,
    },
    /// Bind (or re-bind, or revive) a deployment as a candidate of this alias
    /// at this priority.
    BindDeployment {
        alias_name: String,
        deployment_id: String,
        #[serde(default)]
        priority: i64,
    },
    /// Stop offering a deployment as a candidate of this alias. The row stays.
    RetireBinding {
        alias_name: String,
        deployment_id: String,
    },
    /// Stop offering this alias as a routable name. The row and its bindings
    /// stay.
    RetireAlias { alias_name: String },
}

impl AliasAction {
    /// The alias this action touches. Apply requires a binding for it before
    /// it writes anything: an unbound alias is an unverified write, refused
    /// even when nothing has actually drifted.
    pub fn alias_name(&self) -> &str {
        match self {
            Self::DeclareAlias { alias_name, .. }
            | Self::BindDeployment { alias_name, .. }
            | Self::RetireBinding { alias_name, .. }
            | Self::RetireAlias { alias_name } => alias_name,
        }
    }

    /// The deployment this action binds to, if any.
    pub fn deployment_id(&self) -> Option<&str> {
        match self {
            Self::BindDeployment { deployment_id, .. }
            | Self::RetireBinding { deployment_id, .. } => Some(deployment_id),
            Self::DeclareAlias { .. } | Self::RetireAlias { .. } => None,
        }
    }
}

fn empty_object() -> String {
    "{}".to_string()
}

/// `bp1:<hex64>` over the plan's canonical JSON.
///
/// Canonical here means deterministic, which is what a digest needs and all it
/// needs: the plan is structs and `Vec`s, whose serde encoding is
/// field-declaration order with no map to sort, so the same plan value always
/// produces the same bytes — including after a round trip through the artifact
/// file, which is the trip the digest exists to protect.
///
/// Adding a field to any of these types changes the digest of an otherwise
/// identical plan, deliberately: a plan produced by a build that knew about a
/// precondition must not be applyable by one that would silently ignore it.
pub fn alias_plan_digest(plan: &BoundAliasPlan) -> String {
    let encoded = serde_json::to_vec(plan).expect("BoundAliasPlan is JSON-encodable by shape");
    let mut hasher = Sha256::new();
    hasher.update(&encoded);
    format!("{ALIAS_PLAN_DIGEST_SCHEME}:{:x}", hasher.finalize())
}

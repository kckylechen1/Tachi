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
//!
//! # Why every type here is `deny_unknown_fields`
//!
//! "No prose" is a statement about the *grammar*, and serde's default is to
//! discard what it does not recognize — which put the prose straight back
//! (#1681 PR-D review). An artifact carrying `"summary": "retires nothing"`
//! parsed clean, the discarded field never entered [`alias_plan_digest`], the
//! approved digest still verified, and apply proceeded: exactly the lie surface
//! the vocabulary was designed without. So an unknown field is refused rather
//! than dropped, on every type an artifact can reach. The rule composes with
//! the digest's own: a field this build knows about changes the digest, and a
//! field it does not know about is refused, so there is no third category of
//! content that can ride along unaccounted for.
//!
//! The attribute is asserted rather than assumed. [`AliasAction`] is an
//! internally tagged enum, and internally tagged deserialization buffers the
//! map before dispatching to a variant — a place where a container attribute
//! can quietly fail to survive the trip, in either direction (silently ignored,
//! or applied so aggressively that the tag itself reads as an unknown field).
//! `tests::an_unknown_field_inside_any_action_is_refused` mutates a field into
//! each of the four variants and `tests::every_action_in_the_vocabulary_still_parses`
//! holds the other end, so this module's claim rests on the tests and not on a
//! reading of serde.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Scheme prefix of an alias plan digest: `bp1:<hex64>`. Distinct from
/// `vault::apply`'s `pd1:` so a provider-account plan can never be handed to
/// alias apply and verify by accident.
pub const ALIAS_PLAN_DIGEST_SCHEME: &str = "bp1";

/// One alias plan, with the preconditions it was decided against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundAliasPlan {
    pub bindings: AliasPlanBindings,
    pub actions: Vec<AliasAction>,
}

/// Everything apply re-reads before it writes anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct DeploymentRevisionBinding {
    pub deployment_id: String,
    /// `None` binds absence, the same way [`AliasRevisionBinding::revision`]
    /// does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
}

/// The closed action vocabulary of an alias plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The plan the unknown-field tests mutate: one of every action, so a
    /// variant that stopped refusing unknown content is caught by the variant's
    /// own case rather than by whichever one the fixture happened to use.
    fn plan_json() -> serde_json::Value {
        serde_json::json!({
            "bindings": {
                "policy_revision": "ar1:00",
                "aliases": [{"alias_name": "chat.default", "revision": 7}],
                "deployments": [{"deployment_id": "env:reasoning", "revision": 3}],
            },
            "actions": [
                {
                    "action": "declare_alias",
                    "alias_name": "chat.default",
                    "required_capabilities": "{}",
                    "constraints": "{}",
                    "source_refs": ["env:reasoning"],
                },
                {
                    "action": "bind_deployment",
                    "alias_name": "chat.default",
                    "deployment_id": "env:reasoning",
                    "priority": 0,
                },
                {
                    "action": "retire_binding",
                    "alias_name": "chat.legacy",
                    "deployment_id": "env:summary",
                },
                {"action": "retire_alias", "alias_name": "chat.legacy"},
            ],
        })
    }

    /// Deserialize the fixture after running `mutate` over it.
    fn parse_mutated(
        mutate: impl FnOnce(&mut serde_json::Value),
    ) -> Result<BoundAliasPlan, serde_json::Error> {
        let mut json = plan_json();
        mutate(&mut json);
        serde_json::from_value(json)
    }

    fn insert(target: &mut serde_json::Value, key: &str) {
        target
            .as_object_mut()
            .expect("fixture node is an object")
            .insert(key.to_string(), serde_json::json!("retires nothing"));
    }

    /// The control: refusing unknown fields must not have made the vocabulary
    /// itself unparseable. This is not ceremony — `deny_unknown_fields` on an
    /// internally tagged enum is exactly where serde can turn the *tag* into an
    /// unknown field, and a rule that rejects every plan would "pass" all four
    /// refusal tests below while breaking apply.
    #[test]
    fn every_action_in_the_vocabulary_still_parses() {
        let plan: BoundAliasPlan = serde_json::from_value(plan_json()).expect("fixture parses");
        assert_eq!(plan.actions.len(), 4, "one of every action");
        assert_eq!(plan.bindings.policy_revision, "ar1:00");
        // And the round trip the digest depends on survives it.
        let encoded = serde_json::to_value(&plan).expect("encode");
        let decoded: BoundAliasPlan = serde_json::from_value(encoded).expect("re-parse own output");
        assert_eq!(alias_plan_digest(&decoded), alias_plan_digest(&plan));
    }

    #[test]
    fn an_unknown_field_on_the_plan_is_refused_not_dropped() {
        let refusal = parse_mutated(|json| insert(json, "summary"))
            .expect_err("a summary beside the digest is the lie surface the plan has no field for");
        assert!(
            refusal.to_string().contains("summary"),
            "the refusal has to name the field: {refusal}"
        );
    }

    #[test]
    fn an_unknown_field_inside_the_bindings_is_refused() {
        parse_mutated(|json| insert(&mut json["bindings"], "note"))
            .expect_err("bindings are preconditions, not a free-text carrier");
        parse_mutated(|json| insert(&mut json["bindings"]["aliases"][0], "why"))
            .expect_err("an alias precondition carries a name and a revision, nothing else");
        parse_mutated(|json| insert(&mut json["bindings"]["deployments"][0], "why"))
            .expect_err("a deployment precondition carries an id and a revision, nothing else");
    }

    /// The one serde would not have given us for free: `AliasAction` is
    /// internally tagged, and internally tagged enums buffer their content
    /// before dispatching to the variant. If `deny_unknown_fields` did not
    /// survive that buffering, undigested prose would ride inside an action
    /// while every test above stayed green.
    #[test]
    fn an_unknown_field_inside_any_action_is_refused() {
        for index in 0..4 {
            let refusal = match parse_mutated(|json| insert(&mut json["actions"][index], "summary"))
            {
                Ok(plan) => panic!("action {index} accepted undigested content: {plan:?}"),
                Err(refusal) => refusal,
            };
            assert!(
                refusal.to_string().contains("summary"),
                "action {index} refusal has to name the field: {refusal}"
            );
        }
    }

    /// A field the plan *does* know, spelled for a variant that does not have
    /// it, is unknown content too — this is how a `priority` smuggled into a
    /// retirement would otherwise be silently dropped.
    #[test]
    fn a_field_from_another_variant_is_unknown_content_here() {
        parse_mutated(|json| {
            json["actions"][3]
                .as_object_mut()
                .expect("retire_alias is an object")
                .insert("priority".to_string(), serde_json::json!(9));
        })
        .expect_err("retire_alias has no priority; accepting one would digest a lie");
    }
}

// vault/apply.rs — the bound reconcile plan and its digest (tachi#1680 D4).
//
// # What a "bound" plan is
//
// A reconcile plan is not a list of intentions; it is a list of intentions
// **plus the exact state they were decided against**. Every account the plan
// touches carries the revision, `auth_ref` and policy ref it was planned
// against; every credential it reasoned about carries the `vault_entries`
// timestamp it was read at; every discovery source carries the SHA-256 of the
// bytes that were parsed. Apply re-reads all of it inside one write
// transaction and refuses the whole plan if any single binding moved — see
// [`crate::MemoryStore::apply_provider_account_plan`].
//
// That is why the bindings are a first-class part of this type rather than
// something the caller passes alongside: a plan without its preconditions is
// not applyable, and making that impossible to express is cheaper than
// remembering it.
//
// # Why the digest covers exactly this struct
//
// The plan travels through a file the operator can read, edit, and hand to
// `apply` minutes or days later. `pd1:<sha256>` over this struct's canonical
// JSON is what makes "the thing I approved is the thing that ran" checkable:
// apply recomputes it and refuses on any difference.
//
// Nothing else belongs in the digested surface, and — just as important —
// nothing that describes the plan to a human belongs *outside* it. A rendered
// summary stored next to the digest would be a lie surface: an attacker who
// edits only the prose leaves the digest valid while the operator approves a
// description of something the plan does not do. The artifact this type is
// written into therefore stores no prose at all; `plan` and `apply` both
// render their human view from these bound actions.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::accounts::{AccountClass, AuthMode, CustodyKind, NewProviderAccount};

/// Scheme prefix of a plan digest: `pd1:<hex64>`. Versioned in the value so a
/// later canonicalization change is visibly a different scheme rather than a
/// silently different hash of the same shape.
pub const PLAN_DIGEST_SCHEME: &str = "pd1";

/// One reconcile plan, with the preconditions it was decided against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundAccountPlan {
    pub bindings: PlanBindings,
    pub actions: Vec<AccountAction>,
}

/// Everything apply re-reads before it writes anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanBindings {
    /// Discovery sources (config/env files) and the digest of the bytes the
    /// plan was built from.
    #[serde(default)]
    pub sources: Vec<SourceBinding>,
    /// Vault entries whose metadata the plan depended on.
    #[serde(default)]
    pub vault_entries: Vec<VaultEntryBinding>,
    /// Rotation pools whose membership the plan depended on.
    #[serde(default)]
    pub vault_pools: Vec<VaultPoolBinding>,
    /// Accounts the plan's actions touch.
    #[serde(default)]
    pub accounts: Vec<AccountBinding>,
    /// Custody rows the plan's actions repoint.
    #[serde(default)]
    pub custody: Vec<CustodyBinding>,
}

/// A discovery source and the SHA-256 of its bytes at plan time.
///
/// `source_id` is opaque to this crate: the reconcile pipeline owns the
/// descriptor grammar, and re-reading the source is the caller's job through
/// [`PlanSourceDigests`] — memcore is a storage leaf and does not touch the
/// filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceBinding {
    pub source_id: String,
    pub sha256: String,
}

/// A `vault_entries` row and the `updated_at` the plan read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultEntryBinding {
    pub entry_name: String,
    /// `None` binds **absence**: the plan was decided on the fact that this
    /// name held no Vault entry, and an entry appearing under it since is
    /// drift exactly as a changed timestamp is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// A rotation pool and a digest of the membership the plan read.
///
/// # Why a digest and not a list of member timestamps
///
/// The obvious binding — one [`VaultEntryBinding`] per pool member — would put
/// `DEEPSEEK_API_KEY_2` in the plan file, and member indices are exactly the
/// custody layout `auth_ref` exists to keep off account surfaces (#1680 D5). A
/// plan is an account surface: it is written to disk, read by an operator, and
/// quite possibly pasted into a ticket.
///
/// So the plan carries the pool *prefix*, which is the same logical name the
/// account's aliases already carry and leaks nothing, plus an opaque digest
/// over the members' names and timestamps. Apply recomputes it from
/// `vault_entries` inside its transaction. The digest is also a strictly
/// stronger precondition than a list of timestamps would have been: a member
/// added or removed changes it, and a per-name list could not have noticed a
/// member that was not in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultPoolBinding {
    pub prefix: String,
    pub members_digest: String,
}

/// An existing account and the state the plan was decided against.
///
/// `credential_policy_ref` is the policy binding (#1680 D4's "policy
/// revision"): the ref string carries its own version, and this slice has no
/// separate policy table to read a revision out of, so the bound precondition
/// is the exact ref the account carried at plan time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBinding {
    pub account_id: String,
    pub revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_policy_ref: Option<String>,
}

/// A custody row and the revision the plan was decided against. Carries no
/// custody target: the plan is a public surface and the target is Vault layout
/// (#1680 D5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyBinding {
    pub auth_ref: String,
    pub revision: i64,
}

/// One name an account was observed under, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasSighting {
    pub alias_name: String,
    pub source_kind: String,
}

/// An account the plan proposes to create.
///
/// `account_id` and `auth_ref` are minted at **plan** time, not apply time, so
/// that replaying the same plan is decidable by identity rather than by
/// guessing from evidence: apply looks the id up and, finding the account
/// already there in the same shape, does nothing.
///
/// `custody_logical_name` is the Vault entry name or rotation-pool prefix —
/// the same logical name that appears in `aliases`, never a pool member
/// (`…_API_KEY_2`). The member index is the layout `auth_ref` exists to hide,
/// and `db::vault_accounts` refuses it at the store door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAccount {
    pub account_id: String,
    pub provider_kind: String,
    pub auth_mode: AuthMode,
    pub account_class: AccountClass,
    pub account_fingerprint: String,
    pub auth_ref: String,
    pub custody_kind: CustodyKind,
    pub custody_logical_name: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_policy_ref: Option<String>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<AliasSighting>,
    /// Public-safe JSON object recorded on the `account_created` follow-up
    /// event: fingerprints, counts, source ids. Never key material.
    #[serde(default = "empty_evidence")]
    pub evidence: String,
}

impl PlannedAccount {
    /// The store-layer row this action creates. Kept as a conversion rather
    /// than storing a [`NewProviderAccount`] directly because a plan must be
    /// serializable and `NewProviderAccount` deliberately is not: it is the
    /// store's input type, not a wire type.
    pub fn to_new_account(&self) -> NewProviderAccount {
        NewProviderAccount {
            account_id: self.account_id.clone(),
            provider_kind: self.provider_kind.clone(),
            auth_mode: self.auth_mode,
            auth_ref: Some(self.auth_ref.clone()),
            account_fingerprint: self.account_fingerprint.clone(),
            account_class: self.account_class,
            capabilities: self.capabilities.clone(),
            credential_policy_ref: self.credential_policy_ref.clone(),
            refresh_authority: super::accounts::REFRESH_AUTHORITY_NONE.to_string(),
            status: super::accounts::ACCOUNT_STATUS_ACTIVE.to_string(),
            source_refs: self.source_refs.clone(),
        }
    }
}

/// An operator's explicit go-ahead for collapsing two account identities.
///
/// A bare boolean would have been enough for the machine and useless for the
/// audit: the merge event records who confirmed and when, because a merge is
/// the one action in this vocabulary that destroys a distinction the evidence
/// could not decide on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeConfirmation {
    pub confirmed_by: String,
    pub confirmed_at: String,
}

/// The closed action vocabulary of a reconcile plan.
///
/// Note what is **absent**: there is no "import this value into the Vault" and
/// no "delete". Apply writes the four provider-account tables and nothing else
/// — a credential never moves, never gets copied, and never gets erased by a
/// reconcile pass. Bringing a new secret under Vault custody stays an explicit,
/// separate operator action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AccountAction {
    /// Create an account, its custody row, and its first alias sightings.
    CreateAccount { account: Box<PlannedAccount> },
    /// Record that an existing account was seen under a name.
    ObserveAlias {
        account_id: String,
        alias_name: String,
        source_kind: String,
    },
    /// Stop claiming an account is reachable under a name. The row stays.
    RetireAlias {
        account_id: String,
        alias_name: String,
    },
    /// Record a new account fingerprint (a member rotated, was added, or was
    /// dropped). This is the action that advances an account's revision.
    RecordFingerprint {
        account_id: String,
        account_fingerprint: String,
        event_kind: String,
        #[serde(default = "empty_evidence")]
        evidence: String,
    },
    /// Repoint custody at a different Vault object, keeping `auth_ref` — and
    /// therefore every upper-layer reference — unchanged.
    RepointCustody {
        auth_ref: String,
        custody_kind: CustodyKind,
        custody_logical_name: String,
    },
    /// Collapse `from_account_id` into `into_account_id`: the source's active
    /// aliases move to the target and the source retires.
    ///
    /// `confirmation` is `Option` and not `bool` for a reason that is the
    /// whole point of this variant: a plan generated from evidence always
    /// leaves it `None`, and apply refuses `None`
    /// ([`crate::error::ProviderPlanRefusal::UnconfirmedMerge`]). Alias-family
    /// similarity — two env-var names that the registry says belong to one
    /// vendor family — is a *hint*, never authority to fuse two account
    /// identities; only an operator supplies the missing half.
    MergeAccounts {
        from_account_id: String,
        into_account_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confirmation: Option<MergeConfirmation>,
    },
}

impl AccountAction {
    /// Every account id this action touches. Apply requires each of them to
    /// carry a binding (or to be created by this same plan) before it writes
    /// anything.
    pub fn account_ids(&self) -> Vec<&str> {
        match self {
            Self::CreateAccount { account } => vec![account.account_id.as_str()],
            Self::ObserveAlias { account_id, .. }
            | Self::RetireAlias { account_id, .. }
            | Self::RecordFingerprint { account_id, .. } => vec![account_id.as_str()],
            Self::RepointCustody { .. } => Vec::new(),
            Self::MergeAccounts {
                from_account_id,
                into_account_id,
                ..
            } => vec![from_account_id.as_str(), into_account_id.as_str()],
        }
    }
}

fn empty_evidence() -> String {
    "{}".to_string()
}

/// `pd1:<hex64>` over the plan's canonical JSON.
///
/// Canonical here means "deterministic", which is what a digest needs and all
/// it needs: the plan is built entirely out of structs and `Vec`s, whose serde
/// encoding is field-declaration order with no map to sort, so the same plan
/// value always produces the same bytes — including after a round trip through
/// the artifact file, which is the trip the digest exists to protect.
///
/// Adding a field to any of these types therefore changes the digest of an
/// otherwise identical plan. That is deliberate: a plan produced by a build
/// that knew about a precondition must not be applyable by one that would
/// silently ignore it.
pub fn plan_digest(plan: &BoundAccountPlan) -> String {
    let encoded = serde_json::to_vec(plan).expect("BoundAccountPlan is JSON-encodable by shape");
    let mut hasher = Sha256::new();
    hasher.update(&encoded);
    format!("{PLAN_DIGEST_SCHEME}:{:x}", hasher.finalize())
}

/// Re-reads a plan's discovery sources at apply time.
///
/// Implemented by the reconcile pipeline, called by apply **inside** its write
/// transaction. The indirection is a layering rule, not ceremony: memcore is a
/// storage leaf with no filesystem surface, and a plan's sources are files. It
/// is also what keeps the source check honest — re-reading the file before
/// opening the transaction would leave a window in which it changes, which is
/// exactly the drift the binding exists to catch.
pub trait PlanSourceDigests {
    /// The current SHA-256 of `source_id`, or `None` if the source no longer
    /// exists. An `Err` is a refusal (unreadable evidence is not "unchanged"),
    /// never a panic.
    fn current_digest(&self, source_id: &str) -> Result<Option<String>, String>;
}

/// A source set that answers "gone" for everything. Useful for plans with no
/// file-backed sources, and as the honest default in tests that are not about
/// source drift.
pub struct NoPlanSources;

impl PlanSourceDigests for NoPlanSources {
    fn current_digest(&self, _source_id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan() -> BoundAccountPlan {
        BoundAccountPlan {
            bindings: PlanBindings {
                sources: vec![SourceBinding {
                    source_id: "env_file:/home/x/.tachi/config.env".to_string(),
                    sha256: "a".repeat(64),
                }],
                vault_entries: vec![VaultEntryBinding {
                    entry_name: "DEEPSEEK_API_KEY".to_string(),
                    updated_at: Some("2026-08-01T00:00:00Z".to_string()),
                }],
                accounts: Vec::new(),
                custody: Vec::new(),
            },
            actions: vec![AccountAction::ObserveAlias {
                account_id: "01ACCOUNT".to_string(),
                alias_name: "DEEPSEEK_API_KEY".to_string(),
                source_kind: "vault_entry".to_string(),
            }],
        }
    }

    /// The digest is a pure function of the plan value, stable across a round
    /// trip through the artifact file. Without this, an operator-approved plan
    /// would fail to apply for no reason other than serde ordering.
    #[test]
    fn digest_survives_a_json_round_trip() {
        let plan = sample_plan();
        let before = plan_digest(&plan);
        let text = serde_json::to_string(&plan).expect("encode");
        let parsed: BoundAccountPlan = serde_json::from_str(&text).expect("decode");
        assert_eq!(parsed, plan);
        assert_eq!(plan_digest(&parsed), before);
        assert!(before.starts_with("pd1:"), "scheme prefix: {before}");
        assert_eq!(before.len(), "pd1:".len() + 64);
    }

    /// Every part of the bound content is inside the digest — an edited action
    /// and an edited binding must both be detectable.
    #[test]
    fn digest_changes_when_any_bound_field_changes() {
        let base = plan_digest(&sample_plan());

        let mut edited_action = sample_plan();
        if let AccountAction::ObserveAlias { alias_name, .. } = &mut edited_action.actions[0] {
            *alias_name = "DISTILL_API_KEY".to_string();
        }
        assert_ne!(plan_digest(&edited_action), base, "action is digested");

        let mut edited_source = sample_plan();
        edited_source.bindings.sources[0].sha256 = "b".repeat(64);
        assert_ne!(plan_digest(&edited_source), base, "source is digested");

        let mut edited_entry = sample_plan();
        edited_entry.bindings.vault_entries[0].updated_at = None;
        assert_ne!(
            plan_digest(&edited_entry),
            base,
            "vault binding is digested"
        );
    }

    /// A merge action generated from evidence carries no confirmation, and the
    /// serialized form says so by omission rather than by a `null` a hand edit
    /// could quietly turn into an object.
    #[test]
    fn an_unconfirmed_merge_serializes_without_a_confirmation_field() {
        let action = AccountAction::MergeAccounts {
            from_account_id: "01A".to_string(),
            into_account_id: "01B".to_string(),
            confirmation: None,
        };
        let text = serde_json::to_string(&action).expect("encode");
        assert!(
            !text.contains("confirmation"),
            "unconfirmed merge must omit the field: {text}"
        );
        assert!(text.contains("\"action\":\"merge_accounts\""), "{text}");
    }
}

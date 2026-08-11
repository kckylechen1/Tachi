//! The bound apply transaction for provider-account reconcile plans
//! (tachi#1680 D4).
//!
//! # Why this is a store-layer write and not a CLI composition
//!
//! Everything reconcile does before this point is read-only evidence
//! collection, and it would have been tempting to build apply the same way:
//! read the state, decide, then call the existing single-purpose accessors one
//! after another. That is exactly the shape #1680's cross-vendor review
//! rejected, for two reasons that are both about who is holding what while the
//! writes happen.
//!
//! 1. The daemon's write guard is an **in-process** lock. A `tachi vault
//!    reconcile apply` run from a shell is a different process, so the guard
//!    does not exist for it. The only mutual exclusion that spans processes is
//!    SQLite's own, which means apply must *be* one transaction rather than a
//!    sequence of them.
//! 2. Preconditions verified before a transaction are preconditions verified
//!    at a moment that is already over. Re-reading them inside the write
//!    transaction — after `BEGIN IMMEDIATE` has taken the write lock — is what
//!    makes "drift yields zero writes" true rather than likely.
//!
//! So: one `BEGIN IMMEDIATE`, every binding re-read under it, every write
//! after every check, and a rollback on any mismatch. A refusal leaves the
//! database byte-for-byte as it was.
//!
//! # Ordering
//!
//! All verification happens before any write, and the actions then run in plan
//! order. That ordering is load-bearing for merges: the source account's
//! aliases must land on the target before the source retires, so a crash-free
//! reader never sees a name belonging to nobody.

use std::collections::{BTreeSet, HashSet};

use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::db::vault_accounts::{
    append_provider_account_event, get_account_custody_by_auth_ref, get_provider_account,
    insert_account_custody, insert_provider_account, list_provider_account_aliases,
    record_account_fingerprint, record_provider_account_alias, retire_provider_account,
    retire_provider_account_alias, update_custody_target, vault_pool_members_digest,
    AccountRetirement, AliasObservation, FingerprintUpdate,
};
use crate::error::{MemoryError, ProviderPlanRefusal};
use crate::vault::accounts::{
    NewProviderAccountEvent, EVENT_KIND_ACCOUNT_MERGED, EVENT_KIND_ALIAS_OBSERVED,
    EVENT_KIND_ALIAS_RETIRED, EVENT_KIND_PLAN_NOOP,
};
use crate::vault::apply::{
    plan_digest, AccountAction, BoundAccountPlan, PlanSourceDigests, PlannedAccount,
};
use crate::MemoryStore;

/// One account/alias pair named in an apply report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AliasRef {
    pub account_id: String,
    pub alias_name: String,
}

/// An account and the revision it now sits at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountRevision {
    pub account_id: String,
    pub revision: i64,
}

/// What one apply actually did.
///
/// Secret-negative and layout-negative by construction: ids, names and
/// revisions only. `custody_repointed` names `auth_ref`s, never targets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AccountApplyReport {
    pub plan_digest: String,
    /// Whether anything about the recorded state changed. `false` is the
    /// idempotent-replay outcome: the plan was verified in full, every action
    /// found the world already matching it, and every account revision is
    /// exactly where it was.
    pub changed: bool,
    pub accounts_created: Vec<String>,
    pub accounts_retired: Vec<AccountRevision>,
    pub aliases_created: Vec<AliasRef>,
    pub aliases_revived: Vec<AliasRef>,
    pub aliases_retired: Vec<AliasRef>,
    pub fingerprints_advanced: Vec<AccountRevision>,
    pub custody_repointed: Vec<String>,
    /// `plan_noop` event ids, one per account the plan referenced, written
    /// only when the whole apply changed nothing.
    pub noop_event_ids: Vec<i64>,
}

impl MemoryStore {
    /// Apply a bound reconcile plan, or refuse it and write nothing.
    ///
    /// `declared_digest` is the digest the plan was *presented* under — what
    /// the artifact file says, and what the operator approved. It is compared
    /// against the digest recomputed from the plan's own content, which is
    /// what catches both halves of a tampered artifact: edit an action and the
    /// recomputed digest moves, edit the digest and the declared one moves.
    ///
    /// `sources` re-reads the plan's file-backed evidence from inside the
    /// transaction (see [`PlanSourceDigests`]).
    ///
    /// # Errors
    ///
    /// [`MemoryError::ProviderAccountPlanRefused`] for every drift and every
    /// malformed plan — in all cases nothing was written. Other variants are
    /// genuine storage failures, which roll back for the same reason.
    pub fn apply_provider_account_plan(
        &mut self,
        plan: &BoundAccountPlan,
        declared_digest: &str,
        sources: &dyn PlanSourceDigests,
    ) -> Result<AccountApplyReport, MemoryError> {
        // BEGIN IMMEDIATE, not the default DEFERRED: the write lock must be
        // held *before* the preconditions are re-read, or the re-read is just
        // an earlier read with a shorter window.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let report = apply_within(&tx, plan, declared_digest, sources)?;
        tx.commit()?;
        Ok(report)
    }
}

fn refuse<T>(reason: ProviderPlanRefusal, detail: impl Into<String>) -> Result<T, MemoryError> {
    Err(MemoryError::ProviderAccountPlanRefused {
        reason,
        detail: detail.into(),
    })
}

fn apply_within(
    tx: &Transaction<'_>,
    plan: &BoundAccountPlan,
    declared_digest: &str,
    sources: &dyn PlanSourceDigests,
) -> Result<AccountApplyReport, MemoryError> {
    let recomputed = plan_digest(plan);
    if recomputed != declared_digest {
        return refuse(
            ProviderPlanRefusal::PlanDigestMismatch,
            format!("plan was presented as '{declared_digest}' but hashes to '{recomputed}'"),
        );
    }

    verify_plan_shape(plan)?;
    verify_sources(plan, sources)?;
    verify_vault_entries(tx, plan)?;
    verify_vault_pools(tx, plan)?;
    verify_accounts(tx, plan)?;
    verify_custody(tx, plan)?;

    let mut report = AccountApplyReport {
        plan_digest: recomputed,
        ..Default::default()
    };
    for action in &plan.actions {
        run_action(tx, plan, action, &mut report)?;
    }

    let untouched = report.accounts_created.is_empty()
        && report.accounts_retired.is_empty()
        && report.aliases_created.is_empty()
        && report.aliases_revived.is_empty()
        && report.aliases_retired.is_empty()
        && report.fingerprints_advanced.is_empty()
        && report.custody_repointed.is_empty();
    report.changed = !untouched;

    if untouched {
        report.noop_event_ids = record_replay_noop(tx, plan, &report.plan_digest)?;
    }
    Ok(report)
}

// ── Verification ───────────────────────────────────────────────────────────

/// Structural checks that need no database at all: identifiers present,
/// bindings not self-contradictory, evidence well formed, every touched
/// account bound, every merge confirmed.
fn verify_plan_shape(plan: &BoundAccountPlan) -> Result<(), MemoryError> {
    let mut bound: HashSet<&str> = HashSet::new();
    for binding in &plan.bindings.accounts {
        if binding.account_id.trim().is_empty() {
            return refuse(
                ProviderPlanRefusal::MalformedPlan,
                "an account binding carries an empty account_id",
            );
        }
        if !bound.insert(binding.account_id.as_str()) {
            return refuse(
                ProviderPlanRefusal::MalformedPlan,
                format!("account '{}' is bound twice", binding.account_id),
            );
        }
    }

    let mut bound_custody: HashSet<&str> = HashSet::new();
    for binding in &plan.bindings.custody {
        if !bound_custody.insert(binding.auth_ref.as_str()) {
            return refuse(
                ProviderPlanRefusal::MalformedPlan,
                format!("auth_ref '{}' is bound twice", binding.auth_ref),
            );
        }
    }

    let mut bound_sources: HashSet<&str> = HashSet::new();
    for binding in &plan.bindings.sources {
        if !bound_sources.insert(binding.source_id.as_str()) {
            return refuse(
                ProviderPlanRefusal::MalformedPlan,
                format!("source '{}' is bound twice", binding.source_id),
            );
        }
    }

    let mut created: HashSet<&str> = HashSet::new();
    for action in &plan.actions {
        match action {
            AccountAction::CreateAccount { account } => {
                verify_planned_account(account)?;
                if !created.insert(account.account_id.as_str()) {
                    return refuse(
                        ProviderPlanRefusal::MalformedPlan,
                        format!("account '{}' is created twice", account.account_id),
                    );
                }
            }
            AccountAction::ObserveAlias {
                alias_name,
                source_kind,
                ..
            } => {
                if alias_name.trim().is_empty() || source_kind.trim().is_empty() {
                    return refuse(
                        ProviderPlanRefusal::MalformedPlan,
                        "an observe_alias action carries an empty alias_name or source_kind",
                    );
                }
            }
            AccountAction::RetireAlias { alias_name, .. } => {
                if alias_name.trim().is_empty() {
                    return refuse(
                        ProviderPlanRefusal::MalformedPlan,
                        "a retire_alias action carries an empty alias_name",
                    );
                }
            }
            AccountAction::RecordFingerprint {
                account_fingerprint,
                event_kind,
                evidence,
                ..
            } => {
                if account_fingerprint.trim().is_empty() || event_kind.trim().is_empty() {
                    return refuse(
                        ProviderPlanRefusal::MalformedPlan,
                        "a record_fingerprint action carries an empty fingerprint or event_kind",
                    );
                }
                verify_evidence(evidence)?;
            }
            AccountAction::RepointCustody {
                auth_ref,
                custody_logical_name,
                ..
            } => {
                if auth_ref.trim().is_empty() || custody_logical_name.trim().is_empty() {
                    return refuse(
                        ProviderPlanRefusal::MalformedPlan,
                        "a repoint_custody action carries an empty auth_ref or logical name",
                    );
                }
                if !bound_custody.contains(auth_ref.as_str()) {
                    return refuse(
                        ProviderPlanRefusal::UnboundAccount,
                        format!(
                            "repoint_custody targets auth_ref '{auth_ref}', which the plan binds \
                             no custody revision for"
                        ),
                    );
                }
            }
            // The one action that cannot be derived from evidence alone.
            // Alias-family similarity proposes; only an operator disposes.
            AccountAction::MergeAccounts {
                from_account_id,
                into_account_id,
                confirmation,
            } => {
                if from_account_id == into_account_id {
                    return refuse(
                        ProviderPlanRefusal::MalformedPlan,
                        format!("merge action names '{from_account_id}' as both sides"),
                    );
                }
                let confirmed = confirmation
                    .as_ref()
                    .is_some_and(|c| !c.confirmed_by.trim().is_empty());
                if !confirmed {
                    return refuse(
                        ProviderPlanRefusal::UnconfirmedMerge,
                        format!(
                            "merging account '{from_account_id}' into '{into_account_id}' needs \
                             an explicit operator confirmation; alias-family similarity is \
                             advisory evidence and never authority to fuse two identities"
                        ),
                    );
                }
            }
        }

        for account_id in action.account_ids() {
            if !bound.contains(account_id) && !created.contains(account_id) {
                return refuse(
                    ProviderPlanRefusal::UnboundAccount,
                    format!(
                        "action targets account '{account_id}', for which the plan binds no \
                         precondition"
                    ),
                );
            }
        }
    }
    Ok(())
}

fn verify_planned_account(account: &PlannedAccount) -> Result<(), MemoryError> {
    if account.account_id.trim().is_empty()
        || account.auth_ref.trim().is_empty()
        || account.provider_kind.trim().is_empty()
        || account.account_fingerprint.trim().is_empty()
        || account.custody_logical_name.trim().is_empty()
    {
        return refuse(
            ProviderPlanRefusal::MalformedPlan,
            "a create_account action leaves a required identifier empty",
        );
    }
    for alias in &account.aliases {
        if alias.alias_name.trim().is_empty() || alias.source_kind.trim().is_empty() {
            return refuse(
                ProviderPlanRefusal::MalformedPlan,
                format!(
                    "create_account for '{}' carries an alias with an empty name or source_kind",
                    account.account_id
                ),
            );
        }
    }
    verify_evidence(&account.evidence)
}

/// Evidence lands verbatim in a TEXT column that every later reader parses as
/// JSON. A plan that puts something else there is malformed now rather than a
/// parse failure for whoever reads the audit log next year.
fn verify_evidence(evidence: &str) -> Result<(), MemoryError> {
    match serde_json::from_str::<serde_json::Value>(evidence) {
        Ok(serde_json::Value::Object(_)) => Ok(()),
        _ => refuse(
            ProviderPlanRefusal::MalformedPlan,
            "event evidence must be a JSON object",
        ),
    }
}

fn verify_sources(
    plan: &BoundAccountPlan,
    sources: &dyn PlanSourceDigests,
) -> Result<(), MemoryError> {
    for binding in &plan.bindings.sources {
        match sources.current_digest(&binding.source_id) {
            Ok(Some(current)) if current == binding.sha256 => {}
            Ok(Some(_)) => {
                return refuse(
                    ProviderPlanRefusal::SourceDigestMismatch,
                    format!(
                        "source '{}' changed since the plan read it",
                        binding.source_id
                    ),
                )
            }
            Ok(None) => {
                return refuse(
                    ProviderPlanRefusal::SourceDigestMismatch,
                    format!(
                        "source '{}' no longer exists; the plan's evidence is gone",
                        binding.source_id
                    ),
                )
            }
            Err(err) => {
                return refuse(
                    ProviderPlanRefusal::SourceDigestMismatch,
                    format!("source '{}' could not be re-read: {err}", binding.source_id),
                )
            }
        }
    }
    Ok(())
}

fn verify_vault_entries(tx: &Transaction<'_>, plan: &BoundAccountPlan) -> Result<(), MemoryError> {
    for binding in &plan.bindings.vault_entries {
        let current: Option<String> = tx
            .query_row(
                "SELECT updated_at FROM vault_entries WHERE name = ?1",
                params![binding.entry_name],
                |row| row.get(0),
            )
            .optional()?;
        match (&binding.updated_at, current) {
            (Some(expected), Some(actual)) if *expected == actual => {}
            (None, None) => {}
            (Some(_), Some(_)) => {
                return refuse(
                    ProviderPlanRefusal::VaultEntryDrift,
                    format!(
                        "vault entry '{}' was written since the plan read it",
                        binding.entry_name
                    ),
                )
            }
            (Some(_), None) => {
                return refuse(
                    ProviderPlanRefusal::VaultEntryDrift,
                    format!("vault entry '{}' no longer exists", binding.entry_name),
                )
            }
            (None, Some(_)) => {
                return refuse(
                    ProviderPlanRefusal::VaultEntryDrift,
                    format!(
                        "vault entry '{}' now exists; the plan was decided on its absence",
                        binding.entry_name
                    ),
                )
            }
        }
    }
    Ok(())
}

fn verify_vault_pools(tx: &Transaction<'_>, plan: &BoundAccountPlan) -> Result<(), MemoryError> {
    for binding in &plan.bindings.vault_pools {
        let current = vault_pool_members_digest(tx, &binding.prefix)?;
        if current != binding.members_digest {
            return refuse(
                ProviderPlanRefusal::VaultEntryDrift,
                format!(
                    "rotation pool '{}' has different membership than the plan read \
                     (a member was added, removed, or rewritten)",
                    binding.prefix
                ),
            );
        }
    }
    Ok(())
}

fn verify_accounts(tx: &Transaction<'_>, plan: &BoundAccountPlan) -> Result<(), MemoryError> {
    for binding in &plan.bindings.accounts {
        let Some(account) = get_provider_account(tx, &binding.account_id)? else {
            return refuse(
                ProviderPlanRefusal::UnknownAccount,
                format!("bound account '{}' does not exist", binding.account_id),
            );
        };
        if account.revision != binding.revision {
            return refuse(
                ProviderPlanRefusal::AccountRevisionDrift,
                format!(
                    "account '{}' is at revision {} but the plan was built against {}",
                    binding.account_id, account.revision, binding.revision
                ),
            );
        }
        if account.auth_ref != binding.auth_ref {
            return refuse(
                ProviderPlanRefusal::AuthRefDrift,
                format!(
                    "account '{}' no longer carries the auth_ref the plan bound",
                    binding.account_id
                ),
            );
        }
        if account.credential_policy_ref != binding.credential_policy_ref {
            return refuse(
                ProviderPlanRefusal::PolicyRefDrift,
                format!(
                    "account '{}' now carries credential policy {:?}, not the {:?} the plan was \
                     authorized under",
                    binding.account_id,
                    account.credential_policy_ref,
                    binding.credential_policy_ref
                ),
            );
        }
    }
    Ok(())
}

fn verify_custody(tx: &Transaction<'_>, plan: &BoundAccountPlan) -> Result<(), MemoryError> {
    for binding in &plan.bindings.custody {
        let Some(custody) = get_account_custody_by_auth_ref(tx, &binding.auth_ref)? else {
            return refuse(
                ProviderPlanRefusal::CustodyRevisionDrift,
                format!(
                    "custody for auth_ref '{}' no longer exists",
                    binding.auth_ref
                ),
            );
        };
        if custody.revision != binding.revision {
            return refuse(
                ProviderPlanRefusal::CustodyRevisionDrift,
                format!(
                    "custody for auth_ref '{}' is at revision {} but the plan bound {}",
                    binding.auth_ref, custody.revision, binding.revision
                ),
            );
        }
    }
    Ok(())
}

// ── Execution ──────────────────────────────────────────────────────────────

fn run_action(
    tx: &Transaction<'_>,
    plan: &BoundAccountPlan,
    action: &AccountAction,
    report: &mut AccountApplyReport,
) -> Result<(), MemoryError> {
    let digest = report.plan_digest.clone();
    match action {
        AccountAction::CreateAccount { account } => {
            create_account(tx, account, &digest, report)?;
        }
        AccountAction::ObserveAlias {
            account_id,
            alias_name,
            source_kind,
        } => {
            observe_alias(tx, account_id, alias_name, source_kind, &digest, report)?;
        }
        AccountAction::RetireAlias {
            account_id,
            alias_name,
        } => {
            retire_alias(tx, account_id, alias_name, &digest, report)?;
        }
        AccountAction::RecordFingerprint {
            account_id,
            account_fingerprint,
            event_kind,
            evidence,
        } => {
            // `expected_revision` is deliberately `None`: the binding was
            // verified at the top of this transaction under the write lock, and
            // passing it again would break the moment one plan advances the
            // same account twice.
            let update = record_account_fingerprint(
                tx,
                account_id,
                None,
                account_fingerprint,
                event_kind,
                Some(&digest),
                evidence,
            )?;
            if let FingerprintUpdate::Advanced { revision, .. } = update {
                report.fingerprints_advanced.push(AccountRevision {
                    account_id: account_id.clone(),
                    revision,
                });
            }
        }
        AccountAction::RepointCustody {
            auth_ref,
            custody_kind,
            custody_logical_name,
        } => {
            let before = get_account_custody_by_auth_ref(tx, auth_ref)?;
            let after =
                update_custody_target(tx, auth_ref, None, *custody_kind, custody_logical_name)?;
            let moved = match (before, after) {
                (Some(before), Some(after)) => before.revision != after.revision,
                _ => false,
            };
            if moved {
                report.custody_repointed.push(auth_ref.clone());
            }
        }
        AccountAction::MergeAccounts {
            from_account_id,
            into_account_id,
            confirmation,
        } => {
            merge_accounts(
                tx,
                plan,
                from_account_id,
                into_account_id,
                confirmation
                    .as_ref()
                    .map(|c| c.confirmed_by.as_str())
                    .unwrap_or_default(),
                &digest,
                report,
            )?;
        }
    }
    Ok(())
}

fn create_account(
    tx: &Transaction<'_>,
    account: &PlannedAccount,
    digest: &str,
    report: &mut AccountApplyReport,
) -> Result<(), MemoryError> {
    // Replay is decided by identity, not by evidence: a plan that already ran
    // finds its own account_id and does nothing. A row under that id in a
    // different shape is a plan collision, not a replay, and is refused.
    if let Some(existing) = get_provider_account(tx, &account.account_id)? {
        if existing.provider_kind != account.provider_kind
            || existing.account_fingerprint != account.account_fingerprint
            || existing.auth_ref.as_deref() != Some(account.auth_ref.as_str())
        {
            return refuse(
                ProviderPlanRefusal::MalformedPlan,
                format!(
                    "account '{}' already exists in a different shape than this plan creates",
                    account.account_id
                ),
            );
        }
    } else {
        insert_provider_account(tx, &account.to_new_account())?;
        insert_account_custody(
            tx,
            &account.auth_ref,
            &account.account_id,
            account.custody_kind,
            &account.custody_logical_name,
        )?;
        if account.evidence != "{}" {
            append_provider_account_event(
                tx,
                &NewProviderAccountEvent::new(
                    account.account_id.as_str(),
                    1,
                    crate::vault::accounts::EVENT_KIND_FINGERPRINT_OBSERVED,
                )
                .with_plan_digest(digest)
                .with_evidence(account.evidence.as_str()),
            )?;
        }
        report.accounts_created.push(account.account_id.clone());
    }

    for alias in &account.aliases {
        observe_alias(
            tx,
            &account.account_id,
            &alias.alias_name,
            &alias.source_kind,
            digest,
            report,
        )?;
    }
    Ok(())
}

fn observe_alias(
    tx: &Transaction<'_>,
    account_id: &str,
    alias_name: &str,
    source_kind: &str,
    digest: &str,
    report: &mut AccountApplyReport,
) -> Result<(), MemoryError> {
    let observation = record_provider_account_alias(tx, account_id, alias_name, source_kind)?;
    // `Refreshed` moves `last_seen` and nothing else. It is a sighting, not a
    // state change, so it neither counts as a change nor earns an event —
    // otherwise every replay would look like it did something.
    let bucket = match observation {
        AliasObservation::Created => Some(&mut report.aliases_created),
        AliasObservation::Revived => Some(&mut report.aliases_revived),
        AliasObservation::Refreshed => None,
    };
    if let Some(bucket) = bucket {
        bucket.push(AliasRef {
            account_id: account_id.to_string(),
            alias_name: alias_name.to_string(),
        });
        let revision = current_revision(tx, account_id)?;
        append_provider_account_event(
            tx,
            &NewProviderAccountEvent::new(account_id, revision, EVENT_KIND_ALIAS_OBSERVED)
                .with_plan_digest(digest)
                .with_evidence(alias_evidence(alias_name, source_kind)),
        )?;
    }
    Ok(())
}

fn retire_alias(
    tx: &Transaction<'_>,
    account_id: &str,
    alias_name: &str,
    digest: &str,
    report: &mut AccountApplyReport,
) -> Result<(), MemoryError> {
    if !retire_provider_account_alias(tx, account_id, alias_name)? {
        return Ok(());
    }
    report.aliases_retired.push(AliasRef {
        account_id: account_id.to_string(),
        alias_name: alias_name.to_string(),
    });
    let revision = current_revision(tx, account_id)?;
    append_provider_account_event(
        tx,
        &NewProviderAccountEvent::new(account_id, revision, EVENT_KIND_ALIAS_RETIRED)
            .with_plan_digest(digest)
            .with_evidence(alias_evidence(alias_name, "retired")),
    )?;
    Ok(())
}

fn merge_accounts(
    tx: &Transaction<'_>,
    plan: &BoundAccountPlan,
    from_account_id: &str,
    into_account_id: &str,
    confirmed_by: &str,
    digest: &str,
    report: &mut AccountApplyReport,
) -> Result<(), MemoryError> {
    if get_provider_account(tx, into_account_id)?.is_none() {
        let created_here = plan.actions.iter().any(|action| match action {
            AccountAction::CreateAccount { account } => account.account_id == into_account_id,
            _ => false,
        });
        if !created_here {
            return refuse(
                ProviderPlanRefusal::UnknownAccount,
                format!("merge target '{into_account_id}' does not exist"),
            );
        }
    }
    let Some(source) = get_provider_account(tx, from_account_id)? else {
        return refuse(
            ProviderPlanRefusal::UnknownAccount,
            format!("merge source '{from_account_id}' does not exist"),
        );
    };
    if source.status == crate::vault::accounts::ACCOUNT_STATUS_RETIRED {
        // Already merged away by an earlier run of this same plan.
        return Ok(());
    }

    // Aliases move first: a name must never be reachable from neither side.
    let mut moved: BTreeSet<String> = BTreeSet::new();
    for alias in list_provider_account_aliases(tx, from_account_id)? {
        if alias.retired {
            continue;
        }
        observe_alias(
            tx,
            into_account_id,
            &alias.alias_name,
            &alias.source_kind,
            digest,
            report,
        )?;
        retire_alias(tx, from_account_id, &alias.alias_name, digest, report)?;
        moved.insert(alias.alias_name);
    }

    let evidence = merge_evidence(from_account_id, into_account_id, confirmed_by, &moved);
    let retirement = retire_provider_account(
        tx,
        from_account_id,
        None,
        EVENT_KIND_ACCOUNT_MERGED,
        Some(digest),
        &evidence,
    )?;
    if let AccountRetirement::Retired { revision, .. } = retirement {
        report.accounts_retired.push(AccountRevision {
            account_id: from_account_id.to_string(),
            revision,
        });
    }

    // The absorbing side gets its own event at its own revision: a merge read
    // from only one end is unreadable history.
    let target_revision = current_revision(tx, into_account_id)?;
    append_provider_account_event(
        tx,
        &NewProviderAccountEvent::new(into_account_id, target_revision, EVENT_KIND_ACCOUNT_MERGED)
            .with_plan_digest(digest)
            .with_evidence(evidence),
    )?;
    Ok(())
}

/// One `plan_noop` event per account the plan referenced, written only when
/// the apply as a whole changed nothing.
fn record_replay_noop(
    tx: &Transaction<'_>,
    plan: &BoundAccountPlan,
    digest: &str,
) -> Result<Vec<i64>, MemoryError> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for action in &plan.actions {
        for account_id in action.account_ids() {
            seen.insert(account_id);
        }
    }

    let mut ids = Vec::with_capacity(seen.len());
    for account_id in seen {
        let revision = current_revision(tx, account_id)?;
        ids.push(append_provider_account_event(
            tx,
            &NewProviderAccountEvent::new(account_id, revision, EVENT_KIND_PLAN_NOOP)
                .with_plan_digest(digest)
                .with_evidence("{}"),
        )?);
    }
    Ok(ids)
}

fn current_revision(tx: &Transaction<'_>, account_id: &str) -> Result<i64, MemoryError> {
    get_provider_account(tx, account_id)?
        .map(|account| account.revision)
        .ok_or_else(|| MemoryError::ProviderAccountPlanRefused {
            reason: ProviderPlanRefusal::UnknownAccount,
            detail: format!("account '{account_id}' vanished mid-apply"),
        })
}

fn alias_evidence(alias_name: &str, source_kind: &str) -> String {
    serde_json::json!({ "alias_name": alias_name, "source_kind": source_kind }).to_string()
}

fn merge_evidence(
    from_account_id: &str,
    into_account_id: &str,
    confirmed_by: &str,
    aliases: &BTreeSet<String>,
) -> String {
    serde_json::json!({
        "merged_from": from_account_id,
        "merged_into": into_account_id,
        "confirmed_by": confirmed_by,
        "aliases_moved": aliases.iter().collect::<Vec<_>>(),
    })
    .to_string()
}

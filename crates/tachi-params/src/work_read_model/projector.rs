//! The projector: typed snapshots in, [`WorkReadModelSetV1`] out (#1693).
//!
//! # Build/replay contract
//!
//! * **Rebuildable by construction.** [`WorkProjectionIndex`] retains only
//!   the latest snapshot per [`SourceKind`] (greatest
//!   `(observed_at, revision)` key — the #1696 law: arrival order is never
//!   an input). [`rebuild`] folds snapshots through the same `apply` the
//!   incremental path uses, so a full rebuild and an incremental update
//!   over the same snapshot multiset produce canonically equal
//!   projections, in any arrival order.
//! * **Honest degradation.** A source with no snapshot leaves its section
//!   `Unavailable { source }`; delivery is `not_integrated` until #1679
//!   lands. Nothing is ever defaulted into existence.
//! * **No write path.** The projector consumes plain data and returns plain
//!   data. It holds no store handle, opens no connection, and exposes no
//!   mutation into any source — dropping or rebuilding the projection
//!   cannot change an authority.
//! * **Visibility.** Work items carrying ANY private fact are hidden
//!   entirely for unauthorized callers (fail-closed, mirroring the #1696
//!   consumer law); health counts run over the visible set only. A public
//!   claim whose issue subject is absent from an unauthorized CurrentTruth
//!   view is likewise hidden: the projection cannot distinguish "private"
//!   from "nonexistent" and must not leak the difference.
//!
//! # `next_action` derivation table (frozen, deterministic)
//!
//! Work-local actions, in priority order — all exposed, never collapsed
//! into one picked winner:
//!
//! 1. `ResolveConflict` — conflicted GitHub predicates or inconsistent
//!    adjudication (blocker evidence named).
//! 2. `RefreshUnavailableSource` — a contributing CurrentTruth snapshot is
//!    missing or its posture is not fresh.
//! 3. `RepairHeadDrift` — the claim's pinned expected head disagrees with
//!    the evidenced GitHub merge SHA (or the env base, when only that pair
//!    exists).
//! 4. `RepairRevertOrReopen` — forwarded from CurrentTruth's resolved
//!    lifecycle (DECISION (OPEN): R6-2, see [`github_section_for`]).
//! 5. `RunVerification` — a terminal run lacks verification evidence.
//! 6. `Adjudicate` — a terminal run awaits adjudication (worker
//!    submit/exit is never acceptance).
//! 7. `AwaitWorkerResult` — an active claim's run is still running.
//! 8. `AwaitImplementation` / 9. `AwaitOwnerAcceptance` — forwarded from
//!    CurrentTruth's open action.
//! 10. `HandoffOrRelease` — the binding claim is orphaned or
//!     reader-expired.
//! 11. `AuthorizedGithubReview` / `AuthorizedGithubClose` — authorized
//!     GitHub actions forwarded from CurrentTruth's open action, plus the
//!     owner-authorized close when acceptance is present and the issue is
//!     currently open. The projection emits them as data; it never
//!     performs them.
//!
//! No model participates anywhere in this table.

use std::collections::BTreeMap;

use crate::current_truth::consumer::CurrentTruthViewV1;
use crate::current_truth::projection::ActionOwnerClassV1;
use crate::current_truth::types::{PredicateV1, ReductionStatusV1, VisibilityClassV1};
use crate::taskintent::mapping::adjudication::{project_adjudication, AdjudicationState};

use super::sources::{
    AdjudicationFactV1, ClaimStateV1, DeliveryObservationV1, ExecEnvFactV1, OwnerDispositionFactV1,
    OwnerDispositionV1, RunReceiptFactV1, SourceFacts, SourceKind, SourceSnapshot, SourceStamp,
    VerificationFactV1, WorkClaimFactV1,
};
use super::types::{
    predicate_status, predicate_value, AdjudicationSectionV1, BlockerKindV1, BlockerV1, ClaimRowV1,
    ClaimSectionV1, DebtClearingV1, DebtStateV1, DeliverySectionV1, ExecEnvRowV1, ExecEnvSectionV1,
    ExecutionStateV1, GithubSectionV1, HeadDriftV1, ImplementationStatusV1, NextActionKindV1,
    NextActionV1, ProjectionOptions, RequiredAuthorityV1, RunRowV1, RunSectionV1, SectionState,
    TransitionDebtV1, VerificationRowV1, VerificationSectionV1, WorkKey, WorkProjectionHealthV1,
    WorkReadModelSetV1, WorkReadModelV1,
};

/// Outcome of applying one snapshot incrementally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// First snapshot for this source kind.
    Applied,
    /// Newer ordering key: replaced the previous snapshot.
    SupersededExisting,
    /// Older-or-equal key: ignored deterministically (out-of-order/stale
    /// arrivals cannot regress a newer source revision).
    StaleIgnored,
}

/// The incremental carrier: latest snapshot per source kind. This is
/// disposable projection state — dropping it loses nothing an authority
/// owns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkProjectionIndex {
    latest: BTreeMap<SourceKind, SourceSnapshot>,
}

impl WorkProjectionIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one snapshot. Same-key duplicates are ignored (idempotent
    /// writes); older keys are ignored (no regression).
    pub fn apply(&mut self, snapshot: SourceSnapshot) -> ApplyOutcome {
        match self.latest.get(&snapshot.stamp.kind) {
            None => {
                self.latest.insert(snapshot.stamp.kind.clone(), snapshot);
                ApplyOutcome::Applied
            }
            Some(existing) => {
                if snapshot.stamp.ordering_key() > existing.stamp.ordering_key() {
                    self.latest.insert(snapshot.stamp.kind.clone(), snapshot);
                    ApplyOutcome::SupersededExisting
                } else {
                    ApplyOutcome::StaleIgnored
                }
            }
        }
    }

    /// The snapshots currently held.
    pub fn snapshots(&self) -> impl Iterator<Item = &SourceSnapshot> {
        self.latest.values()
    }

    fn stamp_for(&self, kind: &SourceKind) -> Option<SourceStamp> {
        self.latest.get(kind).map(|snapshot| snapshot.stamp.clone())
    }
}

/// Full rebuild: fold every snapshot through the same `apply` the
/// incremental path uses, in any order. Canonically equivalent to
/// incrementally applying the same multiset.
pub fn rebuild<I>(snapshots: I) -> WorkProjectionIndex
where
    I: IntoIterator<Item = SourceSnapshot>,
{
    let mut index = WorkProjectionIndex::new();
    for snapshot in snapshots {
        index.apply(snapshot);
    }
    index
}

/// Project the whole set for one read.
pub fn project(index: &WorkProjectionIndex, options: &ProjectionOptions) -> WorkReadModelSetV1 {
    let work_claims: Vec<WorkClaimFactV1> = index
        .snapshots()
        .find_map(|snapshot| match &snapshot.facts {
            SourceFacts::WorkClaims(claims) => Some(claims.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let run_receipts: Vec<RunReceiptFactV1> = index
        .snapshots()
        .find_map(|snapshot| match &snapshot.facts {
            SourceFacts::RunReceipts(receipts) => Some(receipts.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let exec_envs: Vec<ExecEnvFactV1> = index
        .snapshots()
        .find_map(|snapshot| match &snapshot.facts {
            SourceFacts::ExecEnvs(envs) => Some(envs.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let verification: Vec<VerificationFactV1> = index
        .snapshots()
        .find_map(|snapshot| match &snapshot.facts {
            SourceFacts::Verification(facts) => Some(facts.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let adjudication: Vec<AdjudicationFactV1> = index
        .snapshots()
        .find_map(|snapshot| match &snapshot.facts {
            SourceFacts::Adjudication(facts) => Some(facts.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let owner_dispositions: Vec<OwnerDispositionFactV1> = index
        .snapshots()
        .find_map(|snapshot| match &snapshot.facts {
            SourceFacts::OwnerDispositions(facts) => Some(facts.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let repo_views: Vec<CurrentTruthViewV1> = index
        .snapshots()
        .filter_map(|snapshot| match &snapshot.facts {
            SourceFacts::CurrentTruth(view) => Some((**view).clone()),
            _ => None,
        })
        .collect();
    // Delivery degrades honestly: the only minter of delivery truth is
    // #1679, which is not integrated. A delivery snapshot (when one
    // arrives) still names `not_integrated`; no pending-delivery table is
    // ever fabricated here.
    let delivery_observation = index
        .snapshots()
        .find_map(|snapshot| match &snapshot.facts {
            SourceFacts::Delivery(observation) => Some(observation.clone()),
            _ => None,
        })
        .unwrap_or(DeliveryObservationV1::NotIntegrated {
            note: "no delivery snapshot; #1679 surface not integrated".to_string(),
        });

    // ---- Join: group every fact onto a WorkKey. ----------------------
    // Claims anchor keys (issue ref when named, else dispatch, else claim).
    // Runs/envs/verifications/adjudications join by dispatch id or claim
    // id. A fact that joins nothing gets its own key — never a guessed
    // issue. Every CurrentTruth issue subject is also a work item (pure
    // GitHub state with no local facts).
    let mut keys: Vec<WorkKey> = Vec::new();
    let push_key = |key: WorkKey, keys: &mut Vec<WorkKey>| {
        if !keys.contains(&key) {
            keys.push(key);
        }
    };
    for claim in &work_claims {
        push_key(claim_work_key(claim), &mut keys);
    }
    for run in &run_receipts {
        push_key(WorkKey::Dispatch(run.dispatch_id.clone()), &mut keys);
    }
    for env in &exec_envs {
        match env_work_key(env, &work_claims) {
            Some(key) => push_key(key, &mut keys),
            None => push_key(
                WorkKey::Claim(env.claim_id.clone().unwrap_or_else(|| env.env_id.clone())),
                &mut keys,
            ),
        }
    }
    for fact in &verification {
        if let Some(key) = verification_work_key(fact, &work_claims) {
            push_key(key, &mut keys);
        }
    }
    for fact in &adjudication {
        push_key(
            dispatch_work_key(&fact.dispatch_id, &work_claims),
            &mut keys,
        );
    }
    for view in &repo_views {
        for subject in &view.subjects {
            if let Some((repo, number)) = parse_subject_token(&subject.subject_token) {
                push_key(WorkKey::Issue { repo, number }, &mut keys);
            }
        }
    }
    keys.sort();

    let authorization = options.authorization;
    let mut items = Vec::new();
    for key in keys {
        if let Some(model) = project_one(
            key,
            &work_claims,
            &run_receipts,
            &exec_envs,
            &verification,
            &adjudication,
            &owner_dispositions,
            &repo_views,
            &delivery_observation,
            index,
            options,
        ) {
            // Fail-closed visibility: ANY private fact hides the whole work
            // item from an UNAUTHORIZED caller (mirror of the #1696
            // consumer law); an authorized caller sees private work. A
            // public claim whose issue subject is absent from an
            // unauthorized view hides it too (the projection cannot
            // distinguish private from nonexistent).
            let has_private_fact = !authorization.sees_private && model_touches_private(&model);
            let leaks_hidden_subject =
                !authorization.sees_private && subject_hidden(&model, &repo_views);
            if has_private_fact || leaks_hidden_subject {
                continue;
            }
            items.push(model);
        }
    }

    let health = WorkProjectionHealthV1 {
        visible_work_count: items.len(),
        conflicted_count: items
            .iter()
            .filter(|item| {
                matches!(&item.github, SectionState::Available(section) if section.conflicted)
                    || item
                        .blockers
                        .iter()
                        .any(|blocker| blocker.kind == BlockerKindV1::AdjudicationInconsistent)
            })
            .count(),
        blocked_count: items
            .iter()
            .filter(|item| !item.blockers.is_empty())
            .count(),
        refresh_debt_count: items
            .iter()
            .filter(|item| {
                matches!(&item.github, SectionState::Available(section) if !section.posture_fresh)
            })
            .count(),
    };

    WorkReadModelSetV1 {
        read_at: options.read_at.clone(),
        items,
        health,
    }
}

/// The key one claim anchors to: its issue ref when parseable, else its
/// dispatch id, else its own claim id.
fn claim_work_key(claim: &WorkClaimFactV1) -> WorkKey {
    if let Some(issue_ref) = &claim.issue_ref {
        if let Some(key @ WorkKey::Issue { .. }) = WorkKey::parse_issue_ref(issue_ref) {
            return key;
        }
    }
    if let Some(dispatch_id) = &claim.dispatch_id {
        return WorkKey::Dispatch(dispatch_id.clone());
    }
    WorkKey::Claim(claim.claim_id.clone())
}

/// A dispatch joins the claim that carries the same dispatch id (issue key
/// when the claim names one), else stands alone under `Dispatch`.
fn dispatch_work_key(dispatch_id: &str, claims: &[WorkClaimFactV1]) -> WorkKey {
    claims
        .iter()
        .find(|claim| claim.dispatch_id.as_deref() == Some(dispatch_id))
        .map(claim_work_key)
        .unwrap_or_else(|| WorkKey::Dispatch(dispatch_id.to_string()))
}

/// An env joins its claim (by claim id or dispatch id), else anchors to its
/// own identity.
fn env_work_key(env: &ExecEnvFactV1, claims: &[WorkClaimFactV1]) -> Option<WorkKey> {
    if let Some(claim_id) = &env.claim_id {
        if let Some(claim) = claims.iter().find(|claim| &claim.claim_id == claim_id) {
            return Some(claim_work_key(claim));
        }
        return Some(WorkKey::Claim(claim_id.clone()));
    }
    if let Some(dispatch_id) = &env.dispatch_id {
        return Some(dispatch_work_key(dispatch_id, claims));
    }
    None
}

/// A verification fact joins by issue ref (parseable) or its dispatch's
/// claim key.
fn verification_work_key(fact: &VerificationFactV1, claims: &[WorkClaimFactV1]) -> Option<WorkKey> {
    if let Some(issue_ref) = &fact.issue_ref {
        if let Some(key @ WorkKey::Issue { .. }) = WorkKey::parse_issue_ref(issue_ref) {
            return Some(key);
        }
    }
    fact.dispatch_id
        .as_deref()
        .map(|dispatch_id| dispatch_work_key(dispatch_id, claims))
}

/// Parse a CurrentTruth subject token (`owner/repo#issue:N`).
fn parse_subject_token(token: &str) -> Option<(String, u64)> {
    let (repo, object) = token.rsplit_once('#')?;
    let number = object.strip_prefix("issue:")?.parse().ok()?;
    Some((repo.to_string(), number))
}

/// Whether the work item carries any private fact.
fn model_touches_private(model: &WorkReadModelV1) -> bool {
    if let SectionState::Available(claim) = &model.claim {
        if claim
            .claims
            .iter()
            .any(|row| row.fact.visibility == VisibilityClassV1::Private)
        {
            return true;
        }
    }
    if let SectionState::Available(run) = &model.run {
        if run
            .runs
            .iter()
            .any(|row| row.fact.visibility == VisibilityClassV1::Private)
        {
            return true;
        }
    }
    if let SectionState::Available(env) = &model.exec_env {
        if env
            .envs
            .iter()
            .any(|row| row.fact.visibility == VisibilityClassV1::Private)
        {
            return true;
        }
    }
    false
}

/// Whether an issue-keyed item's subject is absent from the (possibly
/// unauthorized) CurrentTruth view for its repo while that view exists —
/// the caller cannot distinguish private from nonexistent, so the item must
/// not leak.
fn subject_hidden(model: &WorkReadModelV1, repo_views: &[CurrentTruthViewV1]) -> bool {
    let WorkKey::Issue { repo, number } = &model.work_id else {
        return false;
    };
    let Some(view) = repo_views.iter().find(|view| &view.repo == repo) else {
        return false;
    };
    let token = format!("{repo}#issue:{number}");
    !view
        .subjects
        .iter()
        .any(|subject| subject.subject_token == token)
}

#[allow(clippy::too_many_arguments)]
fn project_one(
    key: WorkKey,
    claims: &[WorkClaimFactV1],
    run_receipts: &[RunReceiptFactV1],
    exec_envs: &[ExecEnvFactV1],
    verification: &[VerificationFactV1],
    adjudication: &[AdjudicationFactV1],
    owner_dispositions: &[OwnerDispositionFactV1],
    repo_views: &[CurrentTruthViewV1],
    delivery_observation: &DeliveryObservationV1,
    index: &WorkProjectionIndex,
    options: &ProjectionOptions,
) -> Option<WorkReadModelV1> {
    let bound_claims: Vec<&WorkClaimFactV1> = claims
        .iter()
        .filter(|claim| claim_work_key(claim) == key)
        .collect();
    let bound_runs: Vec<&RunReceiptFactV1> = run_receipts
        .iter()
        .filter(|run| dispatch_work_key(&run.dispatch_id, claims) == key)
        .collect();
    let dispatch_ids: Vec<String> = bound_runs
        .iter()
        .map(|run| run.dispatch_id.clone())
        .collect();

    let claim_section = if claims.is_empty() {
        SectionState::Unavailable {
            source: SourceKind::WorkClaims,
        }
    } else {
        SectionState::Available(ClaimSectionV1 {
            claims: bound_claims
                .iter()
                .map(|fact| ClaimRowV1 {
                    effectively_expired: reader_side_expiry(fact, options),
                    fact: (*fact).clone(),
                })
                .collect(),
        })
    };

    let run_section = if run_receipts.is_empty() {
        SectionState::Unavailable {
            source: SourceKind::RunReceipts,
        }
    } else {
        SectionState::Available(RunSectionV1 {
            runs: bound_runs
                .iter()
                .map(|fact| RunRowV1 {
                    execution_state: execution_state(fact),
                    fact: (*fact).clone(),
                })
                .collect(),
        })
    };

    let bound_envs: Vec<&ExecEnvFactV1> = exec_envs
        .iter()
        .filter(|env| env_work_key(env, claims) == Some(key.clone()))
        .collect();
    let exec_env_section = if exec_envs.is_empty() {
        SectionState::Unavailable {
            source: SourceKind::ExecEnvs,
        }
    } else {
        SectionState::Available(ExecEnvSectionV1 {
            envs: bound_envs
                .iter()
                .map(|fact| ExecEnvRowV1 {
                    fact: (*fact).clone(),
                })
                .collect(),
        })
    };

    let bound_verification: Vec<&VerificationFactV1> = verification
        .iter()
        .filter(|fact| verification_work_key(fact, claims) == Some(key.clone()))
        .collect();
    let verification_section = if verification.is_empty() {
        SectionState::Unavailable {
            source: SourceKind::Verification,
        }
    } else {
        SectionState::Available(VerificationSectionV1 {
            observations: bound_verification
                .iter()
                .map(|fact| VerificationRowV1 {
                    fact: (*fact).clone(),
                })
                .collect(),
        })
    };

    let bound_adjudication: Vec<AdjudicationFactV1> = adjudication
        .iter()
        .filter(|fact| dispatch_work_key(&fact.dispatch_id, claims) == key)
        .cloned()
        .collect();
    let adjudication_section = if adjudication.is_empty() {
        SectionState::Unavailable {
            source: SourceKind::Adjudication,
        }
    } else {
        let state = project_adjudication(bound_adjudication.iter().map(|fact| fact.fact.clone()));
        SectionState::Available(AdjudicationSectionV1 {
            facts: bound_adjudication,
            state,
        })
    };

    let delivery_section = DeliverySectionV1 {
        observation: delivery_observation.clone(),
    };

    // GitHub section: only issue keys have a subject; dispatch/claim keys
    // are honestly `NotApplicable` — never a guessed issue.
    let github_section = match &key {
        WorkKey::Issue { repo, number } => {
            match repo_views.iter().find(|view| &view.repo == repo) {
                None => SectionState::Unavailable {
                    source: SourceKind::CurrentTruth { repo: repo.clone() },
                },
                Some(view) => {
                    SectionState::Available(github_section_for(view, *number, owner_dispositions))
                }
            }
        }
        WorkKey::Dispatch(_) | WorkKey::Claim(_) => SectionState::NotApplicable,
    };

    // ---- contributing source stamps -----------------------------------
    let mut stamps: Vec<SourceStamp> = Vec::new();
    if let SectionState::Available(section) = &claim_section {
        if !section.claims.is_empty() {
            stamps.extend(index.stamp_for(&SourceKind::WorkClaims));
        }
    }
    if let SectionState::Available(section) = &run_section {
        if !section.runs.is_empty() {
            stamps.extend(index.stamp_for(&SourceKind::RunReceipts));
        }
    }
    if let SectionState::Available(section) = &exec_env_section {
        if !section.envs.is_empty() {
            stamps.extend(index.stamp_for(&SourceKind::ExecEnvs));
        }
    }
    if let SectionState::Available(section) = &verification_section {
        if !section.observations.is_empty() {
            stamps.extend(index.stamp_for(&SourceKind::Verification));
        }
    }
    if let SectionState::Available(section) = &adjudication_section {
        if !section.facts.is_empty() {
            stamps.extend(index.stamp_for(&SourceKind::Adjudication));
        }
    }
    if let WorkKey::Issue { repo, .. } = &key {
        // The repo view contributes posture + subject presence even when
        // the subject row itself is absent for this key.
        if repo_views.iter().any(|view| &view.repo == repo) {
            stamps.extend(index.stamp_for(&SourceKind::CurrentTruth { repo: repo.clone() }));
        }
    }
    if let WorkKey::Issue { repo, number } = &key {
        let bound_dispositions = owner_dispositions
            .iter()
            .filter(|fact| {
                WorkKey::parse_issue_ref(&fact.issue_ref)
                    == Some(WorkKey::Issue {
                        repo: repo.clone(),
                        number: *number,
                    })
            })
            .count();
        if bound_dispositions > 0 {
            stamps.extend(index.stamp_for(&SourceKind::OwnerDispositions));
        }
    }
    stamps.sort_by_key(|stamp| stamp.kind.as_token());
    stamps.dedup_by(|a, b| a.as_token() == b.as_token());

    // ---- blockers ------------------------------------------------------
    let mut blockers: Vec<BlockerV1> = Vec::new();
    let github_available = matches!(&github_section, SectionState::Available(_));
    if let SectionState::Available(section) = &github_section {
        if section.conflicted {
            blockers.push(BlockerV1 {
                kind: BlockerKindV1::GithubConflict,
                owner_class: ActionOwnerClassV1::EngineeringAuthority,
                evidence_refs: conflict_evidence(section),
            });
        }
        if !section.posture_fresh {
            blockers.push(BlockerV1 {
                kind: BlockerKindV1::SourceUnavailable,
                owner_class: ActionOwnerClassV1::SourceAdapter,
                evidence_refs: vec![format!("{}:posture", section.repo)],
            });
        }
        // R6-2 (owner-ruled): outstanding transition debt blocks
        // success-shaped projection even when the CurrentTruth family
        // resolution has moved on behind a newer steady-state snapshot.
        if section.transition_debt.any_outstanding() {
            blockers.push(BlockerV1 {
                kind: BlockerKindV1::OutstandingTransitionDebt,
                owner_class: ActionOwnerClassV1::EngineeringAuthority,
                evidence_refs: transition_debt_evidence(section),
            });
        }
    } else if !github_available {
        blockers.push(BlockerV1 {
            kind: BlockerKindV1::SourceUnavailable,
            owner_class: ActionOwnerClassV1::SourceAdapter,
            evidence_refs: vec![key.as_token()],
        });
    }

    if let SectionState::Available(section) = &claim_section {
        let active: Vec<&ClaimRowV1> = section
            .claims
            .iter()
            .filter(|row| row.fact.state == ClaimStateV1::Active)
            .collect();
        if active.len() > 1 {
            blockers.push(BlockerV1 {
                kind: BlockerKindV1::ClaimCollision,
                owner_class: ActionOwnerClassV1::EngineeringAuthority,
                evidence_refs: active.iter().map(|row| row.fact.claim_id.clone()).collect(),
            });
        }
        let orphaned_ids: Vec<String> = section
            .claims
            .iter()
            .filter(|row| {
                row.fact.state == ClaimStateV1::Orphaned || row.effectively_expired == Some(true)
            })
            .map(|row| row.fact.claim_id.clone())
            .collect();
        if !orphaned_ids.is_empty() {
            blockers.push(BlockerV1 {
                kind: BlockerKindV1::ClaimOrphaned,
                owner_class: ActionOwnerClassV1::Worker,
                evidence_refs: orphaned_ids,
            });
        }
    }

    // Head drift: expected head vs evidenced GitHub merge SHA; when no
    // GitHub evidence exists, claim expected head vs env base sha.
    // Absence on either side is never drift.
    let head_drift = head_drift_for(&bound_claims, &github_section, &bound_envs);
    if head_drift.is_some() {
        blockers.push(BlockerV1 {
            kind: BlockerKindV1::HeadDrift,
            owner_class: ActionOwnerClassV1::Worker,
            evidence_refs: vec![key.as_token()],
        });
    }

    let adjudication_state = match &adjudication_section {
        SectionState::Available(section) => Some(section.state.clone()),
        SectionState::Unavailable { .. } | SectionState::NotApplicable => None,
    };
    if adjudication_state == Some(AdjudicationState::Inconsistent) {
        blockers.push(BlockerV1 {
            kind: BlockerKindV1::AdjudicationInconsistent,
            owner_class: ActionOwnerClassV1::EngineeringAuthority,
            evidence_refs: vec![key.as_token()],
        });
    }

    let terminal_dispatch_ids: Vec<String> = bound_runs
        .iter()
        .filter(|run| matches!(execution_state(run), ExecutionStateV1::Finished { .. }))
        .map(|run| run.dispatch_id.clone())
        .collect();
    let terminal_unadjudicated = !terminal_dispatch_ids.is_empty()
        && adjudication_state
            .as_ref()
            .is_none_or(|state| *state == AdjudicationState::Unreviewed);
    if terminal_unadjudicated {
        blockers.push(BlockerV1 {
            kind: BlockerKindV1::AwaitingAdjudication,
            owner_class: ActionOwnerClassV1::Owner,
            evidence_refs: terminal_dispatch_ids.clone(),
        });
    }

    let verification_missing = !terminal_dispatch_ids.is_empty()
        && bound_verification
            .iter()
            .all(|fact| !fact.verification_present);
    if verification_missing {
        blockers.push(BlockerV1 {
            kind: BlockerKindV1::VerificationMissing,
            owner_class: ActionOwnerClassV1::Worker,
            evidence_refs: dispatch_ids.clone(),
        });
    }

    // ---- next actions ---------------------------------------------------
    let mut actions: Vec<NextActionV1> = Vec::new();
    let revision_tokens: Vec<String> = stamps.iter().map(|stamp| stamp.as_token()).collect();
    let mut push_action = |kind: NextActionKindV1,
                           owner_class: ActionOwnerClassV1,
                           required_authority: RequiredAuthorityV1,
                           prerequisite_refs: Vec<String>,
                           blocker_refs: Vec<String>| {
        actions.push(NextActionV1 {
            kind,
            owner_class,
            required_authority,
            prerequisite_refs,
            blocker_refs,
            source_revisions: revision_tokens.clone(),
        });
    };

    let conflicted = blockers.iter().any(|blocker| {
        matches!(
            blocker.kind,
            BlockerKindV1::GithubConflict | BlockerKindV1::AdjudicationInconsistent
        )
    });
    if conflicted {
        push_action(
            NextActionKindV1::ResolveConflict,
            ActionOwnerClassV1::EngineeringAuthority,
            RequiredAuthorityV1::AdjudicatorSeat,
            vec![key.as_token()],
            vec![
                "github_conflict".to_string(),
                "adjudication_inconsistent".to_string(),
            ],
        );
    }
    let refresh_debt = match &github_section {
        SectionState::Available(section) => !section.posture_fresh,
        SectionState::Unavailable { .. } => true,
        SectionState::NotApplicable => false,
    };
    if refresh_debt {
        push_action(
            NextActionKindV1::RefreshUnavailableSource,
            ActionOwnerClassV1::SourceAdapter,
            RequiredAuthorityV1::SourceAdapterRefresh,
            vec![key.as_token()],
            vec!["source_unavailable".to_string()],
        );
    }
    if head_drift.is_some() {
        push_action(
            NextActionKindV1::RepairHeadDrift,
            ActionOwnerClassV1::Worker,
            RequiredAuthorityV1::ClaimHolder,
            vec![key.as_token()],
            vec!["head_drift".to_string()],
        );
    }
    let transition_debt_outstanding = match &github_section {
        SectionState::Available(section) => section.transition_debt.any_outstanding(),
        SectionState::Unavailable { .. } | SectionState::NotApplicable => false,
    };
    if transition_debt_outstanding {
        push_action(
            NextActionKindV1::RepairRevertOrReopen,
            ActionOwnerClassV1::EngineeringAuthority,
            RequiredAuthorityV1::AdjudicatorSeat,
            vec![key.as_token()],
            vec!["outstanding_transition_debt".to_string()],
        );
    }
    if verification_missing {
        push_action(
            NextActionKindV1::RunVerification,
            ActionOwnerClassV1::Worker,
            RequiredAuthorityV1::None,
            dispatch_ids.clone(),
            vec!["verification_missing".to_string()],
        );
    }
    if terminal_unadjudicated {
        push_action(
            NextActionKindV1::Adjudicate,
            ActionOwnerClassV1::Owner,
            RequiredAuthorityV1::AdjudicatorSeat,
            terminal_dispatch_ids.clone(),
            vec!["awaiting_adjudication".to_string()],
        );
    }
    let active_claim_running = bound_claims
        .iter()
        .any(|claim| claim.state == ClaimStateV1::Active)
        && bound_runs.iter().any(|run| {
            matches!(
                execution_state(run),
                ExecutionStateV1::Running | ExecutionStateV1::Unknown
            )
        });
    if active_claim_running {
        push_action(
            NextActionKindV1::AwaitWorkerResult,
            ActionOwnerClassV1::None,
            RequiredAuthorityV1::None,
            dispatch_ids.clone(),
            Vec::new(),
        );
    }
    if let SectionState::Available(section) = &claim_section {
        let orphaned_ids: Vec<String> = section
            .claims
            .iter()
            .filter(|row| {
                row.fact.state == ClaimStateV1::Orphaned || row.effectively_expired == Some(true)
            })
            .map(|row| row.fact.claim_id.clone())
            .collect();
        if !orphaned_ids.is_empty() {
            push_action(
                NextActionKindV1::HandoffOrRelease,
                ActionOwnerClassV1::Worker,
                RequiredAuthorityV1::ClaimHolder,
                orphaned_ids,
                vec!["claim_orphaned".to_string()],
            );
        }
    }

    // Forwarded GitHub-domain actions from CurrentTruth's own open action —
    // re-exposed with provenance, never re-derived. The R6-2 lifecycle
    // semantics inside that resolution are the frozen max-key family law.
    if let SectionState::Available(section) = &github_section {
        if let Some(subject) = &section.subject {
            if let Some(open_action) = &subject.open_action {
                if let Some((kind, owner, authority)) = forwarded_action(open_action.kind) {
                    push_action(
                        kind,
                        owner,
                        authority,
                        vec![subject.subject_token.clone()],
                        Vec::new(),
                    );
                }
            }
            // Owner-authorized close: acceptance present + issue currently
            // open. Data only — the projection never closes anything.
            if predicate_status(subject, PredicateV1::OwnerAcceptancePresent)
                == ReductionStatusV1::Current
                && predicate_status(subject, PredicateV1::IssueOpen) == ReductionStatusV1::Current
                && !section.conflicted
            {
                push_action(
                    NextActionKindV1::AuthorizedGithubClose,
                    ActionOwnerClassV1::Owner,
                    RequiredAuthorityV1::GithubCredential,
                    vec![subject.subject_token.clone()],
                    Vec::new(),
                );
            }
        }
    }

    actions.sort_by_key(|action| action.kind.priority_rank());
    actions.dedup_by(|a, b| a.kind == b.kind && a.prerequisite_refs == b.prerequisite_refs);

    // ---- success-shaped gate --------------------------------------------
    // Unknown GitHub is never success-shaped; NotApplicable (dispatch/claim
    // keys with no issue binding) can be, when adjudication accepted and no
    // blocker stands. Outstanding R6-2 transition debt additionally blocks
    // success even behind a fresh, unconflicted family resolution.
    let adjudication_ok = matches!(
        adjudication_state,
        Some(AdjudicationState::Accepted | AdjudicationState::NotRequired { .. })
    );
    let github_ok = match &github_section {
        SectionState::Available(section) => {
            !section.conflicted && !section.transition_debt.any_outstanding()
        }
        SectionState::NotApplicable => true,
        SectionState::Unavailable { .. } => false,
    };
    let success_shaped = blockers.is_empty() && github_ok && adjudication_ok;

    let revision = revision_fingerprint(&stamps);

    Some(WorkReadModelV1 {
        work_id: key,
        read_at: options.read_at.clone(),
        revision,
        github: github_section,
        claim: claim_section,
        run: run_section,
        exec_env: exec_env_section,
        verification: verification_section,
        adjudication: adjudication_section,
        delivery: delivery_section,
        blockers,
        next_actions: actions,
        success_shaped,
        source_stamps: stamps,
    })
}

/// Reader-side heartbeat expiry — a read, never a write back to the claim
/// store (mirrors `is_claim_stale` semantics).
fn reader_side_expiry(fact: &WorkClaimFactV1, options: &ProjectionOptions) -> Option<bool> {
    let ttl = options.claim_ttl_secs?;
    let heartbeat = crate::current_truth::types::ordering_instant(&fact.heartbeat_at);
    let read_at = crate::current_truth::types::ordering_instant(&options.read_at);
    Some(heartbeat + chrono::Duration::seconds(ttl as i64) <= read_at)
}

/// Typed execution state from timestamps/exit code, never prose.
fn execution_state(fact: &RunReceiptFactV1) -> ExecutionStateV1 {
    if fact.finished_at.is_some() {
        ExecutionStateV1::Finished {
            exit_code: fact.exit_code,
        }
    } else if fact.started_at.is_some() {
        ExecutionStateV1::Running
    } else {
        ExecutionStateV1::Unknown
    }
}

/// Build the GitHub section from one repo view, including the R6-2
/// transition-debt projection (owner-ruled law).
fn github_section_for(
    view: &CurrentTruthViewV1,
    number: u64,
    owner_dispositions: &[OwnerDispositionFactV1],
) -> GithubSectionV1 {
    let token = format!("{}#issue:{number}", view.repo);
    let subject = view
        .subjects
        .iter()
        .find(|subject| subject.subject_token == token)
        .cloned();
    let (transition_debt, merge_sha) = match &subject {
        Some(subject) => {
            let debt = transition_debt_for(view, subject, owner_dispositions);
            // `pr_merged` lives on PR subjects, not the issue row: the
            // evidenced merge SHA is the newest merged head across the
            // currently linked PRs.
            let merge_sha = newest_linked_merge_sha(view, subject);
            (debt, merge_sha)
        }
        None => (
            TransitionDebtV1 {
                revert: DebtStateV1::None,
                reopen: DebtStateV1::None,
            },
            None,
        ),
    };
    let (implementation_status, merge_sha, conflicted) = match &subject {
        None => (ImplementationStatusV1::Unknown, None, false),
        Some(subject) => {
            let conflicted = subject
                .predicates
                .iter()
                .any(|row| row.status == ReductionStatusV1::Conflicted);
            if conflicted {
                (ImplementationStatusV1::Conflicted, None, true)
            } else if matches!(transition_debt.revert, DebtStateV1::Outstanding { .. }) {
                // R6-2 (owner-ruled): a reverted merge is not effective
                // implementation, and a later steady-state snapshot that
                // still reports the original PR merged does not repair it.
                (ImplementationStatusV1::Reverted, None, false)
            } else {
                // The mint's `Unit` gap form ("nothing evidenced at this
                // revision") is Current but NOT evidence — mirror the
                // reducer's law: only an evidenced implementation counts.
                let evidenced_implementation =
                    predicate_status(subject, PredicateV1::ImplementationPresent)
                        == ReductionStatusV1::Current
                        && predicate_value(subject, PredicateV1::ImplementationPresent)
                            .is_some_and(|token| token != "unit");
                if evidenced_implementation {
                    (ImplementationStatusV1::Present, merge_sha, false)
                } else if predicate_status(subject, PredicateV1::ImplementationPrLinked)
                    == ReductionStatusV1::Current
                {
                    if predicate_status(subject, PredicateV1::PrOpen) == ReductionStatusV1::Current
                    {
                        (ImplementationStatusV1::UnderReview, None, false)
                    } else {
                        (ImplementationStatusV1::NotLinked, None, false)
                    }
                } else {
                    (ImplementationStatusV1::NotLinked, None, false)
                }
            }
        }
    };
    GithubSectionV1 {
        repo: view.repo.clone(),
        posture_fresh: view.posture.fresh,
        subject,
        implementation_status,
        merge_sha,
        conflicted,
        transition_debt,
    }
}

/// View-level evidence head with its #1696 ordering key. The key mirrors
/// `head_order_key` exactly (instant, source revision) — the assertion id
/// is excluded from causal comparison, matching the reducer's family law.
type HeadWithKey = (
    (chrono::DateTime<chrono::Utc>, String),
    crate::current_truth::consumer::EvidenceHeadViewV1,
);

fn head_key(
    head: &crate::current_truth::consumer::EvidenceHeadViewV1,
) -> (chrono::DateTime<chrono::Utc>, String) {
    (
        crate::current_truth::types::ordering_instant(&head.observed_at),
        head.source_revision.clone(),
    )
}

/// Collect a predicate row's causal evidence heads with their keys. A
/// transition fact's debt survives regardless of its row's group status:
/// the consumer view's `evidence_heads` are the row's live lineage heads,
/// and debt is carried by the FACT they evidence.
fn row_heads(
    subject: &crate::current_truth::consumer::SubjectTruthViewV1,
    predicate: PredicateV1,
) -> Vec<HeadWithKey> {
    subject
        .predicates
        .iter()
        .find(|row| row.predicate == predicate)
        .map(|row| {
            row.evidence_heads
                .iter()
                .map(|head| (head_key(head), head.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The R6-2 transition-debt projection for one issue subject (owner-ruled
/// law, #1693):
///
/// * `issue_reopened` / `merge_reverted` are TRANSITION facts. A later
///   steady-state snapshot (a plain `issue_open` refresh, the original PR
///   still reporting `merged`) is node/state evidence and NEVER clears an
///   unresolved transition.
/// * `issue_reopened` clears only via a causally later authoritative
///   `issue_closed` (which is NOT owner acceptance).
/// * `merge_reverted` clears only via a causally later merged repair PR
///   linked to the issue (a PR other than the reverted one) or an explicit
///   owner-reviewed `no_repair_required` disposition observed at/after the
///   revert.
///
/// Derived from the admitted consumer view plus owner dispositions; no
/// assertion is rewritten, and the #1696 reducer stays untouched.
fn transition_debt_for(
    view: &CurrentTruthViewV1,
    subject: &crate::current_truth::consumer::SubjectTruthViewV1,
    owner_dispositions: &[OwnerDispositionFactV1],
) -> TransitionDebtV1 {
    let issue_number = parse_subject_number(&subject.subject_token).unwrap_or_default();
    let issue_key = WorkKey::Issue {
        repo: view.repo.clone(),
        number: issue_number,
    };

    let reopen = {
        let reopen_heads = row_heads(subject, PredicateV1::IssueReopened);
        if reopen_heads.is_empty() {
            DebtStateV1::None
        } else {
            let reopen_max = reopen_heads.iter().map(|(key, _)| key.clone()).max();
            let closed_heads = row_heads(subject, PredicateV1::IssueClosed);
            let closed_max = closed_heads.iter().map(|(key, _)| key.clone()).max();
            match (reopen_max, closed_max) {
                (Some(reopen_key), Some(closed_key)) if closed_key > reopen_key => {
                    DebtStateV1::Cleared {
                        by: DebtClearingV1::LaterAuthoritativeIssueClosed,
                        evidence_heads: closed_heads.into_iter().map(|(_, head)| head).collect(),
                    }
                }
                _ => DebtStateV1::Outstanding {
                    evidence_heads: reopen_heads.into_iter().map(|(_, head)| head).collect(),
                },
            }
        }
    };

    let revert = {
        // Linked PRs (current typed links only — never title/body similarity).
        let linked = linked_pr_numbers(subject);
        let pr_subject = |number: u64| {
            view.subjects
                .iter()
                .find(|row| row.subject_token == format!("{}#pull_request:{number}", view.repo))
        };

        let mut reverted_prs: Vec<u64> = Vec::new();
        let mut revert_heads: Vec<crate::current_truth::consumer::EvidenceHeadViewV1> = Vec::new();
        let mut revert_max: Option<(chrono::DateTime<chrono::Utc>, String)> = None;
        for pr_number in &linked {
            let Some(pr_subject) = pr_subject(*pr_number) else {
                continue;
            };
            let heads = row_heads(pr_subject, PredicateV1::MergeReverted);
            if heads.is_empty() {
                continue;
            }
            reverted_prs.push(*pr_number);
            revert_heads.extend(heads.iter().map(|(_, head)| head.clone()));
            revert_max = revert_max
                .into_iter()
                .chain(heads.iter().map(|(key, _)| key.clone()))
                .max();
        }
        if revert_heads.is_empty() {
            DebtStateV1::None
        } else {
            // Clearing evidence 1: a merged repair PR — a linked PR OTHER
            // than a reverted one, merged causally after the revert. The
            // reverted PR's own newer merged re-snapshot is ordinary
            // steady-state evidence and does not count.
            let repair = linked.iter().any(|pr_number| {
                if reverted_prs.contains(pr_number) {
                    return false;
                }
                let Some(pr_subject) = pr_subject(*pr_number) else {
                    return false;
                };
                row_heads(pr_subject, PredicateV1::PrMerged)
                    .iter()
                    .any(|(key, _)| revert_max.as_ref().is_some_and(|max| *key > *max))
            });
            if repair {
                DebtStateV1::Cleared {
                    by: DebtClearingV1::PostRevertRepairPrMerged,
                    evidence_heads: Vec::new(),
                }
            } else {
                // Clearing evidence 2: an explicit owner-reviewed
                // `no_repair_required` disposition, bound to this issue and
                // observed at/after the revert (a stale pre-revert
                // disposition cannot clear a newer debt).
                let causal_dispositions: Vec<&OwnerDispositionFactV1> = owner_dispositions
                    .iter()
                    .filter(|fact| {
                        matches!(
                            fact.disposition,
                            OwnerDispositionV1::NoRepairRequired { .. }
                        ) && WorkKey::parse_issue_ref(&fact.issue_ref) == Some(issue_key.clone())
                            && revert_max
                                .as_ref()
                                .map(|(instant, _)| {
                                    crate::current_truth::types::ordering_instant(&fact.observed_at)
                                        >= *instant
                                })
                                .unwrap_or(false)
                    })
                    .collect();
                if causal_dispositions.is_empty() {
                    DebtStateV1::Outstanding {
                        evidence_heads: revert_heads,
                    }
                } else {
                    DebtStateV1::Cleared {
                        by: DebtClearingV1::OwnerNoRepairRequired,
                        evidence_heads: causal_dispositions
                            .iter()
                            .map(|fact| crate::current_truth::consumer::EvidenceHeadViewV1 {
                                assertion_id: format!("owner-disposition:{}", fact.issue_ref),
                                source: "owner_dispositions".to_string(),
                                source_revision: fact.observed_at.clone(),
                                observed_at: fact.observed_at.clone(),
                            })
                            .collect(),
                    }
                }
            }
        }
    };

    TransitionDebtV1 { revert, reopen }
}

/// The newest evidenced merge SHA across the issue's currently linked PRs
/// (max by evidence head key; `pr_merged` rows live on PR subjects).
fn newest_linked_merge_sha(
    view: &CurrentTruthViewV1,
    subject: &crate::current_truth::consumer::SubjectTruthViewV1,
) -> Option<String> {
    let pr_subject = |number: u64| {
        view.subjects
            .iter()
            .find(|row| row.subject_token == format!("{}#pull_request:{number}", view.repo))
    };
    linked_pr_numbers(subject)
        .into_iter()
        .filter_map(|number| {
            let pr = pr_subject(number)?;
            let row = pr.predicates.iter().find(|row| {
                row.predicate == PredicateV1::PrMerged
                    && row.status == ReductionStatusV1::Current
                    && row.value_token != "unknown"
            })?;
            row.evidence_heads
                .iter()
                .map(|head| (head_key(head), row.value_token.clone()))
                .max_by(|a, b| a.0.cmp(&b.0))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, sha)| sha)
}

/// Linked PR numbers from the issue's CURRENT `implementation_pr_linked`
/// value tokens (the consumer view renders the typed link set as
/// `pull_request:N` tokens, comma-joined).
fn linked_pr_numbers(subject: &crate::current_truth::consumer::SubjectTruthViewV1) -> Vec<u64> {
    let mut numbers = Vec::new();
    if let Some(value) = predicate_value(subject, PredicateV1::ImplementationPrLinked) {
        for token in value.split(',') {
            if let Some(number) = token.strip_prefix("pull_request:") {
                if let Ok(number) = number.parse::<u64>() {
                    numbers.push(number);
                }
            }
        }
    }
    numbers.sort();
    numbers.dedup();
    numbers
}

/// Parse the issue number out of a subject token (`owner/repo#issue:N`).
fn parse_subject_number(token: &str) -> Option<u64> {
    token
        .rsplit_once('#')?
        .1
        .strip_prefix("issue:")?
        .parse()
        .ok()
}

/// Evidence tokens for the transition-debt blocker row.
fn transition_debt_evidence(section: &GithubSectionV1) -> Vec<String> {
    let mut refs = Vec::new();
    for (label, state) in [
        ("merge_reverted", &section.transition_debt.revert),
        ("issue_reopened", &section.transition_debt.reopen),
    ] {
        if let DebtStateV1::Outstanding { evidence_heads } = state {
            refs.push(label.to_string());
            refs.extend(evidence_heads.iter().map(|head| head.assertion_id.clone()));
        }
    }
    refs
}

/// Conflicted-predicate evidence tokens for the blocker row.
fn conflict_evidence(section: &GithubSectionV1) -> Vec<String> {
    section
        .subject
        .as_ref()
        .map(|subject| {
            subject
                .predicates
                .iter()
                .filter(|row| row.status == ReductionStatusV1::Conflicted)
                .map(|row| row.predicate.as_str().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Head drift between the active claim's pinned expected head and the
/// GitHub authority's evidenced merge SHA; falls back to claim-vs-env-base
/// when no GitHub evidence exists. `Some(...)` only when both sides of a
/// comparison are present and disagree — absence is never drift.
fn head_drift_for(
    bound_claims: &[&WorkClaimFactV1],
    github_section: &SectionState<GithubSectionV1>,
    bound_envs: &[&ExecEnvFactV1],
) -> Option<HeadDriftV1> {
    let expected_head = bound_claims
        .iter()
        .filter(|claim| claim.state == ClaimStateV1::Active)
        .filter_map(|claim| claim.expected_head.clone())
        .max()?;
    let github_merge_sha = match github_section {
        SectionState::Available(section) => section.merge_sha.clone(),
        SectionState::Unavailable { .. } | SectionState::NotApplicable => None,
    };
    let env_base_sha = bound_envs
        .iter()
        .filter_map(|env| env.base_sha.clone())
        .max();
    let drift = if let Some(merge_sha) = &github_merge_sha {
        &expected_head != merge_sha
    } else if let Some(base_sha) = &env_base_sha {
        &expected_head != base_sha
    } else {
        false
    };
    drift.then_some(HeadDriftV1 {
        claim_expected_head: Some(expected_head),
        github_merge_sha,
        env_base_sha,
    })
}

/// Map a CurrentTruth open action onto the work-model vocabulary.
fn forwarded_action(
    kind: crate::current_truth::types::OpenActionKindV1,
) -> Option<(NextActionKindV1, ActionOwnerClassV1, RequiredAuthorityV1)> {
    use crate::current_truth::types::OpenActionKindV1;
    match kind {
        OpenActionKindV1::ResolveConflict => Some((
            NextActionKindV1::ResolveConflict,
            ActionOwnerClassV1::EngineeringAuthority,
            RequiredAuthorityV1::AdjudicatorSeat,
        )),
        OpenActionKindV1::RefreshUnavailableSource => Some((
            NextActionKindV1::RefreshUnavailableSource,
            ActionOwnerClassV1::SourceAdapter,
            RequiredAuthorityV1::SourceAdapterRefresh,
        )),
        OpenActionKindV1::RepairRevertOrReopen => Some((
            NextActionKindV1::RepairRevertOrReopen,
            ActionOwnerClassV1::EngineeringAuthority,
            RequiredAuthorityV1::AdjudicatorSeat,
        )),
        OpenActionKindV1::RunVerification => Some((
            NextActionKindV1::RunVerification,
            ActionOwnerClassV1::Worker,
            RequiredAuthorityV1::None,
        )),
        OpenActionKindV1::AwaitImplementation => Some((
            NextActionKindV1::AwaitImplementation,
            ActionOwnerClassV1::None,
            RequiredAuthorityV1::None,
        )),
        OpenActionKindV1::ReviewPr => Some((
            NextActionKindV1::AuthorizedGithubReview,
            ActionOwnerClassV1::Owner,
            RequiredAuthorityV1::GithubCredential,
        )),
        OpenActionKindV1::AwaitOwnerAcceptance => Some((
            NextActionKindV1::AwaitOwnerAcceptance,
            ActionOwnerClassV1::Owner,
            RequiredAuthorityV1::None,
        )),
        OpenActionKindV1::NoOpenAction => None,
    }
}

/// Deterministic revision fingerprint over sorted stamp tokens.
fn revision_fingerprint(stamps: &[SourceStamp]) -> String {
    let mut tokens: Vec<String> = stamps.iter().map(|stamp| stamp.as_token()).collect();
    tokens.sort();
    tokens.dedup();
    tokens.join(";")
}

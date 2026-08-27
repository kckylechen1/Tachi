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
//! 4. `RepairRevertOrReopen` — outstanding R6-2 transition debt (the
//!    owner-ruled law, including debt the CurrentTruth family resolution
//!    has left behind a newer steady-state snapshot).
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
    /// Older key, or an identical same-key duplicate: ignored
    /// deterministically (out-of-order/stale arrivals cannot regress a
    /// newer source revision; idempotent re-application changes nothing).
    StaleIgnored,
}

/// The retained pairing behind one immutable ordering key: the facts and
/// the RAW `observed_at` text they were stamped with (the text
/// participates in conflict detection — see `apply`).
type SeenEntry = (SourceFacts, String);

/// The incremental carrier: latest snapshot per source kind, plus the
/// content identity of EVERY immutable ordering key ever applied — so an
/// equal-key content contradiction is rejected no matter how many newer
/// snapshots arrived in between. This is disposable projection state:
/// dropping it loses nothing an authority owns.
///
/// The `seen` map is a **stream-integrity tripwire, not truth**: it never
/// feeds `project` (only `latest` does) and only rejects corrupted appends
/// fail-closed. Its sensitivity is per-index-lifetime — after a drop and
/// rebuild from the snapshots the sources currently return, a contradiction
/// that only existed between two superseded arrivals is no longer observed.
/// That is not a rebuild/incremental divergence: `rebuild` folds the SAME
/// multiset through the same `apply`, so the projected output and the
/// conflict outcome are identical for equal multisets in every arrival
/// order. The durable backstop for same-key contradiction is each
/// authority's own law (e.g. the #1696 `ContradictsExistingRevision`
/// store gate); no adapter minting from an authority can ever produce one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkProjectionIndex {
    latest: BTreeMap<SourceKind, SourceSnapshot>,
    seen: BTreeMap<(SourceKind, (chrono::DateTime<chrono::Utc>, String)), SeenEntry>,
}

impl WorkProjectionIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one snapshot. Older keys are ignored (no regression); an
    /// identical same-key duplicate is idempotently ignored; two snapshots
    /// sharing one immutable `(observed_at, revision)` key with DIFFERENT
    /// content are rejected fail-closed (the #1696
    /// `ContradictsExistingRevision` law — arrival order never picks the
    /// winner, because neither content wins). The contradiction check runs
    /// against every previously seen key, not just the currently retained
    /// latest, so a superseding snapshot in between cannot mask it. On
    /// rejection the index is left unchanged and the caller must not
    /// consume the conflict.
    pub fn apply(
        &mut self,
        snapshot: SourceSnapshot,
    ) -> Result<ApplyOutcome, super::sources::SnapshotError> {
        // Revalidate on EVERY apply: `SourceSnapshot`'s fields are public,
        // so a caller can bypass `SourceSnapshot::new` and stamp one kind
        // over another's facts. A mismatched snapshot must be rejected
        // here, not silently consumed with wrong availability/provenance
        // (codex R2 round-5 finding 4).
        snapshot.validate()?;
        let ordering_key = snapshot.stamp.ordering_key();
        let seen_key = (snapshot.stamp.kind.clone(), ordering_key.clone());
        if let Some((existing_facts, existing_observed_at)) = self.seen.get(&seen_key) {
            // The stored pairing includes the RAW observed_at text: two
            // RFC 3339 aliases of one instant (`12:00Z` vs `20:00+08:00`)
            // normalize to the same ordering key, and if their texts
            // differ the retained snapshot's exposed `source_stamps`
            // would depend on arrival order — so an alias-text difference
            // at one immutable key is a content conflict, never
            // first-arrival-wins (codex R2 round-6 finding 3).
            if *existing_facts != snapshot.facts
                || existing_observed_at != &snapshot.stamp.observed_at
            {
                return Err(super::sources::SnapshotError::ContentConflict {
                    kind: snapshot.stamp.kind.as_token(),
                    observed_at: snapshot.stamp.observed_at.clone(),
                    revision: snapshot.stamp.revision.clone(),
                });
            }
        }
        self.seen.insert(
            seen_key,
            (snapshot.facts.clone(), snapshot.stamp.observed_at.clone()),
        );

        let outcome = match self.latest.get(&snapshot.stamp.kind) {
            None => ApplyOutcome::Applied,
            Some(existing) => {
                if ordering_key > existing.stamp.ordering_key() {
                    ApplyOutcome::SupersededExisting
                } else {
                    ApplyOutcome::StaleIgnored
                }
            }
        };
        if !matches!(outcome, ApplyOutcome::StaleIgnored) {
            self.latest.insert(snapshot.stamp.kind.clone(), snapshot);
        }
        Ok(outcome)
    }

    /// The snapshots currently held.
    pub fn snapshots(&self) -> impl Iterator<Item = &SourceSnapshot> {
        self.latest.values()
    }

    fn stamp_for(&self, kind: &SourceKind) -> Option<SourceStamp> {
        self.latest.get(kind).map(|snapshot| snapshot.stamp.clone())
    }

    fn has_snapshot(&self, kind: &SourceKind) -> bool {
        self.latest.contains_key(kind)
    }
}

/// Full rebuild: fold every snapshot through the same `apply` the
/// incremental path uses, in any order. Canonically equivalent to
/// incrementally applying the same multiset. A multiset containing an
/// equal-key content conflict fails in every arrival order — the failure
/// itself is the deterministic outcome.
pub fn rebuild<I>(snapshots: I) -> Result<WorkProjectionIndex, super::sources::SnapshotError>
where
    I: IntoIterator<Item = SourceSnapshot>,
{
    let mut index = WorkProjectionIndex::new();
    for snapshot in snapshots {
        index.apply(snapshot)?;
    }
    Ok(index)
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
    // Authorization-scope gate: a view minted under `sees_private=true`
    // carries private subjects with no per-subject marker, so an
    // UNAUTHORIZED read cannot consume it at all — the snapshot is treated
    // as unusable for that read (section Unavailable, issue items hidden,
    // health counters excluded), never as filtered content. Authorized
    // reads consume any scope.
    let repo_views: Vec<CurrentTruthViewV1> = index
        .snapshots()
        .filter_map(|snapshot| match &snapshot.facts {
            SourceFacts::CurrentTruth(facts) => {
                let usable =
                    options.authorization.sees_private || !facts.minted_authorization.sees_private;
                usable.then(|| facts.view.clone())
            }
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
        // R6-2: orphaned revert debt stays an attributable work item —
        // unlinking a reverted PR is not a causal resolution, so it gets
        // its own blocked `PullRequest` key instead of dissolving.
        for number in orphaned_reverted_prs(view) {
            push_key(
                WorkKey::PullRequest {
                    repo: view.repo.clone(),
                    number,
                },
                &mut keys,
            );
        }
    }
    keys.sort();

    let authorization = options.authorization;
    // Private dispositions never enter an unauthorized projection: their
    // EFFECT (a cleared debt) would be a visibility signal, so they are
    // filtered before the fold and the debt stays outstanding.
    let visible_dispositions: Vec<OwnerDispositionFactV1> = if authorization.sees_private {
        owner_dispositions.clone()
    } else {
        owner_dispositions
            .iter()
            .filter(|fact| fact.visibility == VisibilityClassV1::Public)
            .cloned()
            .collect()
    };
    let mut items = Vec::new();
    for key in keys {
        if let Some(model) = project_one(
            key,
            &work_claims,
            &run_receipts,
            &exec_envs,
            &verification,
            &adjudication,
            &visible_dispositions,
            &repo_views,
            &delivery_observation,
            index,
            options,
        ) {
            // Fail-closed visibility: ANY private fact hides the whole work
            // item from an UNAUTHORIZED caller (mirror of the #1696
            // consumer law); an authorized caller sees private work. An
            // issue-keyed item whose subject is not positively visible in
            // an unauthorized CurrentTruth view is hidden too — with no
            // repo view at all, the projection cannot distinguish a hidden
            // subject from an unknown repo, so it must not leak either.
            let has_private_fact = !authorization.sees_private && model_touches_private(&model);
            let leaks_hidden_subject =
                !authorization.sees_private && subject_hidden(&model, &repo_views);
            if has_private_fact || leaks_hidden_subject {
                continue;
            }
            items.push(model);
        }
    }

    // R6-2 visibility of the unlink-after-revert gap: PRs with evidenced
    // `merge_reverted` that no issue's CURRENT link set claims still carry
    // transition debt (unlinking is not a causal resolution). Each is an
    // attributable, blocked `PullRequest` work item (see
    // `orphaned_reverted_prs`); this content-free counter is the set-level
    // summary and shares the same detection so the two can never disagree.
    let orphaned_revert_debt_count = repo_views
        .iter()
        .map(|view| orphaned_reverted_prs(view).len())
        .sum();

    let health = WorkProjectionHealthV1 {
        visible_work_count: items.len(),
        orphaned_revert_debt_count,
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
/// when the claim names one), else stands alone under `Dispatch`. When
/// MULTIPLE DISTINCT claim keys carry the same dispatch id, the
/// association is ambiguous — the dispatch then stands alone under its
/// own key instead of inheriting whichever claim happened to sort first
/// (codex R2 round-5 finding 5: first-match is adapter-order-dependent).
fn dispatch_work_key(dispatch_id: &str, claims: &[WorkClaimFactV1]) -> WorkKey {
    let mut keys: Vec<WorkKey> = claims
        .iter()
        .filter(|claim| claim.dispatch_id.as_deref() == Some(dispatch_id))
        .map(claim_work_key)
        .collect();
    keys.sort();
    keys.dedup();
    match keys.len() {
        1 => keys.pop().expect("single key"),
        // 0 => standalone; >1 => ambiguous, standalone rather than a guess.
        _ => WorkKey::Dispatch(dispatch_id.to_string()),
    }
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

/// Whether the work item carries any private fact (claims, runs, envs,
/// verification observations, or adjudication rows).
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
    if let SectionState::Available(verification) = &model.verification {
        if verification
            .observations
            .iter()
            .any(|row| row.fact.visibility == VisibilityClassV1::Private)
        {
            return true;
        }
    }
    if let SectionState::Available(adjudication) = &model.adjudication {
        if adjudication
            .facts
            .iter()
            .any(|fact| fact.visibility == VisibilityClassV1::Private)
        {
            return true;
        }
    }
    false
}

/// Whether an issue-keyed item's subject is not positively visible in the
/// (possibly unauthorized) CurrentTruth view — including when no repo view
/// exists at all: the caller cannot distinguish a hidden subject from an
/// unknown repo, so the item must not leak either (#1693 discrimination
/// 12, fail-closed).
fn subject_hidden(model: &WorkReadModelV1, repo_views: &[CurrentTruthViewV1]) -> bool {
    let (repo, token) = match &model.work_id {
        WorkKey::Issue { repo, number } => (repo, format!("{repo}#issue:{number}")),
        WorkKey::PullRequest { repo, number } => (repo, format!("{repo}#pull_request:{number}")),
        // Local-scoped keys carry no GitHub subject: nothing to hide on
        // GitHub grounds (private local facts are handled by
        // `model_touches_private`).
        WorkKey::Dispatch(_) | WorkKey::Claim(_) => return false,
    };
    let Some(view) = repo_views.iter().find(|view| &view.repo == repo) else {
        return true;
    };
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

    // Section availability follows SNAPSHOT EXISTENCE, not row counts: an
    // empty snapshot asserts "no facts at this revision" — knowledge, not
    // unavailability. Only a source with no snapshot at all is
    // `Unavailable`, naming itself.
    let claim_section = if index.has_snapshot(&SourceKind::WorkClaims) {
        SectionState::Available(ClaimSectionV1 {
            claims: bound_claims
                .iter()
                .map(|fact| ClaimRowV1 {
                    effectively_expired: reader_side_expiry(fact, options),
                    fact: (*fact).clone(),
                })
                .collect(),
        })
    } else {
        SectionState::Unavailable {
            source: SourceKind::WorkClaims,
        }
    };

    let run_section = if index.has_snapshot(&SourceKind::RunReceipts) {
        SectionState::Available(RunSectionV1 {
            runs: bound_runs
                .iter()
                .map(|fact| RunRowV1 {
                    execution_state: execution_state(fact),
                    fact: (*fact).clone(),
                })
                .collect(),
        })
    } else {
        SectionState::Unavailable {
            source: SourceKind::RunReceipts,
        }
    };

    let bound_envs: Vec<&ExecEnvFactV1> = exec_envs
        .iter()
        .filter(|env| env_work_key(env, claims) == Some(key.clone()))
        .collect();
    let exec_env_section = if index.has_snapshot(&SourceKind::ExecEnvs) {
        SectionState::Available(ExecEnvSectionV1 {
            envs: bound_envs
                .iter()
                .map(|fact| ExecEnvRowV1 {
                    fact: (*fact).clone(),
                })
                .collect(),
        })
    } else {
        SectionState::Unavailable {
            source: SourceKind::ExecEnvs,
        }
    };

    let bound_verification: Vec<&VerificationFactV1> = verification
        .iter()
        .filter(|fact| verification_work_key(fact, claims) == Some(key.clone()))
        .collect();
    let verification_section = if index.has_snapshot(&SourceKind::Verification) {
        SectionState::Available(VerificationSectionV1 {
            observations: bound_verification
                .iter()
                .map(|fact| VerificationRowV1 {
                    fact: (*fact).clone(),
                })
                .collect(),
        })
    } else {
        SectionState::Unavailable {
            source: SourceKind::Verification,
        }
    };

    let bound_adjudication: Vec<AdjudicationFactV1> = adjudication
        .iter()
        .filter(|fact| dispatch_work_key(&fact.dispatch_id, claims) == key)
        .cloned()
        .collect();
    // Per-dispatch evidence law (codex R2 round-5 finding 6): one
    // accepted/verified dispatch must never discharge a DIFFERENT
    // terminal dispatch. Adjudication is evaluated per terminal dispatch
    // over ITS OWN facts; verification counts per terminal dispatch from
    // same-dispatch facts or dispatch-less (issue-level) facts. Computed
    // before the section owns the facts.
    let bound_runs: Vec<&RunReceiptFactV1> = run_receipts
        .iter()
        .filter(|run| dispatch_work_key(&run.dispatch_id, claims) == key)
        .collect();
    let terminal_dispatch_ids: Vec<String> = bound_runs
        .iter()
        .filter(|run| matches!(execution_state(run), ExecutionStateV1::Finished { .. }))
        .map(|run| run.dispatch_id.clone())
        .collect();
    let unadjudicated_ids: Vec<String> = terminal_dispatch_ids
        .iter()
        .filter(|id| {
            let facts: Vec<crate::taskintent::mapping::adjudication::CanonicalAdjudicationFact> =
                bound_adjudication
                    .iter()
                    .filter(|fact| fact.dispatch_id == **id)
                    .map(|fact| fact.fact.clone())
                    .collect();
            facts.is_empty()
                || !matches!(
                    project_adjudication(facts),
                    AdjudicationState::Accepted | AdjudicationState::NotRequired { .. }
                )
        })
        .cloned()
        .collect();
    let unverified_ids: Vec<String> = terminal_dispatch_ids
        .iter()
        .filter(|id| {
            !bound_verification.iter().any(|fact| {
                (fact.dispatch_id.as_deref() == Some(id.as_str()) || fact.dispatch_id.is_none())
                    && fact.verification_present
            })
        })
        .cloned()
        .collect();
    let adjudication_section = if index.has_snapshot(&SourceKind::Adjudication) {
        let state = project_adjudication(bound_adjudication.iter().map(|fact| fact.fact.clone()));
        SectionState::Available(AdjudicationSectionV1 {
            facts: bound_adjudication,
            state,
        })
    } else {
        SectionState::Unavailable {
            source: SourceKind::Adjudication,
        }
    };

    let delivery_section = DeliverySectionV1 {
        observation: delivery_observation.clone(),
    };

    // GitHub section: only issue keys have an issue subject; orphaned
    // reverted PRs carry their own PR-keyed section (R6-2 attributable
    // debt); dispatch/claim keys are honestly `NotApplicable` — never a
    // guessed issue.
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
        WorkKey::PullRequest { repo, number } => {
            match repo_views.iter().find(|view| &view.repo == repo) {
                None => SectionState::Unavailable {
                    source: SourceKind::CurrentTruth { repo: repo.clone() },
                },
                Some(view) => SectionState::Available(orphan_pr_github_section_for(view, *number)),
            }
        }
        WorkKey::Dispatch(_) | WorkKey::Claim(_) => SectionState::NotApplicable,
    };

    // ---- contributing source stamps -----------------------------------
    // A source stamps every item whose section it made Available — an
    // empty-but-present snapshot contributed the "no facts bind" statement,
    // so its revision belongs in the fingerprint.
    let mut stamps: Vec<SourceStamp> = Vec::new();
    if claim_section.is_available() {
        stamps.extend(index.stamp_for(&SourceKind::WorkClaims));
    }
    if run_section.is_available() {
        stamps.extend(index.stamp_for(&SourceKind::RunReceipts));
    }
    if exec_env_section.is_available() {
        stamps.extend(index.stamp_for(&SourceKind::ExecEnvs));
    }
    if verification_section.is_available() {
        stamps.extend(index.stamp_for(&SourceKind::Verification));
    }
    if adjudication_section.is_available() {
        stamps.extend(index.stamp_for(&SourceKind::Adjudication));
    }
    if let WorkKey::Issue { repo, .. } | WorkKey::PullRequest { repo, .. } = &key {
        // The repo view contributes posture + subject presence even when
        // the subject row itself is absent for this key.
        if repo_views.iter().any(|view| &view.repo == repo) {
            stamps.extend(index.stamp_for(&SourceKind::CurrentTruth { repo: repo.clone() }));
        }
    }
    if matches!(key, WorkKey::Issue { .. }) {
        // The dispositions snapshot stamps whenever it EXISTS: its
        // binding-or-not statement is knowledge that participates in the
        // debt derivation (a replacement snapshot that removes the
        // clearing fact flips debt back to Outstanding — the fingerprint
        // must move with it).
        if index.has_snapshot(&SourceKind::OwnerDispositions) {
            stamps.extend(index.stamp_for(&SourceKind::OwnerDispositions));
        }
    }
    // Delivery participates in EVERY item's model (the delivery section
    // always carries the observation), so a delivery snapshot stamps every
    // item it can change — no more same-fingerprint/different-model pairs.
    if index.has_snapshot(&SourceKind::Delivery) {
        stamps.extend(index.stamp_for(&SourceKind::Delivery));
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

    // Per-dispatch blockers use the vectors computed above the section
    // construction (see the per-dispatch evidence law note).
    let terminal_unadjudicated = !unadjudicated_ids.is_empty();
    if terminal_unadjudicated {
        blockers.push(BlockerV1 {
            kind: BlockerKindV1::AwaitingAdjudication,
            owner_class: ActionOwnerClassV1::Owner,
            evidence_refs: unadjudicated_ids.clone(),
        });
    }

    let verification_missing = !unverified_ids.is_empty();
    if verification_missing {
        blockers.push(BlockerV1 {
            kind: BlockerKindV1::VerificationMissing,
            owner_class: ActionOwnerClassV1::Worker,
            evidence_refs: unverified_ids.clone(),
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
            unverified_ids.clone(),
            vec!["verification_missing".to_string()],
        );
    }
    if terminal_unadjudicated {
        push_action(
            NextActionKindV1::Adjudicate,
            ActionOwnerClassV1::Owner,
            RequiredAuthorityV1::AdjudicatorSeat,
            unadjudicated_ids.clone(),
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
    // EXCEPT `RepairRevertOrReopen`: that action is forwarded only while
    // the DERIVED transition debt is still outstanding — an owner
    // disposition (an #1693-level fact #1696 cannot see) can clear the
    // debt while CurrentTruth's own open action still names repair, and a
    // cleared debt must not carry a stale repair action.
    if let SectionState::Available(section) = &github_section {
        if let Some(subject) = &section.subject {
            if let Some(open_action) = &subject.open_action {
                let suppress_repair = open_action.kind
                    == crate::current_truth::types::OpenActionKindV1::RepairRevertOrReopen
                    && !transition_debt_outstanding;
                if !suppress_repair {
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
            }
            // Owner-authorized close: acceptance present + the issue's
            // RESOLVED lifecycle is open-side. The standalone `IssueOpen`
            // row stays `Current` as a lineage head long after a newer
            // close won the family, so the family winner gates the action,
            // never the row status alone. Data only — the projection never
            // closes anything.
            if predicate_status(subject, PredicateV1::OwnerAcceptancePresent)
                == ReductionStatusV1::Current
                && issue_lifecycle_open(subject)
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
    // Issue-keyed completion requires POSITIVE evidence: an available,
    // unconflicted, debt-free GitHub section whose implementation status is
    // `Present` (NotLinked/UnderReview/Unknown/Reverted are never
    // success-shaped), plus accepted/not-required adjudication and no
    // blockers. Dispatch/claim keys (no issue binding) complete on the
    // adjudication law alone: blockers empty + accepted adjudication.
    let adjudication_ok = matches!(
        adjudication_state,
        Some(AdjudicationState::Accepted | AdjudicationState::NotRequired { .. })
    );
    let github_ok = match &github_section {
        SectionState::Available(section) => {
            !section.conflicted
                && !section.transition_debt.any_outstanding()
                && section.implementation_status == ImplementationStatusV1::Present
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
    let (implementation_status, merge_sha, conflicted, conflict_refs) = match &subject {
        None => (ImplementationStatusV1::Unknown, None, false, Vec::new()),
        Some(subject) => {
            // #1696 law consumed here: conflicts block success-shaped
            // projection — including a conflicted row on a LINKED PR
            // subject (its merge/revert evidence is retained contradiction,
            // never resolution for the issue it implements) AND including
            // lifecycle-family conflicts: a same-key family tie (e.g.
            // `issue_open` + `issue_closed` at one immutable revision)
            // leaves every member row individually `Current`, so the
            // per-row scan alone cannot see it — CurrentTruth communicates
            // it as the issue's `ResolveConflict` open action (whose own
            // blockers name the offending objects).
            let mut conflict_refs: Vec<String> = subject
                .predicates
                .iter()
                .filter(|row| row.status == ReductionStatusV1::Conflicted)
                .map(|row| format!("{}:{}", subject.subject_token, row.predicate.as_str()))
                .collect();
            let linked_conflict = linked_pr_numbers(subject).iter().any(|number| {
                pr_subject_in_view(view, *number).is_some_and(|pr| {
                    let before = conflict_refs.len();
                    conflict_refs.extend(
                        pr.predicates
                            .iter()
                            .filter(|row| row.status == ReductionStatusV1::Conflicted)
                            .map(|row| format!("{}:{}", pr.subject_token, row.predicate.as_str())),
                    );
                    conflict_refs.len() > before
                })
            });
            let family_conflict = subject.open_action.as_ref().is_some_and(|action| {
                if action.kind == crate::current_truth::types::OpenActionKindV1::ResolveConflict {
                    conflict_refs.extend(action.blockers.iter().cloned());
                    true
                } else {
                    false
                }
            });
            let conflicted = !conflict_refs.is_empty() || linked_conflict || family_conflict;
            if conflicted {
                (
                    ImplementationStatusV1::Conflicted,
                    None,
                    true,
                    conflict_refs,
                )
            } else if matches!(transition_debt.revert, DebtStateV1::Outstanding { .. }) {
                // R6-2 (owner-ruled): a reverted merge is not effective
                // implementation, and a later steady-state snapshot that
                // still reports the original PR merged does not repair it.
                (ImplementationStatusV1::Reverted, None, false, conflict_refs)
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
                    (
                        ImplementationStatusV1::Present,
                        merge_sha,
                        false,
                        conflict_refs,
                    )
                } else if predicate_status(subject, PredicateV1::ImplementationPrLinked)
                    == ReductionStatusV1::Current
                {
                    // PR lifecycle predicates live on the LINKED PR
                    // subjects, not the issue row (codex R2 round-5
                    // finding 7): under review iff a linked PR subject
                    // currently reports `pr_open`.
                    let any_linked_open = linked_pr_numbers(subject).iter().any(|number| {
                        pr_subject_in_view(view, *number).is_some_and(|pr| {
                            predicate_status(pr, PredicateV1::PrOpen) == ReductionStatusV1::Current
                        })
                    });
                    if any_linked_open {
                        (
                            ImplementationStatusV1::UnderReview,
                            None,
                            false,
                            conflict_refs,
                        )
                    } else {
                        (
                            ImplementationStatusV1::NotLinked,
                            None,
                            false,
                            conflict_refs,
                        )
                    }
                } else {
                    (
                        ImplementationStatusV1::NotLinked,
                        None,
                        false,
                        conflict_refs,
                    )
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
        conflict_refs,
        transition_debt,
    }
}

/// View-level evidence head with its #1696 ordering key. The key mirrors
/// `head_order_key` exactly (instant, source revision) — the assertion id
/// is excluded from causal comparison, matching the reducer's family law.
///
/// Ties are deliberately NOT broken by assertion id (codex R2 round-5
/// finding 2, adjudicated): a clearing observation at the SAME
/// (instant, revision) as the transition is not causally LATER, so the
/// debt fails closed and stays outstanding — the same law the
/// same-instant disposition test pins. Lexical assertion-id order is
/// identity, not time, and may never manufacture causality.
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
///
/// `admissible_only` restricts the row to **non-conflicted** evidence — the
/// gate the R6-2 ruling requires for CLEARING: a causal resolution must be
/// authoritative (`issue_closed` observation, a proven merged repair), and a
/// `Conflicted` row is retained contradiction evidence, never an
/// authoritative resolution. DETECTION stays ungated: a conflicted
/// transition fact still establishes fail-closed outstanding debt.
fn row_heads(
    subject: &crate::current_truth::consumer::SubjectTruthViewV1,
    predicate: PredicateV1,
    admissible_only: bool,
) -> Vec<HeadWithKey> {
    subject
        .predicates
        .iter()
        .find(|row| row.predicate == predicate)
        .filter(|row| !admissible_only || row.status != ReductionStatusV1::Conflicted)
        .map(|row| {
            row.evidence_heads
                .iter()
                .map(|head| (head_key(head), head.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The PR subject row for one number, when the view contains it.
fn pr_subject_in_view(
    view: &CurrentTruthViewV1,
    number: u64,
) -> Option<&crate::current_truth::consumer::SubjectTruthViewV1> {
    view.subjects
        .iter()
        .find(|row| row.subject_token == format!("{}#pull_request:{number}", view.repo))
}

/// The GitHub section for an ORPHANED reverted PR (R6-2): a PR with
/// evidenced `merge_reverted` that no issue's current link set claims. The
/// debt is real and unresolved — unlinking is not a causal resolution — so
/// it gets its own attributable, blocked work item instead of dissolving
/// into a health counter. Its debt clears only by re-attribution: once an
/// issue's current link set claims the PR again, the debt moves back to
/// that issue (where the ruling's causal resolutions — repair PR, owner
/// disposition — can act on it).
///
/// Resolution boundary (codex R2 round-5 finding 3, adjudicated): a
/// post-unlink repair PR or issue disposition CANNOT clear the orphan
/// item, because the causal chain "repair <-> reverted PR" runs through
/// the issue linkage that the unlink removed, and the v1 consumer view
/// exposes no superseded link lineage to reconstruct it. Closing that
/// gap needs the #1696 link-lineage consumer follow-up; until then the
/// orphan honestly stays blocked (fail-closed).
fn orphan_pr_github_section_for(view: &CurrentTruthViewV1, number: u64) -> GithubSectionV1 {
    let subject = pr_subject_in_view(view, number).cloned();
    let revert_heads = subject
        .as_ref()
        .map(|subject| row_heads(subject, PredicateV1::MergeReverted, false))
        .unwrap_or_default();
    let transition_debt = TransitionDebtV1 {
        revert: if revert_heads.is_empty() {
            DebtStateV1::None
        } else {
            DebtStateV1::Outstanding {
                evidence_heads: revert_heads.into_iter().map(|(_, head)| head).collect(),
            }
        },
        reopen: DebtStateV1::None,
    };
    let mut conflict_refs: Vec<String> = subject
        .as_ref()
        .map(|subject| {
            subject
                .predicates
                .iter()
                .filter(|row| row.status == ReductionStatusV1::Conflicted)
                .map(|row| format!("{}:{}", subject.subject_token, row.predicate.as_str()))
                .collect()
        })
        .unwrap_or_default();
    // Lifecycle-family tie on the orphan PR itself (codex R2 round-6
    // finding 2): tied `pr_merged`/`merge_reverted` heads are a family
    // conflict, not ordinary revert debt — the item reports
    // `GithubConflict`, never a plain `reverted` column.
    let lifecycle_max_tie = subject.as_ref().is_some_and(|subject| {
        let family = [
            PredicateV1::PrOpen,
            PredicateV1::PrMerged,
            PredicateV1::PrClosedUnmerged,
            PredicateV1::MergeReverted,
        ];
        let mut max_key: Option<(chrono::DateTime<chrono::Utc>, String)> = None;
        let mut owners = 0usize;
        for predicate in family {
            for (key, _) in row_heads(subject, predicate, false) {
                match &max_key {
                    None => {
                        max_key = Some(key);
                        owners = 1;
                    }
                    Some(best) if key > *best => {
                        max_key = Some(key);
                        owners = 1;
                    }
                    Some(best) if key == *best => owners += 1,
                    _ => {}
                }
            }
        }
        if owners > 1 {
            conflict_refs.push(format!(
                "{}#lifecycle_conflict",
                subject.subject_token.clone()
            ));
            true
        } else {
            false
        }
    });
    let conflicted = !conflict_refs.is_empty() || lifecycle_max_tie;
    // A reverted implementation is never the effective implementation:
    // no merge SHA is projected while the revert debt is outstanding.
    let implementation_status = if conflicted {
        ImplementationStatusV1::Conflicted
    } else if matches!(transition_debt.revert, DebtStateV1::Outstanding { .. }) {
        ImplementationStatusV1::Reverted
    } else {
        // Totality guard: an orphan key with no revert heads (no longer
        // emitted by `project`) degrades honestly instead of guessing.
        ImplementationStatusV1::Unknown
    };
    GithubSectionV1 {
        repo: view.repo.clone(),
        posture_fresh: view.posture.fresh,
        subject,
        implementation_status,
        merge_sha: None,
        conflicted,
        conflict_refs,
        transition_debt,
    }
}

/// Whether the subject's lifecycle FAMILY has a tie at the given key:
/// another family member's head sits at exactly the same
/// (instant, revision) as the would-be clearing evidence. A tied row is
/// individually `Current` yet NOT an authoritative family winner
/// (#1696 communicates family ties as `ResolveConflict`), so it may
/// never discharge transition debt (codex R2 round-6 finding 1).
fn lifecycle_family_tied_at(
    subject: &crate::current_truth::consumer::SubjectTruthViewV1,
    issue_subject_row: bool,
    winning: PredicateV1,
    key: &(chrono::DateTime<chrono::Utc>, String),
) -> bool {
    let family: &[PredicateV1] = if issue_subject_row {
        &[
            PredicateV1::IssueOpen,
            PredicateV1::IssueClosed,
            PredicateV1::IssueReopened,
        ]
    } else {
        &[
            PredicateV1::PrOpen,
            PredicateV1::PrMerged,
            PredicateV1::PrClosedUnmerged,
            PredicateV1::MergeReverted,
        ]
    };
    family
        .iter()
        .filter(|predicate| **predicate != winning)
        .flat_map(|predicate| row_heads(subject, *predicate, false))
        .any(|(head_key, _)| head_key == *key)
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
        let reopen_heads = row_heads(subject, PredicateV1::IssueReopened, false);
        if reopen_heads.is_empty() {
            DebtStateV1::None
        } else {
            let reopen_max = reopen_heads.iter().map(|(key, _)| key.clone()).max();
            // Clearing evidence must be admissible: a Conflicted close row
            // is retained contradiction evidence, never an authoritative
            // `issue_closed` resolution (R6-2 causal resolution law).
            let closed_heads = row_heads(subject, PredicateV1::IssueClosed, true);
            let closed_max = closed_heads.iter().map(|(key, _)| key.clone()).max();
            match (reopen_max, closed_max) {
                (Some(reopen_key), Some(closed_key))
                    if closed_key > reopen_key
                        && !lifecycle_family_tied_at(
                            subject,
                            true,
                            PredicateV1::IssueClosed,
                            &closed_key,
                        ) =>
                {
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

        let mut reverted_prs: Vec<u64> = Vec::new();
        let mut revert_heads: Vec<crate::current_truth::consumer::EvidenceHeadViewV1> = Vec::new();
        let mut revert_max: Option<(chrono::DateTime<chrono::Utc>, String)> = None;
        for pr_number in &linked {
            let Some(pr_subject) = pr_subject_in_view(view, *pr_number) else {
                continue;
            };
            let heads = row_heads(pr_subject, PredicateV1::MergeReverted, false);
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
            // than a reverted one, merged causally after the revert, with
            // an ADMISSIBLE (non-conflicted) merge row: a conflicted
            // `pr_merged` is retained contradiction evidence, not a proven
            // merge. The reverted PR's own newer merged re-snapshot is
            // ordinary steady-state evidence and does not count.
            //
            // Adapter-contract boundary (codex R2 round-5 finding 1,
            // adjudicated): the mint stamps `pr_merged.observed_at` from
            // the SOURCE's `updated_at`, which is stable for an unchanged
            // object — so a pre-revert merge that is merely re-observed
            // does NOT advance its key past the revert under a faithful
            // #1696 adapter. An adapter that stamps refresh time as
            // `updated_at` violates that contract upstream and can
            // fabricate repair evidence here; this projection cannot see
            // superseded merge lineage to defend against it.
            let mut repair_heads: Vec<crate::current_truth::consumer::EvidenceHeadViewV1> =
                Vec::new();
            for pr_number in &linked {
                if reverted_prs.contains(pr_number) {
                    continue;
                }
                let Some(pr_subject) = pr_subject_in_view(view, *pr_number) else {
                    continue;
                };
                for (key, head) in row_heads(pr_subject, PredicateV1::PrMerged, true) {
                    if revert_max.as_ref().is_some_and(|max| key > *max)
                        && !lifecycle_family_tied_at(pr_subject, false, PredicateV1::PrMerged, &key)
                    {
                        repair_heads.push(head);
                    }
                }
            }
            let repair = !repair_heads.is_empty();
            if repair {
                DebtStateV1::Cleared {
                    by: DebtClearingV1::PostRevertRepairPrMerged,
                    evidence_heads: repair_heads,
                }
            } else {
                // Clearing evidence 2: an explicit owner-reviewed
                // `no_repair_required` disposition, bound to this issue and
                // observed STRICTLY after the revert (a pre-revert
                // disposition — including one sharing the revert's exact
                // observation instant — cannot clear a later debt; when
                // causal resolution evidence is absent the debt stays
                // visible, fail-closed).
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
                                        > *instant
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
/// Reverted PRs are EXCLUDED: a reverted merge is not the effective
/// implementation, and the original PR's post-revert merged re-snapshots
/// are steady-state noise (R6-2) that must not mask the repair PR's SHA
/// or fabricate head drift.
fn newest_linked_merge_sha(
    view: &CurrentTruthViewV1,
    subject: &crate::current_truth::consumer::SubjectTruthViewV1,
) -> Option<String> {
    linked_pr_numbers(subject)
        .into_iter()
        .filter(|number| {
            pr_subject_in_view(view, *number)
                .is_some_and(|pr| row_heads(pr, PredicateV1::MergeReverted, false).is_empty())
        })
        .filter_map(|number| {
            let pr = pr_subject_in_view(view, number)?;
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

/// Whether the issue's CURRENT resolved lifecycle is an open-side winner
/// (`issue_open` or `issue_reopened` holds the family max key over
/// `issue_closed`). The standalone `IssueOpen` row stays `Current` as a
/// lineage head long after a newer close won the family, so close
/// authorization is gated by the family winner, never by row status
/// alone (#1696 family law, consumed read-only).
fn issue_lifecycle_open(subject: &crate::current_truth::consumer::SubjectTruthViewV1) -> bool {
    let open_side = row_heads(subject, PredicateV1::IssueOpen, false)
        .into_iter()
        .chain(row_heads(subject, PredicateV1::IssueReopened, false))
        .map(|(key, _)| key)
        .max();
    let closed_side = row_heads(subject, PredicateV1::IssueClosed, false)
        .into_iter()
        .map(|(key, _)| key)
        .max();
    match (open_side, closed_side) {
        (Some(open), Some(closed)) => open > closed,
        (Some(_), None) => true,
        (None, _) => false,
    }
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

/// PRs with evidenced `merge_reverted` that no issue's CURRENT link set
/// claims (R6-2: unlinking is not a causal resolution, so the debt stays
/// attributable). Shared by key emission and the health counter so the
/// attributable items and the content-free count can never disagree.
fn orphaned_reverted_prs(view: &CurrentTruthViewV1) -> Vec<u64> {
    let claimed: std::collections::BTreeSet<u64> = view
        .subjects
        .iter()
        .filter(|subject| parse_subject_token(&subject.subject_token).is_some())
        .flat_map(linked_pr_numbers)
        .collect();
    let mut numbers: Vec<u64> = view
        .subjects
        .iter()
        .filter(|subject| subject.subject_token.contains("#pull_request:"))
        .filter(|subject| {
            subject.predicates.iter().any(|row| {
                row.predicate == PredicateV1::MergeReverted && !row.evidence_heads.is_empty()
            })
        })
        .filter_map(|subject| {
            subject
                .subject_token
                .rsplit_once('#')
                .and_then(|(_, object)| object.strip_prefix("pull_request:"))
                .and_then(|token| token.parse::<u64>().ok())
        })
        .filter(|number| !claimed.contains(number))
        .collect();
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
    let mut refs = section.conflict_refs.clone();
    if let Some(subject) = &section.subject {
        refs.extend(
            subject
                .predicates
                .iter()
                .filter(|row| row.status == ReductionStatusV1::Conflicted)
                .map(|row| format!("{}:{}", subject.subject_token, row.predicate.as_str())),
        );
    }
    refs.sort();
    refs.dedup();
    refs
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
///
/// Semantics boundary (codex R2 round-4 finding 5, adjudicated): the
/// fingerprint is WHOLE-SOURCE provenance, not an item-scoped content
/// hash. Sections consume whole-source snapshots (rebuildability requires
/// it), so a snapshot whose only change is in private work elsewhere
/// legitimately moves a public item's fingerprint — the token names the
/// source revision the projection consumed, nothing about WHICH subject
/// changed. What a source revision string encodes (counter, content
/// hash over private rows, ...) is the minting adapter's responsibility
/// and its disclosure policy, not this projection's: the projection never
/// decodes or widens it.
fn revision_fingerprint(stamps: &[SourceStamp]) -> String {
    let mut tokens: Vec<String> = stamps.iter().map(|stamp| stamp.as_token()).collect();
    tokens.sort();
    tokens.dedup();
    tokens.join(";")
}

//! Typed source snapshots for the Unified Work Read Model (#1693).
//!
//! Every authority the projection consumes is read through a **typed
//! snapshot** minted by that authority's own read adapter, stamped with the
//! source identity, its immutable revision, and the observation time. The
//! projection never opens a source store itself and holds no handle to one:
//! a [`SourceSnapshot`] is plain data, so a projection built from snapshots
//! is rebuildable by construction and cannot write anything back.
//!
//! Snapshots are **whole-source** observations: one `WorkClaims` snapshot
//! carries every claim observed at that revision, one `CurrentTruth`
//! snapshot carries one repository's consumer view. This mirrors the
//! #1696 store law — source revision, not arrival order, decides currency —
//! and keeps the incremental fold simple: for each [`SourceKind`] the
//! projection retains only the snapshot with the greatest
//! `(observed_at, revision)` ordering key; older arrivals are ignored
//! deterministically no matter when they arrive.

use crate::current_truth::consumer::CurrentTruthViewV1;
use crate::current_truth::types::{ordering_instant, VisibilityClassV1};
use crate::taskintent::mapping::adjudication::CanonicalAdjudicationFact;
use crate::taskintent::plan::LifecycleMode;

/// Which authority a snapshot observes (#1693 "Authority inputs"). Closed
/// vocabulary: adding a source is a deliberate schema change, not a config
/// edit. Fieldful variants make multi-instance sources (one CurrentTruth
/// view per repository) addressable without a second registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceKind {
    /// The WorkClaim lease table (`session_claims`).
    WorkClaims,
    /// Staffing run receipts (`StaffRunReceipt` producers).
    RunReceipts,
    /// ExecEnv leases (branch/worktree/base-head identity).
    ExecEnvs,
    /// One repository's CurrentTruth consumer view (#1696).
    CurrentTruth { repo: String },
    /// Verification evidence facts over dispatch outcomes.
    Verification,
    /// Adjudication spine facts (`dispatch_adjudications`).
    Adjudication,
    /// Issue-scoped owner dispositions (e.g. a reviewed `no_repair_required`
    /// over a revert). Minted by the owner-surface adapter; the projection
    /// consumes read-only.
    OwnerDispositions,
    /// Result delivery (#1679). Not yet an integrated surface — see
    /// [`DeliveryObservationV1::NotIntegrated`].
    Delivery,
}

impl SourceKind {
    /// Stable token for stamps and revision fingerprints.
    pub fn as_token(&self) -> String {
        match self {
            SourceKind::WorkClaims => "work_claims".to_string(),
            SourceKind::RunReceipts => "run_receipts".to_string(),
            SourceKind::ExecEnvs => "exec_envs".to_string(),
            SourceKind::CurrentTruth { repo } => format!("current_truth[{repo}]"),
            SourceKind::Verification => "verification".to_string(),
            SourceKind::Adjudication => "adjudication".to_string(),
            SourceKind::OwnerDispositions => "owner_dispositions".to_string(),
            SourceKind::Delivery => "delivery".to_string(),
        }
    }
}

/// Identity + revision + observation time of one snapshot. `observed_at` is
/// an RFC 3339 instant supplied by the source adapter; the projection never
/// reads a clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStamp {
    pub kind: SourceKind,
    pub revision: String,
    pub observed_at: String,
}

impl SourceStamp {
    /// The deterministic ordering key shared with the #1696 store law:
    /// `(observed_at instant, revision)`. Arrival order is never an input.
    pub fn ordering_key(&self) -> (chrono::DateTime<chrono::Utc>, String) {
        (ordering_instant(&self.observed_at), self.revision.clone())
    }

    /// Deterministic revision-fingerprint token (`kind@revision`).
    pub fn as_token(&self) -> String {
        format!("{}@{}", self.kind.as_token(), self.revision)
    }
}

/// One WorkClaim row as the projection sees it. A typed mirror of the
/// `session_claims` vocabulary (#1239), carried as plain data so the
/// projection links no store. The server edge materializes these from the
/// authoritative rows read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkClaimFactV1 {
    pub claim_id: String,
    pub agent_identity_id: Option<String>,
    pub session_client: Option<String>,
    /// `owner/repo#N` when the claim names an issue.
    pub issue_ref: Option<String>,
    pub dispatch_id: Option<String>,
    pub branch: String,
    pub worktree_path: Option<String>,
    pub role: Option<String>,
    pub mode: ClaimModeV1,
    pub expected_head: Option<String>,
    pub lease_expires_at: Option<String>,
    pub transition_version: i64,
    pub exec_env_id: Option<String>,
    pub state: ClaimStateV1,
    pub heartbeat_at: String,
    pub visibility: VisibilityClassV1,
}

/// Claim lifecycle state, mirroring the `session_claims` three-state law
/// (#1239): orphaned is evidence of an interrupted owner, never an implicit
/// release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStateV1 {
    Active,
    Orphaned,
    Released,
}

impl ClaimStateV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimStateV1::Active => "active",
            ClaimStateV1::Orphaned => "orphaned",
            ClaimStateV1::Released => "released",
        }
    }
}

/// Writable vs read-only claim scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimModeV1 {
    ReadOnly,
    Writable,
}

impl ClaimModeV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimModeV1::ReadOnly => "read_only",
            ClaimModeV1::Writable => "writable",
        }
    }
}

/// One staffing run receipt as the projection sees it (#1692/#1623
/// vocabulary). `state_token` is the receipt's own lifecycle token, carried
/// verbatim — the projection derives execution state from the typed
/// timestamps, never from prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReceiptFactV1 {
    pub dispatch_id: String,
    pub assignment_id: Option<String>,
    pub lifecycle_owner: LifecycleMode,
    pub state_token: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub run_dir: String,
    pub visibility: VisibilityClassV1,
}

/// One ExecEnv lease as the projection sees it (#894/#1118 vocabulary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEnvFactV1 {
    pub env_id: String,
    pub kind: String,
    pub path: String,
    pub repo_root: String,
    pub branch: String,
    pub base_sha: Option<String>,
    pub dispatch_id: Option<String>,
    pub claim_id: Option<String>,
    pub state_token: String,
    pub reclaim_reason: Option<String>,
    pub visibility: VisibilityClassV1,
}

/// One verification observation bound to a dispatch and/or issue ref
/// (fields mirror the `dispatch_outcomes` evidence summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationFactV1 {
    pub dispatch_id: Option<String>,
    pub issue_ref: Option<String>,
    pub verification_present: bool,
    pub diff_present: bool,
    pub evidence_refs: Vec<String>,
    /// Private verification/result evidence is representable and hides the
    /// whole work item for unauthorized callers (#1693 discrimination 12).
    pub visibility: VisibilityClassV1,
}

/// One adjudication-spine fact bound to a dispatch (#1636 law: a worker
/// submit/exit alone is never acceptance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjudicationFactV1 {
    pub dispatch_id: String,
    pub fact: CanonicalAdjudicationFact,
    /// Adjudication rows can name private work; visibility is fail-closed
    /// at projection time.
    pub visibility: VisibilityClassV1,
}

/// One issue-scoped owner disposition. The R6-2 owner ruling names an
/// explicit owner-reviewed `no_repair_required` as one of the only causal
/// resolutions for a `merge_reverted` transition debt; this fact is how
/// such a disposition enters the projection. It is minted by the
/// owner-surface adapter — never by this projection — and carries its own
/// observation time so a stale (pre-revert) disposition cannot clear a
/// newer debt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerDispositionFactV1 {
    /// `owner/repo#N` of the disposition's subject issue.
    pub issue_ref: String,
    /// RFC 3339 observation time of the disposition itself.
    pub observed_at: String,
    pub disposition: OwnerDispositionV1,
    /// Private dispositions hide the whole work item for unauthorized
    /// callers.
    pub visibility: VisibilityClassV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerDispositionV1 {
    /// The owner reviewed the revert and ruled it requires no repair.
    NoRepairRequired { note: String },
}

/// What the delivery surface can honestly report today. #1679 has not
/// landed: there is no durable requester-delivery ledger to observe, so the
/// only honest observation is `NotIntegrated`. When #1679 lands, its
/// canonical states become additional variants here — never a fabricated
/// `pending-delivery` table inside this projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryObservationV1 {
    /// No delivery surface is integrated (#1679 open). Delivery state is
    /// `unavailable / not_integrated`, never guessed.
    NotIntegrated { note: String },
}

impl DeliveryObservationV1 {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryObservationV1::NotIntegrated { .. } => "not_integrated",
        }
    }
}

/// A CurrentTruth consumer view **bound to the authorization scope it was
/// minted under**. `read_view` pre-filters private subjects by its caller,
/// and the view itself carries no scope marker — so the projection records
/// the minting scope here and refuses to serve an unauthorized read from a
/// private-scoped snapshot (fail-closed: the scope mismatch is treated as
/// an unusable view for that read, never as filtered content).
///
/// Trust seam (codex R2 round-7 finding 1, adjudicated): the scope
/// marker is recorded by the ADAPTER that minted the view, exactly as
/// every fact's `visibility` in this module is. A caller that constructs
/// plain-data snapshots and mislabels them can mislead any projection —
/// the security boundary is the adapter minting from the authority
/// (`read_view` enforces real authorization); in-process construction is
/// inside that boundary by design, consistently for all source kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentTruthFactsV1 {
    pub view: CurrentTruthViewV1,
    pub minted_authorization: crate::current_truth::consumer::CallerAuthorizationV1,
}

/// The facts one snapshot observed. The variant must agree with the stamp's
/// [`SourceKind`]; [`SourceSnapshot::new`] enforces this fail-closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceFacts {
    WorkClaims(Vec<WorkClaimFactV1>),
    RunReceipts(Vec<RunReceiptFactV1>),
    ExecEnvs(Vec<ExecEnvFactV1>),
    CurrentTruth(Box<CurrentTruthFactsV1>),
    Verification(Vec<VerificationFactV1>),
    Adjudication(Vec<AdjudicationFactV1>),
    OwnerDispositions(Vec<OwnerDispositionFactV1>),
    Delivery(DeliveryObservationV1),
}

impl SourceFacts {
    fn kind(&self) -> SourceKind {
        match self {
            SourceFacts::WorkClaims(_) => SourceKind::WorkClaims,
            SourceFacts::RunReceipts(_) => SourceKind::RunReceipts,
            SourceFacts::ExecEnvs(_) => SourceKind::ExecEnvs,
            SourceFacts::CurrentTruth(facts) => SourceKind::CurrentTruth {
                repo: facts.view.repo.clone(),
            },
            SourceFacts::Verification(_) => SourceKind::Verification,
            SourceFacts::Adjudication(_) => SourceKind::Adjudication,
            SourceFacts::OwnerDispositions(_) => SourceKind::OwnerDispositions,
            SourceFacts::Delivery(_) => SourceKind::Delivery,
        }
    }
}

/// A snapshot whose kind, revision, and facts agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub stamp: SourceStamp,
    pub facts: SourceFacts,
}

/// The stamp/facts pairing is wrong — a caller tried to mint a snapshot
/// whose declared kind does not match its facts (fail-closed, never a
/// coerced default) — or two snapshots share one immutable
/// `(observed_at, revision)` key while carrying different facts. The
/// latter mirrors the #1696 `ContradictsExistingRevision` law: the same
/// immutable revision may not contradict itself, so arrival order never
/// decides which content wins — neither does.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("snapshot declared {declared} but facts are {actual}")]
    KindMismatch { declared: String, actual: String },
    #[error("snapshot revision must be non-empty for {0}")]
    EmptyRevision(String),
    #[error("CurrentTruth view carries repo `{view}` but stamp declares `{stamp}`")]
    RepoMismatch { stamp: String, view: String },
    #[error(
        "two snapshots for {kind} share ordering key ({observed_at}, {revision}) with different content"
    )]
    ContentConflict {
        kind: String,
        observed_at: String,
        revision: String,
    },
    #[error("snapshot observed_at `{observed_at}` for {kind} is not RFC 3339")]
    InvalidObservedAt { kind: String, observed_at: String },
}

/// `observed_at` must be a real RFC 3339 timestamp: the shared
/// `ordering_instant` maps malformed text to the MINIMUM instant, so an
/// unvalidated malformed snapshot would sort before everything and be
/// `StaleIgnored`, silently preserving stale truth instead of failing
/// closed (codex R2 round-10 finding 2).
fn validate_observed_at(kind: &SourceKind, observed_at: &str) -> Result<(), SnapshotError> {
    if chrono::DateTime::parse_from_rfc3339(observed_at).is_err() {
        return Err(SnapshotError::InvalidObservedAt {
            kind: kind.as_token(),
            observed_at: observed_at.to_string(),
        });
    }
    Ok(())
}

impl SourceSnapshot {
    /// Mint a validated snapshot. `revision` must be non-empty; the facts
    /// variant must match the declared kind; a CurrentTruth snapshot's
    /// stamped repo must equal the view's repo; `observed_at` must parse
    /// as RFC 3339.
    pub fn new(
        kind: SourceKind,
        revision: impl Into<String>,
        observed_at: impl Into<String>,
        facts: SourceFacts,
    ) -> Result<Self, SnapshotError> {
        let revision = revision.into();
        if revision.is_empty() {
            return Err(SnapshotError::EmptyRevision(kind.as_token()));
        }
        let observed_at = observed_at.into();
        validate_observed_at(&kind, &observed_at)?;
        let actual = facts.kind();
        match (&kind, &actual) {
            (
                SourceKind::CurrentTruth { repo: stamped },
                SourceKind::CurrentTruth { repo: view },
            ) => {
                if stamped != view {
                    return Err(SnapshotError::RepoMismatch {
                        stamp: stamped.clone(),
                        view: view.clone(),
                    });
                }
            }
            (declared, actual) if declared == actual => {}
            (declared, actual) => {
                return Err(SnapshotError::KindMismatch {
                    declared: declared.as_token(),
                    actual: actual.as_token(),
                });
            }
        }
        Ok(Self {
            stamp: SourceStamp {
                kind,
                revision,
                observed_at,
            },
            facts,
        })
    }

    /// Fail-closed revalidation of an already-constructed snapshot. The
    /// struct's fields are public (plain-data seam), so a caller can
    /// bypass `new` and pair one kind's stamp with another kind's facts;
    /// [`WorkProjectionIndex::apply`] runs this on every apply so a
    /// mismatched snapshot is rejected, never silently consumed with the
    /// wrong section availability or provenance.
    pub fn validate(&self) -> Result<(), SnapshotError> {
        if self.stamp.revision.is_empty() {
            return Err(SnapshotError::EmptyRevision(self.stamp.kind.as_token()));
        }
        validate_observed_at(&self.stamp.kind, &self.stamp.observed_at)?;
        let actual = self.facts.kind();
        match (&self.stamp.kind, &actual) {
            (
                SourceKind::CurrentTruth { repo: stamped },
                SourceKind::CurrentTruth { repo: view },
            ) => {
                if stamped != view {
                    return Err(SnapshotError::RepoMismatch {
                        stamp: stamped.clone(),
                        view: view.clone(),
                    });
                }
            }
            (declared, actual) if declared == actual => {}
            (declared, actual) => {
                return Err(SnapshotError::KindMismatch {
                    declared: declared.as_token(),
                    actual: actual.as_token(),
                });
            }
        }
        Ok(())
    }
}

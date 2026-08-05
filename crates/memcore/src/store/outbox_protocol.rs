//! The idempotent reconciliation protocol over the durable outbox
//! (tachi#1644, #1630 workstream A leaf A2).
//!
//! A1 (`crate::store::outbox`) gave the kernel a durable event log and a frozen
//! six-state machine. This module is the **protocol** a host's sync loop drives
//! that log with: claim a batch, report what the peer said about each event,
//! and — when the peer reports divergence — resolve the conflict by an explicit
//! typed decision.
//!
//! ## Still no remote in here
//!
//! Nothing in this module opens a connection, pushes anything, or waits for
//! anyone. It is transport-agnostic on purpose (#1630: the sync adapter is a
//! downstream consumer, and a `StoreProfile::PortableKernel` database must be
//! able to run this protocol with no Tachi daemon anywhere). So every name here
//! says what actually happened locally: a caller *reported* an outcome. A1's
//! honesty rule for `last_successful_sync` — "when a caller last told this
//! store an event was accepted", never "when a remote confirmed" — is the same
//! rule, and it is why this module's entry point is `apply_outbox_outcome`
//! rather than anything containing the word "remote".
//!
//! ## The three invariants this layer exists to hold
//!
//! 1. **Replay cannot double-apply.** Every entry point is idempotent by
//!    *state*, not by memory of having been called: re-reporting the outcome an
//!    event already carries is a no-op receipt that is explicitly
//!    distinguishable from the first application
//!    ([`OutboxOutcomeApplication`]), and re-committing a completed
//!    `event_id` is refused at insert by A1's primary key without a second
//!    memory write.
//! 2. **No last-write-wins for governed heads.** A reported outcome that
//!    differs from the recorded one is refused, never overwritten. A conflict
//!    is *never* auto-resolved: `conflicted` sits there until a caller makes an
//!    explicit typed decision, and no decision path rewrites the local object
//!    to match a peer.
//! 3. **A remote refusal never erases the local source event.** Rejection,
//!    conflict and both resolutions leave a durable, inspectable row; the local
//!    memory row is untouched by all of them.
//!
//! ## What a receipt binds, and what it cannot
//!
//! Every call returns the event **read back from the destination** after the
//! write, so `event_id`, the new state, the canonical timestamps and the stored
//! `last_error_class` all come from storage rather than from the caller's
//! request. `prior_state` is the state this call observed before writing. That
//! is A1's `OutboxCommitReceipt` idiom continued.
//!
//! What a receipt *cannot* do is make the caller's supporting evidence durable.
//! [`OutboxOutcomeEvidence`] is validated and echoed, not stored: the A1 table
//! has twelve pinned columns and no evidence column, and
//! `schema::validate_memory_outbox_schema` refuses a table whose column shape
//! has drifted. The one piece of evidence that survives into the row is the
//! error class, because `last_error_class` exists. See this module's
//! `reclaim`/lineage notes below for the same limit stated where it bites.

use std::time::Duration;

use chrono::{SecondsFormat, Utc};

use crate::{
    db::{self, OutboxEventRow, OutboxState},
    error::{MemoryError, OutboxOutcomeRefusal},
    MemoryStore,
};

use super::outbox::{enqueue_outbox_resolution_successor_event_within_tx, OutboxEventMeta};

/// What one drain asks for.
///
/// `limit` bounds the batch; `0` yields an empty batch rather than a refusal,
/// matching [`MemoryStore::list_outbox_events`].
///
/// `reclaim_stale_after` is the crash-recovery bound: an event that has been
/// `in_flight` for at least this long may be taken over by this drain. `None`
/// — the conservative default — never touches another consumer's in-flight
/// event. It is opt-in because only the caller knows how long its own push
/// leg can legitimately take, and a bound shorter than that turns every slow
/// push into a duplicate delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxClaimRequest {
    pub limit: usize,
    pub reclaim_stale_after: Option<Duration>,
}

impl OutboxClaimRequest {
    /// Claim only events nobody has been handed. Never takes over an in-flight
    /// event.
    pub fn first_claims_only(limit: usize) -> Self {
        Self {
            limit,
            reclaim_stale_after: None,
        }
    }

    /// Claim pending events, and also take over any event that has been in
    /// flight for at least `stale_after`.
    pub fn with_reclaim(limit: usize, stale_after: Duration) -> Self {
        Self {
            limit,
            reclaim_stale_after: Some(stale_after),
        }
    }
}

/// How a claim came to hold an event.
///
/// The distinction is durable-state derived, not remembered: `First` means the
/// row was `pending`, `Reclaimed` means it was already `in_flight` and past the
/// cutoff. A takeover is therefore never silent — it arrives in the batch
/// wearing a different shape than a first hand-off, so a push loop that must
/// treat a possible duplicate delivery differently can.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxClaimKind {
    /// The event was `pending`: this is its first hand-off.
    First,
    /// The event was already `in_flight` and older than the caller's staleness
    /// bound, so this claim took it over. Both fields are carried so the
    /// receipt is *checkable* rather than merely asserted: a reader can verify
    /// `previous_claim_at <= stale_claim_cutoff` for itself.
    ///
    /// There is no reclaim **count** here. Counting takeovers across process
    /// restarts needs a durable counter, and the A1 row has no spare column —
    /// see this module's note on [`MemoryStore::claim_outbox_events`].
    Reclaimed {
        /// The lease stamp this claim replaced, i.e. when the previous claim
        /// was taken or last renewed.
        previous_claim_at: String,
        /// The cutoff this drain used, in canonical UTC-ISO.
        stale_claim_cutoff: String,
    },
}

/// One event handed to a consumer by [`MemoryStore::claim_outbox_events`].
///
/// `event` is the row as stored after the claim: `state` is
/// [`OutboxState::InFlight`], `state_changed_at` is this claim's lease stamp,
/// and `payload_digest` is the digest of the payload as the store holds it —
/// the value a push leg transmits and a peer compares against.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClaimedOutboxEvent {
    pub event: OutboxEventRow,
    pub claim: OutboxClaimKind,
}

/// What a caller reports back about an event it was handed.
///
/// Three outcomes, matching the three edges out of `in_flight` in the frozen
/// A1 matrix. There is deliberately no "retry" or "failed, try again" outcome:
/// a push that never got an answer is not an outcome at all, and the event
/// simply stays `in_flight` until its lease goes stale and a later drain takes
/// it over. Manufacturing an outcome for a silent remote is precisely the
/// failure #1630 forbids ("a silent remote outage cannot be reported as a
/// successful synchronized capture").
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxOutcome {
    /// The consumer accepted it.
    Acknowledged,
    /// The consumer refused it. Terminal, and the local event survives with
    /// the class recorded — a refusal never erases the local source event.
    Rejected { error_class: String },
    /// The consumer reports a divergent state for this object. Terminal until
    /// an explicit typed decision; never auto-resolved.
    Conflicted { error_class: String },
}

impl OutboxOutcome {
    /// The state this outcome moves an `in_flight` event to.
    pub fn target_state(&self) -> OutboxState {
        match self {
            Self::Acknowledged => OutboxState::Acknowledged,
            Self::Rejected { .. } => OutboxState::Rejected,
            Self::Conflicted { .. } => OutboxState::Conflicted,
        }
    }

    /// The class stored in `last_error_class`, or `None` for an acceptance.
    pub fn error_class(&self) -> Option<&str> {
        match self {
            Self::Acknowledged => None,
            Self::Rejected { error_class } | Self::Conflicted { error_class } => {
                Some(error_class.as_str())
            }
        }
    }

    fn validate(&self) -> Result<(), MemoryError> {
        match self.error_class() {
            Some(class) => db::refuse_invalid_class("outcome error_class", class),
            None => Ok(()),
        }
    }
}

/// The caller's supporting material for one reported outcome.
///
/// # Not durable, and named so it cannot be mistaken for durable
///
/// None of this is stored. The A1 table has twelve pinned columns and no
/// evidence column, and adding one is a v30 migration this leaf does not have
/// authority to mint. What survives into the row is the outcome's
/// `error_class`, because `last_error_class` exists. Everything here is
/// validated, bound into the returned receipt, and then gone — so a caller
/// that needs it later must log the receipt.
///
/// It is validated rather than waved through because an unvalidated string
/// field on a synchronization seam becomes a content channel: `reported_by` is
/// a classification token under the same bound as every other class column
/// (`db::MAX_OUTBOX_CLASS_BYTES`, no control characters), and a peer digest
/// must be a canonical lowercase-hex SHA-256 or it is not a digest.
///
/// `peer_revision`/`peer_payload_digest` are what make a conflict report
/// actionable: paired against the event's own `source_revision` and
/// `payload_digest`, they say *how* the two sides diverge, which is the input
/// a human or policy needs to choose a resolution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxOutcomeEvidence {
    /// Who reported the outcome — a token (a peer name, an adapter id), not a
    /// message.
    pub reported_by: String,
    /// The revision the reporter says it holds for this object, if it said.
    pub peer_revision: Option<i64>,
    /// The payload digest the reporter says it holds, if it said. Canonical
    /// lowercase-hex SHA-256, the same shape the event carries.
    pub peer_payload_digest: Option<String>,
}

impl OutboxOutcomeEvidence {
    /// Evidence that names only the reporter.
    pub fn from_reporter(reporter: &str) -> Self {
        Self {
            reported_by: reporter.to_string(),
            peer_revision: None,
            peer_payload_digest: None,
        }
    }

    fn validate(&self) -> Result<(), MemoryError> {
        db::refuse_invalid_class("evidence reported_by", &self.reported_by)?;
        match self.peer_payload_digest.as_deref() {
            Some(digest) => db::refuse_non_canonical_digest(digest),
            None => Ok(()),
        }
    }
}

/// Whether a call performed the change or found it already recorded.
///
/// This is the field that makes idempotence *observable*. A protocol whose
/// duplicate delivery silently returns the same success as the first delivery
/// cannot tell a caller that its retry was a retry; one that errors on the
/// duplicate forces every caller to treat a redelivery as a failure. So both
/// are receipts, and they are different receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxOutcomeApplication {
    /// This call moved the event; `prior_state` was `in_flight`.
    Applied,
    /// The event already carried exactly this outcome. **Nothing was
    /// written**: the row's `state_changed_at` still names the first
    /// application, not this call.
    AlreadyApplied,
}

/// Proof of what one reported outcome did to one event.
///
/// The packet's five bindings, and where each lives: `event.event_id` (which
/// event), [`Self::prior_state`] (the state observed before the call),
/// `event.state` (the state after), `event.created_at`/`event.state_changed_at`
/// (canonical timestamps, as stored), and `event.last_error_class` +
/// [`Self::evidence`] (why). Everything but `prior_state`, `application` and
/// `evidence` is read back from the destination after the write, so the
/// receipt reports what SQLite holds rather than what the caller asked for —
/// A1's `OutboxCommitReceipt` idiom.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxOutcomeReceipt {
    /// The row as stored after the call.
    pub event: OutboxEventRow,
    /// The state this call observed before writing anything.
    pub prior_state: OutboxState,
    /// Whether this call applied the outcome or found it already recorded.
    pub application: OutboxOutcomeApplication,
    /// The evidence the caller supplied. Echoed, never stored.
    pub evidence: OutboxOutcomeEvidence,
}

/// The explicit decisions that can end a conflict.
///
/// # Why there is no automatic arm
///
/// #1630 forbids last-write-wins for governed heads, so the kernel has no
/// policy for choosing between a local head and a peer's. A `conflicted` event
/// therefore sits where it is until somebody — an operator, or a host policy
/// that took responsibility for the choice — calls this with one of three
/// answers. Two of them consume the event; the third records that the decision
/// was deliberately postponed.
///
/// **No arm rewrites the local `memories` row.** "RemoteWins" means the local
/// *event* is withdrawn, not that the peer's payload is written over local
/// memory: importing a peer's version is a memory write on its own terms, with
/// its own event, and never a side effect of resolving an outbox conflict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxConflictResolution {
    /// The local mutation stands. A **new** event is enqueued for the same
    /// object, re-read and re-digested from the destination as it is now, and
    /// the conflicted event is withdrawn as consumed. The old event is never
    /// rewritten into a retry of itself — its history stays exactly what
    /// happened.
    LocalWins,
    /// The peer's version stands. The local event is withdrawn for operator
    /// attention under a class carrying the caller's reason; nothing is
    /// re-enqueued, and local memory is untouched. The stored class is
    /// prefixed with [`OUTBOX_REMOTE_WINS_RESOLVED_CLASS_PREFIX`] — see that
    /// constant's doc comment for why this is a resolved conflict, not an
    /// open one, for health-reporting purposes.
    RemoteWins { error_class: String },
    /// Deliberately not decided now. Writes nothing at all — not even a
    /// restamp — so the event's history still shows when the conflict was
    /// reported rather than when someone last looked at it.
    Deferred,
}

/// Suffix appended to a conflicted event's id to mint its `LocalWins`
/// successor.
///
/// The successor's id **is** the lineage record. The A1 row has no lineage
/// column and adding one is a v30 migration this leaf does not have authority
/// to mint, so the one durable field a new event can carry a reference in is
/// its own caller-stable id. Two properties fall out of deriving it rather
/// than minting a fresh opaque id, and both are wanted:
///
/// * a reader of the raw table can see which event a successor came from, with
///   no join and no side table;
/// * the derivation is deterministic, so a replayed resolution cannot produce
///   a *second* successor — the primary key refuses it.
pub const OUTBOX_LOCAL_WINS_SUCCESSOR_SUFFIX: &str = "::local-wins";

/// The class stamped on a conflicted event that a `LocalWins` decision
/// consumed.
///
/// Kernel-fixed rather than caller-supplied: this token is the durable record
/// of *which* resolution consumed the event, and a caller-chosen string could
/// describe it as anything.
pub const OUTBOX_LOCAL_WINS_RESOLVED_CLASS: &str = "conflict_resolved_local_wins";

/// The class *prefix* stamped on a conflicted event that a `RemoteWins`
/// decision withdrew (tachi#1644 review fix).
///
/// `RemoteWins` still takes the caller's class, because there the interesting
/// fact is why the peer's version won, which the kernel does not know — but
/// the stored value is `"{OUTBOX_REMOTE_WINS_RESOLVED_CLASS_PREFIX}.{caller
/// class}"`, not the caller's class alone. Before this prefix existed, a
/// `RemoteWins` resolution and an ad-hoc operator quarantine were both plain
/// caller-chosen tokens sitting in `last_error_class`, and
/// [`db::read_outbox_health`](crate::db::read_outbox_health) had no way to
/// tell "this conflict was decided" from "this store is holding an
/// unresolved problem" — every `RemoteWins` resolution read as permanent
/// local degradation forever. The prefix is what
/// `read_outbox_health`'s `conflict_resolved_%` match recognizes; the
/// caller's own reason survives as the suffix rather than being discarded, so
/// the fix does not cost an operator the original diagnosis to get a correct
/// health signal.
pub const OUTBOX_REMOTE_WINS_RESOLVED_CLASS_PREFIX: &str = "conflict_resolved_remote_wins";

/// Compose the class stamped on a `RemoteWins` withdrawal from the caller's
/// own class. See [`OUTBOX_REMOTE_WINS_RESOLVED_CLASS_PREFIX`] for why this
/// is a prefix-plus-suffix rather than either alone.
fn outbox_remote_wins_resolved_class(caller_class: &str) -> String {
    format!("{OUTBOX_REMOTE_WINS_RESOLVED_CLASS_PREFIX}.{caller_class}")
}

/// The id [`OutboxConflictResolution::LocalWins`] mints for the successor of
/// `conflicted_event_id`. Public so a caller can find the successor without
/// having kept the receipt.
pub fn outbox_local_wins_successor_id(conflicted_event_id: &str) -> String {
    format!("{conflicted_event_id}{OUTBOX_LOCAL_WINS_SUCCESSOR_SUFFIX}")
}

/// What one resolution did. Three decisions, three receipt shapes — a caller
/// cannot read one as another, and there is no `Option` field that quietly
/// means "the other kind of resolution".
///
/// `LocalWins` boxes its second row for the reason `WorktreeOpenArgs` in
/// `tachi-bootstrap` is a separate struct: two inline `OutboxEventRow`s would
/// make every value of this enum — including a bare `Deferred` — pay for the
/// biggest variant (`clippy::large_enum_variant`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxConflictResolutionReceipt {
    /// The local mutation stood.
    LocalWins {
        /// The conflicted event, now withdrawn as consumed, carrying
        /// [`OUTBOX_LOCAL_WINS_RESOLVED_CLASS`].
        resolved: OutboxEventRow,
        /// The freshly enqueued `pending` event for the same object. Its
        /// `source_revision` and `payload_digest` are read from the
        /// destination **now**, so it announces the object's current local
        /// state rather than replaying the state the conflict was about.
        successor: Box<OutboxEventRow>,
    },
    /// The peer's version stood; the local event is withdrawn.
    RemoteWins { quarantined: OutboxEventRow },
    /// The decision was postponed. The row is returned exactly as it was
    /// found, unwritten.
    Deferred { unresolved: OutboxEventRow },
}

fn outcome_refused(
    reason: OutboxOutcomeRefusal,
    event_id: &str,
    state: OutboxState,
) -> MemoryError {
    MemoryError::OutboxOutcomeRefused {
        reason,
        event_id: event_id.to_string(),
        state: state.as_str().to_string(),
    }
}

/// Mint the staleness cutoff in the **same canonical shape** every stamp in
/// this table is written with (tachi#1432: millisecond precision, `Z` suffix,
/// 24 characters).
///
/// This is load-bearing, not hygiene. The cutoff is compared against
/// `state_changed_at` lexically by SQLite, and lexical order is chronological
/// order *only* for that one shape. A cutoff minted at, say, microsecond
/// precision would compare wrong against millisecond stamps in the same second
/// and silently change which events are reclaimable. So the cutoff is built
/// with `to_rfc3339_opts(SecondsFormat::Millis, true)` — literally the
/// formatter `db::common::now_utc_iso` uses — and the result is length-checked
/// before it can reach the query.
fn stale_claim_cutoff(stale_after: Duration) -> Result<String, MemoryError> {
    let bound = chrono::Duration::from_std(stale_after).map_err(|_| {
        MemoryError::InvalidArg(format!(
            "outbox claim staleness bound {stale_after:?} is too large to express as a timestamp \
             offset"
        ))
    })?;
    let cutoff = Utc::now().checked_sub_signed(bound).ok_or_else(|| {
        MemoryError::InvalidArg(format!(
            "outbox claim staleness bound {stale_after:?} moves the cutoff outside the \
             representable timestamp range"
        ))
    })?;
    let rendered = cutoff.to_rfc3339_opts(SecondsFormat::Millis, true);
    if rendered.len() != CANONICAL_UTC_ISO_LEN {
        return Err(MemoryError::InvalidArg(format!(
            "outbox claim staleness bound {stale_after:?} renders the non-canonical cutoff \
             '{rendered}', which cannot be compared against stored stamps lexically"
        )));
    }
    Ok(rendered)
}

/// Length of a canonical UTC-ISO stamp (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
const CANONICAL_UTC_ISO_LEN: usize = 24;

fn claimed_event(
    row: db::ClaimedOutboxRow,
    cutoff: Option<&str>,
) -> Result<ClaimedOutboxEvent, MemoryError> {
    let claim = match row.previous_state {
        OutboxState::Pending => OutboxClaimKind::First,
        OutboxState::InFlight => OutboxClaimKind::Reclaimed {
            previous_claim_at: row.previous_state_changed_at,
            stale_claim_cutoff: cutoff
                .ok_or_else(|| {
                    MemoryError::Internal(format!(
                        "outbox event '{}' was taken over by a drain that declared no staleness \
                         bound",
                        row.event.event_id
                    ))
                })?
                .to_string(),
        },
        other => {
            return Err(MemoryError::Internal(format!(
                "outbox event '{}' was claimed out of state '{other}', which is not drainable",
                row.event.event_id
            )))
        }
    };
    Ok(ClaimedOutboxEvent {
        event: row.event,
        claim,
    })
}

impl MemoryStore {
    /// Claim a bounded batch of drainable events, moving them to `in_flight` in
    /// **one** transaction, and return them with a receipt each.
    ///
    /// This is the drain seam a host's push loop sits on: what comes back is
    /// the work list plus, per event, the payload digest to transmit and
    /// whether this hand-off is a first delivery or a takeover.
    ///
    /// # Crash recovery
    ///
    /// A consumer that dies mid-push leaves its events `in_flight` forever —
    /// there is no retry edge back to `pending` in the frozen A1 matrix, by
    /// design. Recovery is therefore a *takeover*: with
    /// [`OutboxClaimRequest::with_reclaim`], any event whose lease is older
    /// than the caller's bound comes back in the batch as
    /// [`OutboxClaimKind::Reclaimed`], carrying the stamp it replaced and the
    /// cutoff used. The takeover renews the lease, so the same bound does not
    /// hand the event out again on the next drain, and it changes no state:
    /// `in_flight -> in_flight` remains illegal in the A1 machine, because a
    /// takeover is not a transition.
    ///
    /// # Reclaim bookkeeping: what this does not persist
    ///
    /// Each reclaim is a fresh, self-describing claim receipt, but the number
    /// of times an event has been taken over is **not** counted on the row.
    /// The v29 table has twelve columns, all pinned by
    /// `schema::validate_memory_outbox_schema` (a drifted column shape makes
    /// the database refuse to open), and none of them is spare. A durable
    /// counter therefore needs a v30 migration — schema authority this leaf
    /// does not hold — so what is reported here is exactly what the durable
    /// state supports: *this* takeover, and what it replaced. A cross-restart
    /// count would have to be reconstructed by the caller from its own claim
    /// log until that column exists.
    ///
    /// # Refusals
    ///
    /// A staleness bound too large to express as a timestamp offset is
    /// [`MemoryError::InvalidArg`]; nothing is claimed. A terminal event is
    /// never selected at any bound.
    pub fn claim_outbox_events(
        &mut self,
        request: &OutboxClaimRequest,
    ) -> Result<Vec<ClaimedOutboxEvent>, MemoryError> {
        let cutoff = match request.reclaim_stale_after {
            Some(stale_after) => Some(stale_claim_cutoff(stale_after)?),
            None => None,
        };
        let limit = request.limit;
        let db_label = self.db_label.clone();
        db::retry_memory_locked("claim_outbox_events", &db_label, || {
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let rows = db::claim_outbox_events_within_tx(&tx, limit, cutoff.as_deref())?;
            tx.commit()?;
            rows.into_iter()
                .map(|row| claimed_event(row, cutoff.as_deref()))
                .collect()
        })
    }

    /// Record what a consumer reported about one event it was handed, and
    /// return a typed receipt.
    ///
    /// # The five cases, by the state the event is actually in
    ///
    /// * `in_flight` — the ordinary path. The outcome is applied through the
    ///   frozen A1 machine, which enforces the edge's legality and the
    ///   error-class rule (failure states require a class, acceptance refuses
    ///   one). Receipt: [`OutboxOutcomeApplication::Applied`].
    /// * already carrying **this same outcome and class** — an idempotent
    ///   no-op. Nothing is written, no stamp moves, and the receipt says
    ///   [`OutboxOutcomeApplication::AlreadyApplied`] so a caller can tell its
    ///   retry was a retry. This is the duplicate-acknowledgement case: a
    ///   redelivery of an ack is normal traffic, not an error.
    /// * already carrying a **different** terminal outcome (or the same
    ///   outcome with a different class) —
    ///   [`OutboxOutcomeRefusal::OutcomeAlreadyDiffers`]. The recorded outcome
    ///   stands. Overwriting it with whichever report arrived last is exactly
    ///   the last-write-wins behaviour #1630 forbids for governed heads.
    /// * `pending` — [`OutboxOutcomeRefusal::NeverClaimed`]. Nobody was handed
    ///   this event, so nobody can report on it. Refusing loudly here is what
    ///   turns a mis-addressed or replayed message into a visible protocol
    ///   violation instead of a state jump that skips `in_flight`.
    /// * `quarantined` — [`OutboxOutcomeRefusal::Withdrawn`]. The event was
    ///   withdrawn for operator attention; a late acknowledgement does not get
    ///   to un-withdraw it.
    ///
    /// An unknown `event_id` is [`MemoryError::NotFound`]. Every refusal
    /// writes nothing.
    ///
    /// # What it never touches
    ///
    /// The `memories` row. An outcome is information about the event, not a
    /// new mutation of the object — no reported outcome, including a conflict,
    /// rewrites local memory to match a peer.
    pub fn apply_outbox_outcome(
        &mut self,
        event_id: &str,
        outcome: &OutboxOutcome,
        evidence: &OutboxOutcomeEvidence,
    ) -> Result<OutboxOutcomeReceipt, MemoryError> {
        // Validate before opening a transaction: a malformed report must never
        // take a write lock (the ordering `commit_with_outbox_event` uses for
        // path validation).
        outcome.validate()?;
        evidence.validate()?;
        let db_label = self.db_label.clone();
        db::retry_memory_locked("apply_outbox_outcome", &db_label, || {
            // Even the replay branch that resolves to
            // `OutboxOutcomeApplication::AlreadyApplied` and writes nothing
            // opens this same `BEGIN IMMEDIATE`, because the state has to be
            // read and the write decision made as one atomic observation —
            // a real contention cost under a busy retry storm, not a bug
            // (tachi#1644 review).
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let current = db::read_outbox_event(&tx, event_id)?.ok_or_else(|| {
                MemoryError::NotFound(format!(
                    "outbox event '{event_id}' does not exist; an outcome cannot be reported for \
                     an event this store never enqueued"
                ))
            })?;

            let receipt = match current.state {
                OutboxState::InFlight => {
                    let event = db::transition_outbox_event_within_tx(
                        &tx,
                        event_id,
                        outcome.target_state(),
                        outcome.error_class(),
                    )?;
                    OutboxOutcomeReceipt {
                        event,
                        prior_state: OutboxState::InFlight,
                        application: OutboxOutcomeApplication::Applied,
                        evidence: evidence.clone(),
                    }
                }
                OutboxState::Pending => {
                    return Err(outcome_refused(
                        OutboxOutcomeRefusal::NeverClaimed,
                        event_id,
                        OutboxState::Pending,
                    ))
                }
                OutboxState::Quarantined => {
                    return Err(outcome_refused(
                        OutboxOutcomeRefusal::Withdrawn,
                        event_id,
                        OutboxState::Quarantined,
                    ))
                }
                terminal => {
                    let same_outcome = terminal == outcome.target_state()
                        && current.last_error_class.as_deref() == outcome.error_class();
                    if !same_outcome {
                        return Err(outcome_refused(
                            OutboxOutcomeRefusal::OutcomeAlreadyDiffers,
                            event_id,
                            terminal,
                        ));
                    }
                    OutboxOutcomeReceipt {
                        event: current,
                        prior_state: terminal,
                        application: OutboxOutcomeApplication::AlreadyApplied,
                        evidence: evidence.clone(),
                    }
                }
            };
            tx.commit()?;
            Ok(receipt)
        })
    }

    /// End a conflict by an explicit typed decision.
    ///
    /// The kernel never ends one on its own. `conflicted` is terminal until
    /// this call arrives with one of [`OutboxConflictResolution`]'s three
    /// answers, and no path here writes the peer's version over local memory —
    /// that is the "no LWW for governed heads" clause stated as code.
    ///
    /// # What each decision does, in one transaction
    ///
    /// * [`OutboxConflictResolution::LocalWins`] — enqueues a **new** pending
    ///   event for the same object (id derived by
    ///   [`outbox_local_wins_successor_id`], class/authority/source inherited
    ///   from the conflicted event, revision and digest re-read from the
    ///   destination now), then withdraws the conflicted event under
    ///   [`OUTBOX_LOCAL_WINS_RESOLVED_CLASS`]. Both halves land together or
    ///   neither does, so there is no state in which the old event was
    ///   consumed without a successor to carry the mutation.
    /// * [`OutboxConflictResolution::RemoteWins`] — withdraws the local event
    ///   under [`OUTBOX_REMOTE_WINS_RESOLVED_CLASS_PREFIX`] plus the caller's
    ///   class. Nothing is re-enqueued and the object is untouched.
    /// * [`OutboxConflictResolution::Deferred`] — writes nothing.
    ///
    /// # Refusals
    ///
    /// * Unknown `event_id` — [`MemoryError::NotFound`].
    /// * Any state other than `conflicted` —
    ///   [`OutboxOutcomeRefusal::NotConflicted`], carrying the state found. A
    ///   *second* resolution of the same event lands here: the first one
    ///   consumed it into `quarantined`, and the refusal names that state, so
    ///   a caller replaying its decision learns the decision already landed
    ///   rather than producing a second successor.
    /// * `LocalWins` when the derived successor id is already taken —
    ///   [`MemoryError::Duplicate`], and the conflict stays conflicted. The
    ///   primary key is the enforcement; nothing is half-applied.
    /// * `LocalWins` when the object no longer exists —
    ///   [`MemoryError::NotFound`] from A1's enqueue seam, which refuses an
    ///   event whose object this transaction cannot see.
    pub fn resolve_outbox_conflict(
        &mut self,
        event_id: &str,
        resolution: &OutboxConflictResolution,
    ) -> Result<OutboxConflictResolutionReceipt, MemoryError> {
        if let OutboxConflictResolution::RemoteWins { error_class } = resolution {
            db::refuse_invalid_class("resolution error_class", error_class)?;
        }
        let db_label = self.db_label.clone();
        db::retry_memory_locked("resolve_outbox_conflict", &db_label, || {
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let current = db::read_outbox_event(&tx, event_id)?.ok_or_else(|| {
                MemoryError::NotFound(format!(
                    "outbox event '{event_id}' does not exist; there is no conflict to resolve"
                ))
            })?;
            if current.state != OutboxState::Conflicted {
                return Err(outcome_refused(
                    OutboxOutcomeRefusal::NotConflicted,
                    event_id,
                    current.state,
                ));
            }

            let receipt = match resolution {
                OutboxConflictResolution::Deferred => OutboxConflictResolutionReceipt::Deferred {
                    unresolved: current,
                },
                OutboxConflictResolution::RemoteWins { error_class } => {
                    let stamped_class = outbox_remote_wins_resolved_class(error_class);
                    let quarantined = db::transition_outbox_event_within_tx(
                        &tx,
                        event_id,
                        OutboxState::Quarantined,
                        Some(stamped_class.as_str()),
                    )?;
                    OutboxConflictResolutionReceipt::RemoteWins { quarantined }
                }
                OutboxConflictResolution::LocalWins => {
                    let successor = enqueue_outbox_resolution_successor_event_within_tx(
                        &tx,
                        &current.object_id,
                        &OutboxEventMeta {
                            event_id: outbox_local_wins_successor_id(event_id),
                            object_class: current.object_class.clone(),
                            authority_class: current.authority_class.clone(),
                            source_store: current.source_store.clone(),
                            source_partition: current.source_partition.clone(),
                        },
                    )?;
                    let resolved = db::transition_outbox_event_within_tx(
                        &tx,
                        event_id,
                        OutboxState::Quarantined,
                        Some(OUTBOX_LOCAL_WINS_RESOLVED_CLASS),
                    )?;
                    OutboxConflictResolutionReceipt::LocalWins {
                        resolved,
                        successor: Box::new(successor),
                    }
                }
            };
            tx.commit()?;
            Ok(receipt)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::outbox::{outbox_payload_digest, OutboxEventMeta};
    use crate::types::MemoryEntry;

    fn entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/scratch/outbox".to_string(),
            summary: String::new(),
            text: text.to_string(),
            importance: 0.6,
            timestamp: "2026-08-05T00:00:00.000Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "manual".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: serde_json::json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn meta(event_id: &str) -> OutboxEventMeta {
        OutboxEventMeta {
            event_id: event_id.to_string(),
            object_class: "memory".to_string(),
            authority_class: "host".to_string(),
            source_store: "global".to_string(),
            source_partition: "default".to_string(),
        }
    }

    /// Commit one memory row and its pending event.
    fn commit(store: &mut MemoryStore, object_id: &str, event_id: &str) {
        store
            .commit_with_outbox_event(
                &entry(object_id, &format!("body for {object_id}")),
                &meta(event_id),
            )
            .expect("commit must succeed");
    }

    /// Age a lease deterministically, rather than by sleeping.
    fn age_claim(store: &MemoryStore, event_id: &str, to: &str) {
        store
            .connection()
            .execute(
                "UPDATE memory_outbox_events SET state_changed_at = ?2 WHERE event_id = ?1",
                rusqlite::params![event_id, to],
            )
            .expect("age the lease");
    }

    fn claimed_ids(claimed: &[ClaimedOutboxEvent]) -> Vec<&str> {
        claimed
            .iter()
            .map(|item| item.event.event_id.as_str())
            .collect()
    }

    #[test]
    fn a_claim_hands_out_pending_events_in_enqueue_order_and_bounded_by_limit() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        for index in 0..3 {
            commit(&mut store, &format!("obj-{index}"), &format!("evt-{index}"));
        }

        let first = store
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(2))
            .expect("claim");
        assert_eq!(claimed_ids(&first), vec!["evt-0", "evt-1"]);
        for item in &first {
            assert_eq!(item.event.state, OutboxState::InFlight);
            assert_eq!(item.claim, OutboxClaimKind::First);
        }
        // The digest the push leg transmits is the one A1 stored.
        let stored = store.get("obj-0").expect("get").expect("present");
        assert_eq!(
            first[0].event.payload_digest,
            outbox_payload_digest(&stored).expect("digest")
        );

        assert_eq!(
            claimed_ids(
                &store
                    .claim_outbox_events(&OutboxClaimRequest::first_claims_only(10))
                    .expect("claim")
            ),
            vec!["evt-2"],
            "a drain with no staleness bound never takes what another holds"
        );
        assert!(store
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(10))
            .expect("claim")
            .is_empty());
        assert!(store
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(0))
            .expect("claim")
            .is_empty());
    }

    /// The crash case. A consumer died holding `evt-crash`; there is no retry
    /// edge back to `pending`, so recovery is a takeover — and it arrives
    /// wearing a receipt that says so.
    #[test]
    fn a_stale_in_flight_event_is_reclaimed_with_a_receipt_naming_the_prior_claim() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-crash", "evt-crash");
        let held = store
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(10))
            .expect("claim");
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].claim, OutboxClaimKind::First);

        // Nothing is reclaimable while the lease is fresh.
        assert!(
            store
                .claim_outbox_events(&OutboxClaimRequest::with_reclaim(
                    10,
                    Duration::from_secs(3600)
                ))
                .expect("claim")
                .is_empty(),
            "a fresh claim is not stale"
        );

        age_claim(&store, "evt-crash", "2020-01-01T00:00:00.000Z");
        let reclaimed = store
            .claim_outbox_events(&OutboxClaimRequest::with_reclaim(
                10,
                Duration::from_secs(3600),
            ))
            .expect("claim");
        assert_eq!(claimed_ids(&reclaimed), vec!["evt-crash"]);
        assert_eq!(reclaimed[0].event.state, OutboxState::InFlight);
        match &reclaimed[0].claim {
            OutboxClaimKind::Reclaimed {
                previous_claim_at,
                stale_claim_cutoff,
            } => {
                assert_eq!(previous_claim_at, "2020-01-01T00:00:00.000Z");
                assert!(
                    previous_claim_at <= stale_claim_cutoff,
                    "the receipt must be checkable: {previous_claim_at} vs {stale_claim_cutoff}"
                );
                assert_eq!(stale_claim_cutoff.len(), CANONICAL_UTC_ISO_LEN);
                assert!(stale_claim_cutoff.ends_with('Z'));
            }
            other => panic!("a takeover must not be reported as a first claim: {other:?}"),
        }
        assert!(
            reclaimed[0].event.state_changed_at.as_str() > "2020-01-01T00:00:00.000Z",
            "the takeover must renew the lease"
        );
        assert!(
            store
                .claim_outbox_events(&OutboxClaimRequest::with_reclaim(
                    10,
                    Duration::from_secs(3600)
                ))
                .expect("claim")
                .is_empty(),
            "the renewed lease is no longer stale"
        );
    }

    /// Claim everything drainable and return the batch.
    fn drain(store: &mut MemoryStore) -> Vec<ClaimedOutboxEvent> {
        store
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(16))
            .expect("claim")
    }

    fn evidence() -> OutboxOutcomeEvidence {
        OutboxOutcomeEvidence::from_reporter("peer_alpha")
    }

    /// The whole ordinary cycle, end to end: a committed mutation rests as
    /// backlog, a drain hands it out, and a reported acceptance lands it.
    #[test]
    fn a_full_protocol_walk_runs_pending_to_claimed_to_acknowledged() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-walk", "evt-walk");

        let claimed = drain(&mut store);
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].event.state, OutboxState::InFlight);

        let receipt = store
            .apply_outbox_outcome("evt-walk", &OutboxOutcome::Acknowledged, &evidence())
            .expect("acknowledge");
        assert_eq!(receipt.prior_state, OutboxState::InFlight);
        assert_eq!(receipt.event.state, OutboxState::Acknowledged);
        assert_eq!(receipt.application, OutboxOutcomeApplication::Applied);
        assert_eq!(receipt.event.last_error_class, None);
        assert_eq!(receipt.evidence, evidence());
        assert_eq!(receipt.event.created_at, claimed[0].event.created_at);
        assert!(
            receipt.event.state_changed_at >= claimed[0].event.state_changed_at,
            "the stamp must not move backwards"
        );
        assert_eq!(
            store.outbox_event("evt-walk").expect("read").unwrap(),
            receipt.event,
            "the receipt must equal what a later reader sees"
        );
        assert!(
            drain(&mut store).is_empty(),
            "an acknowledged event is not re-drainable"
        );
    }

    /// A redelivered acknowledgement is normal traffic, not an error — and the
    /// receipt says which delivery it was.
    #[test]
    fn a_duplicate_acknowledgement_is_an_idempotent_no_op_that_moves_no_stamp() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-dup-ack", "evt-dup-ack");
        drain(&mut store);

        let first = store
            .apply_outbox_outcome("evt-dup-ack", &OutboxOutcome::Acknowledged, &evidence())
            .expect("first acknowledgement");
        let second = store
            .apply_outbox_outcome(
                "evt-dup-ack",
                &OutboxOutcome::Acknowledged,
                &OutboxOutcomeEvidence::from_reporter("peer_beta"),
            )
            .expect("a duplicate acknowledgement must not be an error");

        assert_eq!(first.application, OutboxOutcomeApplication::Applied);
        assert_eq!(second.application, OutboxOutcomeApplication::AlreadyApplied);
        assert_eq!(second.prior_state, OutboxState::Acknowledged);
        assert_eq!(second.event.state, OutboxState::Acknowledged);
        assert_eq!(
            second.event, first.event,
            "the no-op must not rewrite a single column, including the stamp"
        );
        assert_eq!(
            second.evidence.reported_by, "peer_beta",
            "the receipt echoes this call's evidence, not the first call's"
        );
    }

    /// The same rule for a rejection: re-reporting it is a no-op, but changing
    /// the story is refused rather than overwritten.
    #[test]
    fn an_outcome_that_differs_from_the_recorded_one_is_refused_not_overwritten() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-differs", "evt-differs");
        drain(&mut store);
        let rejected = store
            .apply_outbox_outcome(
                "evt-differs",
                &OutboxOutcome::Rejected {
                    error_class: "schema_refused".to_string(),
                },
                &evidence(),
            )
            .expect("rejection");
        assert_eq!(
            rejected.event.last_error_class.as_deref(),
            Some("schema_refused")
        );

        let repeat = store
            .apply_outbox_outcome(
                "evt-differs",
                &OutboxOutcome::Rejected {
                    error_class: "schema_refused".to_string(),
                },
                &evidence(),
            )
            .expect("the identical rejection is idempotent");
        assert_eq!(repeat.application, OutboxOutcomeApplication::AlreadyApplied);

        for contradiction in [
            OutboxOutcome::Acknowledged,
            OutboxOutcome::Rejected {
                error_class: "some_other_reason".to_string(),
            },
            OutboxOutcome::Conflicted {
                error_class: "divergent_revision".to_string(),
            },
        ] {
            let error = store
                .apply_outbox_outcome("evt-differs", &contradiction, &evidence())
                .expect_err("a contradicting outcome must be refused");
            match &error {
                MemoryError::OutboxOutcomeRefused {
                    reason,
                    event_id,
                    state,
                } => {
                    assert_eq!(*reason, OutboxOutcomeRefusal::OutcomeAlreadyDiffers);
                    assert_eq!(event_id, "evt-differs");
                    assert_eq!(state, "rejected");
                }
                other => panic!("unexpected error variant: {other:?}"),
            }
        }
        assert_eq!(
            store.outbox_event("evt-differs").expect("read").unwrap(),
            rejected.event,
            "the first recorded outcome must survive every contradiction"
        );
    }

    #[test]
    fn an_outcome_for_an_unknown_event_is_not_found() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let error = store
            .apply_outbox_outcome("evt-nobody", &OutboxOutcome::Acknowledged, &evidence())
            .expect_err("an unknown event must be refused");
        assert!(
            matches!(error, MemoryError::NotFound(_)),
            "unexpected error variant: {error:?}"
        );
    }

    /// Nobody was handed this event, so nobody can report on it. The refusal is
    /// its own reason rather than a generic illegal transition, because it
    /// diagnoses the caller's loop rather than the state machine.
    #[test]
    fn an_outcome_for_a_never_claimed_event_is_refused_as_a_protocol_violation() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-unclaimed", "evt-unclaimed");

        let error = store
            .apply_outbox_outcome("evt-unclaimed", &OutboxOutcome::Acknowledged, &evidence())
            .expect_err("an outcome for a never-claimed event must be refused");
        match &error {
            MemoryError::OutboxOutcomeRefused {
                reason,
                event_id,
                state,
            } => {
                assert_eq!(*reason, OutboxOutcomeRefusal::NeverClaimed);
                assert_eq!(event_id, "evt-unclaimed");
                assert_eq!(state, "pending");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
        assert_eq!(
            store
                .outbox_event("evt-unclaimed")
                .expect("read")
                .unwrap()
                .state,
            OutboxState::Pending,
            "the refusal must write nothing"
        );
    }

    #[test]
    fn an_outcome_for_a_quarantined_event_is_refused_as_withdrawn() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-held", "evt-held");
        store
            .transition_outbox_event("evt-held", OutboxState::Quarantined, Some("operator_hold"))
            .expect("quarantine");

        let error = store
            .apply_outbox_outcome("evt-held", &OutboxOutcome::Acknowledged, &evidence())
            .expect_err("a withdrawn event must not be un-withdrawn by a late acknowledgement");
        match &error {
            MemoryError::OutboxOutcomeRefused { reason, state, .. } => {
                assert_eq!(*reason, OutboxOutcomeRefusal::Withdrawn);
                assert_eq!(state, "quarantined");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    /// A refusal from a consumer never erases the local source event, and never
    /// touches the local object.
    #[test]
    fn a_rejection_records_its_class_and_leaves_the_local_object_untouched() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-reject", "evt-reject");
        let before = store.get("obj-reject").expect("get").expect("present");
        drain(&mut store);

        let receipt = store
            .apply_outbox_outcome(
                "evt-reject",
                &OutboxOutcome::Rejected {
                    error_class: "remote_refused".to_string(),
                },
                &OutboxOutcomeEvidence {
                    reported_by: "peer_alpha".to_string(),
                    peer_revision: Some(7),
                    peer_payload_digest: None,
                },
            )
            .expect("rejection");
        assert_eq!(receipt.event.state, OutboxState::Rejected);
        assert_eq!(
            receipt.event.last_error_class.as_deref(),
            Some("remote_refused")
        );
        assert_eq!(receipt.evidence.peer_revision, Some(7));

        let after = store.get("obj-reject").expect("get").expect("present");
        assert_eq!(after.revision, before.revision);
        assert_eq!(
            outbox_payload_digest(&after).expect("digest"),
            outbox_payload_digest(&before).expect("digest"),
            "a rejection must not rewrite local memory"
        );
        assert_eq!(
            store
                .outbox_health()
                .expect("health")
                .last_error_class
                .as_deref(),
            Some("remote_refused")
        );
    }

    /// The tokens a caller reports are a vocabulary, not a message channel —
    /// the same rule the storage layer enforces, applied before any lock is
    /// taken.
    #[test]
    fn reported_tokens_are_validated_as_tokens_not_free_text() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-tokens", "evt-tokens");
        drain(&mut store);

        let oversized = OutboxOutcome::Rejected {
            error_class: "c".repeat(crate::db::MAX_OUTBOX_CLASS_BYTES + 1),
        };
        assert!(store
            .apply_outbox_outcome("evt-tokens", &oversized, &evidence())
            .is_err());
        let multiline = OutboxOutcome::Rejected {
            error_class: "remote said:\nstack trace follows".to_string(),
        };
        assert!(store
            .apply_outbox_outcome("evt-tokens", &multiline, &evidence())
            .is_err());
        // tachi#1644 review fix: the allowlist is `[a-z0-9_.-]`, not merely
        // "no control characters" — an ordinary space and an uppercase letter
        // are both free text, not a classification token.
        let spaced = OutboxOutcome::Rejected {
            error_class: "remote refused".to_string(),
        };
        assert!(
            store
                .apply_outbox_outcome("evt-tokens", &spaced, &evidence())
                .is_err(),
            "a space is not in the [a-z0-9_.-] allowlist"
        );
        let uppercase = OutboxOutcome::Rejected {
            error_class: "Remote_Refused".to_string(),
        };
        assert!(
            store
                .apply_outbox_outcome("evt-tokens", &uppercase, &evidence())
                .is_err(),
            "uppercase ascii is not in the [a-z0-9_.-] allowlist"
        );
        assert!(store
            .apply_outbox_outcome(
                "evt-tokens",
                &OutboxOutcome::Acknowledged,
                &OutboxOutcomeEvidence::from_reporter("   ")
            )
            .is_err());
        assert!(
            store
                .apply_outbox_outcome(
                    "evt-tokens",
                    &OutboxOutcome::Acknowledged,
                    &OutboxOutcomeEvidence {
                        reported_by: "peer_alpha".to_string(),
                        peer_revision: None,
                        peer_payload_digest: Some("not-a-digest".to_string()),
                    }
                )
                .is_err(),
            "a peer digest that is not a canonical SHA-256 is not a digest"
        );

        assert_eq!(
            store
                .outbox_event("evt-tokens")
                .expect("read")
                .unwrap()
                .state,
            OutboxState::InFlight,
            "every validation refusal must leave the event exactly where it was"
        );
    }

    /// Commit an object, drain it, and have a consumer report a conflict.
    fn conflict(store: &mut MemoryStore, object_id: &str, event_id: &str) -> OutboxEventRow {
        commit(store, object_id, event_id);
        drain(store);
        store
            .apply_outbox_outcome(
                event_id,
                &OutboxOutcome::Conflicted {
                    error_class: "divergent_revision".to_string(),
                },
                &OutboxOutcomeEvidence {
                    reported_by: "peer_alpha".to_string(),
                    peer_revision: Some(41),
                    peer_payload_digest: None,
                },
            )
            .expect("conflict")
            .event
    }

    /// The local mutation stands: a NEW event carries it, the old event is
    /// consumed with a class saying which decision consumed it, and the
    /// successor announces the object as it is *now*.
    #[test]
    fn local_wins_consumes_the_conflicted_event_and_enqueues_a_lineage_successor() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let conflicted = conflict(&mut store, "obj-lw", "evt-lw");

        // The local object moves on after the conflict was reported.
        store
            .upsert(&entry("obj-lw", "body rewritten after the conflict"))
            .expect("local update");

        let receipt = store
            .resolve_outbox_conflict("evt-lw", &OutboxConflictResolution::LocalWins)
            .expect("local wins");
        let (resolved, successor) = match receipt {
            OutboxConflictResolutionReceipt::LocalWins {
                resolved,
                successor,
            } => (resolved, successor),
            other => panic!("unexpected receipt shape: {other:?}"),
        };

        assert_eq!(resolved.event_id, "evt-lw");
        assert_eq!(resolved.state, OutboxState::Quarantined);
        assert_eq!(
            resolved.last_error_class.as_deref(),
            Some(OUTBOX_LOCAL_WINS_RESOLVED_CLASS)
        );
        assert_eq!(
            resolved.payload_digest, conflicted.payload_digest,
            "the consumed event's history must not be rewritten"
        );
        assert_eq!(resolved.source_revision, conflicted.source_revision);

        assert_eq!(successor.event_id, outbox_local_wins_successor_id("evt-lw"));
        assert_eq!(successor.event_id, "evt-lw::local-wins");
        assert_eq!(successor.state, OutboxState::Pending);
        assert_eq!(successor.last_error_class, None);
        assert_eq!(successor.object_id, "obj-lw");
        assert_eq!(successor.object_class, conflicted.object_class);
        assert_eq!(successor.authority_class, conflicted.authority_class);
        assert_eq!(successor.source_store, conflicted.source_store);
        assert_eq!(successor.source_partition, conflicted.source_partition);

        let stored = store.get("obj-lw").expect("get").expect("present");
        assert_eq!(
            successor.payload_digest,
            outbox_payload_digest(&stored).expect("digest"),
            "the successor announces the object as it is now"
        );
        assert!(
            successor.source_revision > conflicted.source_revision,
            "the successor carries the current revision, not the conflicted one: {} vs {}",
            successor.source_revision,
            conflicted.source_revision
        );
        assert_ne!(successor.payload_digest, conflicted.payload_digest);

        // And it is ordinary drainable work again.
        let claimed = drain(&mut store);
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].event.event_id, "evt-lw::local-wins");
        assert_eq!(claimed[0].claim, OutboxClaimKind::First);
    }

    /// "RemoteWins" withdraws the local *event*. It does not write the peer's
    /// version over local memory — importing a peer's payload is a memory write
    /// on its own terms, never a side effect of resolving a conflict.
    #[test]
    fn remote_wins_withdraws_the_local_event_without_rewriting_local_memory() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        conflict(&mut store, "obj-rw", "evt-rw");
        let before = store.get("obj-rw").expect("get").expect("present");

        let receipt = store
            .resolve_outbox_conflict(
                "evt-rw",
                &OutboxConflictResolution::RemoteWins {
                    error_class: "peer_authority_wins".to_string(),
                },
            )
            .expect("remote wins");
        let quarantined = match receipt {
            OutboxConflictResolutionReceipt::RemoteWins { quarantined } => quarantined,
            other => panic!("unexpected receipt shape: {other:?}"),
        };
        assert_eq!(quarantined.state, OutboxState::Quarantined);
        assert_eq!(
            quarantined.last_error_class.as_deref(),
            Some("conflict_resolved_remote_wins.peer_authority_wins"),
            "the stored class carries the resolved-conflict prefix plus the caller's own reason"
        );

        let after = store.get("obj-rw").expect("get").expect("present");
        assert_eq!(after.revision, before.revision);
        assert_eq!(
            outbox_payload_digest(&after).expect("digest"),
            outbox_payload_digest(&before).expect("digest"),
            "no resolution may overwrite local memory with a peer's version"
        );
        assert!(
            store
                .outbox_event("evt-rw::local-wins")
                .expect("read")
                .is_none(),
            "RemoteWins re-enqueues nothing"
        );
        let health = store.outbox_health().expect("health");
        assert_eq!(
            health.local_store_status,
            db::LocalStoreStatus::Healthy,
            "a RemoteWins resolution is a decided conflict, not a live local degradation"
        );
        assert_eq!(health.resolved_count, 1);
    }

    #[test]
    fn a_deferred_resolution_writes_nothing_and_is_repeatable() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let conflicted = conflict(&mut store, "obj-def", "evt-def");

        for _ in 0..2 {
            let receipt = store
                .resolve_outbox_conflict("evt-def", &OutboxConflictResolution::Deferred)
                .expect("deferred");
            match receipt {
                OutboxConflictResolutionReceipt::Deferred { unresolved } => {
                    assert_eq!(
                        unresolved, conflicted,
                        "postponing a decision must not move a single column, including the stamp"
                    );
                }
                other => panic!("unexpected receipt shape: {other:?}"),
            }
        }
        assert_eq!(
            store.outbox_event("evt-def").expect("read").unwrap(),
            conflicted
        );
    }

    #[test]
    fn a_resolution_of_an_event_that_is_not_conflicted_is_refused() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-notcon", "evt-notcon");

        for state_token in ["pending", "acknowledged"] {
            if state_token == "acknowledged" {
                drain(&mut store);
                store
                    .apply_outbox_outcome("evt-notcon", &OutboxOutcome::Acknowledged, &evidence())
                    .expect("acknowledge");
            }
            let error = store
                .resolve_outbox_conflict("evt-notcon", &OutboxConflictResolution::LocalWins)
                .expect_err("only a conflicted event can be resolved");
            match &error {
                MemoryError::OutboxOutcomeRefused { reason, state, .. } => {
                    assert_eq!(*reason, OutboxOutcomeRefusal::NotConflicted);
                    assert_eq!(state, state_token);
                }
                other => panic!("unexpected error variant: {other:?}"),
            }
            assert!(
                store
                    .outbox_event("evt-notcon::local-wins")
                    .expect("read")
                    .is_none(),
                "a refused resolution must enqueue nothing"
            );
        }
    }

    /// A replayed decision cannot fork the lineage: the first resolution
    /// consumed the event, so the second is refused by state — and even if the
    /// state check were bypassed, the derived successor id is already taken.
    #[test]
    fn a_replayed_resolution_cannot_produce_a_second_successor() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        conflict(&mut store, "obj-replay", "evt-replay");
        store
            .resolve_outbox_conflict("evt-replay", &OutboxConflictResolution::LocalWins)
            .expect("first resolution");

        let error = store
            .resolve_outbox_conflict("evt-replay", &OutboxConflictResolution::LocalWins)
            .expect_err("a replayed resolution must be refused");
        match &error {
            MemoryError::OutboxOutcomeRefused { reason, state, .. } => {
                assert_eq!(*reason, OutboxOutcomeRefusal::NotConflicted);
                assert_eq!(
                    state, "quarantined",
                    "the refusal must name the state the first decision left behind"
                );
            }
            other => panic!("unexpected error variant: {other:?}"),
        }

        let events: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memory_outbox_events WHERE object_id = 'obj-replay'",
                [],
                |row| row.get(0),
            )
            .expect("count events");
        assert_eq!(events, 2, "exactly one successor, however many replays");
    }

    /// #1630's acceptance anchor, walked: the same `event_id` redelivered
    /// through the whole cycle cannot double-apply. The refusal lands at the
    /// insert (A1's primary key) — before any second memory write becomes
    /// durable, not after.
    #[test]
    fn replaying_a_completed_event_id_is_refused_at_insert_with_no_second_memory_write() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-replay-cycle", "evt-replay-cycle");
        drain(&mut store);
        let acknowledged = store
            .apply_outbox_outcome(
                "evt-replay-cycle",
                &OutboxOutcome::Acknowledged,
                &evidence(),
            )
            .expect("acknowledge");

        let before = store
            .get("obj-replay-cycle")
            .expect("get")
            .expect("present");
        let error = store
            .commit_with_outbox_event(
                &entry("obj-replay-cycle", "a different body arriving on replay"),
                &meta("evt-replay-cycle"),
            )
            .expect_err("a completed event_id must be refused at insert");
        assert!(
            matches!(error, MemoryError::Duplicate(_)),
            "unexpected error variant: {error:?}"
        );

        let after = store
            .get("obj-replay-cycle")
            .expect("get")
            .expect("present");
        assert_eq!(
            after.revision, before.revision,
            "the refused replay must not write memory a second time"
        );
        assert_eq!(
            outbox_payload_digest(&after).expect("digest"),
            outbox_payload_digest(&before).expect("digest")
        );
        assert_eq!(
            store
                .outbox_event("evt-replay-cycle")
                .expect("read")
                .unwrap(),
            acknowledged.event,
            "the recorded outcome must survive the replay untouched"
        );

        let redelivered = store
            .apply_outbox_outcome(
                "evt-replay-cycle",
                &OutboxOutcome::Acknowledged,
                &evidence(),
            )
            .expect("a redelivered acknowledgement is not an error");
        assert_eq!(
            redelivered.application,
            OutboxOutcomeApplication::AlreadyApplied
        );
        assert!(
            drain(&mut store).is_empty(),
            "nothing is re-drainable after a replay"
        );
    }

    /// The duplicate delivery a takeover deliberately creates: the new holder
    /// acknowledges, then the presumed-dead original holder's acknowledgement
    /// finally arrives. It must land as a no-op, not a second application.
    #[test]
    fn a_late_acknowledgement_after_a_takeover_applies_once() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        commit(&mut store, "obj-late", "evt-late");
        drain(&mut store);
        age_claim(&store, "evt-late", "2020-01-01T00:00:00.000Z");
        let reclaimed = store
            .claim_outbox_events(&OutboxClaimRequest::with_reclaim(
                10,
                Duration::from_secs(60),
            ))
            .expect("reclaim");
        assert_eq!(reclaimed.len(), 1);

        let by_new_holder = store
            .apply_outbox_outcome(
                "evt-late",
                &OutboxOutcome::Acknowledged,
                &OutboxOutcomeEvidence::from_reporter("peer_beta"),
            )
            .expect("the new holder acknowledges");
        let by_original_holder = store
            .apply_outbox_outcome(
                "evt-late",
                &OutboxOutcome::Acknowledged,
                &OutboxOutcomeEvidence::from_reporter("peer_alpha"),
            )
            .expect("the late acknowledgement is not an error");

        assert_eq!(by_new_holder.application, OutboxOutcomeApplication::Applied);
        assert_eq!(
            by_original_holder.application,
            OutboxOutcomeApplication::AlreadyApplied
        );
        assert_eq!(by_original_holder.event, by_new_holder.event);
    }

    /// The six health fields through the whole protocol, one step at a time.
    /// The derivation itself is pinned by `db::outbox`'s tests; what this
    /// walks is that the protocol's own moves land where the read model says
    /// they do.
    #[test]
    fn health_reflects_the_distribution_through_the_whole_protocol_walk() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let empty = store.outbox_health().expect("health");
        assert_eq!(empty.remote_sync_status, db::RemoteSyncStatus::Idle);
        assert_eq!(empty.local_store_status, db::LocalStoreStatus::Healthy);

        commit(&mut store, "obj-a", "evt-a");
        commit(&mut store, "obj-b", "evt-b");
        commit(&mut store, "obj-c", "evt-c");
        let queued = store.outbox_health().expect("health");
        assert_eq!(
            queued.remote_sync_status,
            db::RemoteSyncStatus::Backlogged { pending_count: 3 }
        );
        assert_eq!(queued.pending_count, 3);

        let claimed = store
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(2))
            .expect("claim");
        assert_eq!(claimed.len(), 2);
        let drained = store.outbox_health().expect("health");
        assert_eq!(
            drained.remote_sync_status,
            db::RemoteSyncStatus::InFlight { in_flight_count: 2 },
            "live work outranks the remaining backlog"
        );
        assert_eq!(drained.pending_count, 1);
        assert_eq!(drained.last_successful_sync, None);

        let acknowledged = store
            .apply_outbox_outcome("evt-a", &OutboxOutcome::Acknowledged, &evidence())
            .expect("acknowledge");
        let partly = store.outbox_health().expect("health");
        assert_eq!(
            partly.remote_sync_status,
            db::RemoteSyncStatus::InFlight { in_flight_count: 1 }
        );
        assert_eq!(
            partly.last_successful_sync.as_deref(),
            Some(acknowledged.event.state_changed_at.as_str())
        );

        store
            .apply_outbox_outcome(
                "evt-b",
                &OutboxOutcome::Conflicted {
                    error_class: "divergent_revision".to_string(),
                },
                &evidence(),
            )
            .expect("conflict");
        let conflicted = store.outbox_health().expect("health");
        assert_eq!(
            conflicted.remote_sync_status,
            db::RemoteSyncStatus::Backlogged { pending_count: 1 },
            "the untouched third event still outranks a historical failure"
        );
        assert_eq!(
            conflicted.last_error_class.as_deref(),
            Some("divergent_revision")
        );

        store
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(10))
            .expect("claim the rest");
        store
            .apply_outbox_outcome(
                "evt-c",
                &OutboxOutcome::Rejected {
                    error_class: "remote_refused".to_string(),
                },
                &evidence(),
            )
            .expect("rejection");
        let failing = store.outbox_health().expect("health");
        assert_eq!(
            failing.remote_sync_status,
            db::RemoteSyncStatus::Failing {
                rejected_count: 1,
                conflicted_count: 1
            }
        );
        assert_eq!(failing.pending_count, 0);
        assert_eq!(failing.local_store_status, db::LocalStoreStatus::Healthy);

        // Deciding the conflict turns it into new work, and marks the consumed
        // event as withdrawn — so `conflicted` keeps meaning "still awaiting a
        // decision".
        store
            .resolve_outbox_conflict("evt-b", &OutboxConflictResolution::LocalWins)
            .expect("local wins");
        let resolved = store.outbox_health().expect("health");
        assert_eq!(
            resolved.remote_sync_status,
            db::RemoteSyncStatus::Backlogged { pending_count: 1 }
        );
        assert_eq!(
            resolved.local_store_status,
            db::LocalStoreStatus::Healthy,
            "tachi#1644 review fix: a decided conflict is not a live local degradation"
        );
        assert_eq!(
            resolved.resolved_count, 1,
            "the decision is still durably counted, just not flagged as degradation"
        );
        assert_eq!(
            resolved.last_error_class.as_deref(),
            Some(OUTBOX_LOCAL_WINS_RESOLVED_CLASS)
        );

        store
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(10))
            .expect("claim the successor");
        let successor_ack = store
            .apply_outbox_outcome(
                &outbox_local_wins_successor_id("evt-b"),
                &OutboxOutcome::Acknowledged,
                &evidence(),
            )
            .expect("acknowledge the successor");
        let settled = store.outbox_health().expect("health");
        assert_eq!(
            settled.remote_sync_status,
            db::RemoteSyncStatus::Failing {
                rejected_count: 1,
                conflicted_count: 0
            },
            "the decided conflict no longer counts as an unresolved one"
        );
        assert_eq!(
            settled.last_successful_sync.as_deref(),
            Some(successor_ack.event.state_changed_at.as_str())
        );
        assert_eq!(settled.pending_count, 0);
        assert_eq!(settled.oldest_pending_at, None);
    }

    /// The cutoff is minted with the same formatter every stamp in the table
    /// uses. If that ever drifts, lexical comparison stops being chronological
    /// comparison and the staleness bound silently changes meaning.
    #[test]
    fn the_staleness_cutoff_is_canonical_and_an_unrepresentable_bound_is_refused() {
        let cutoff = stale_claim_cutoff(Duration::from_secs(60)).expect("cutoff");
        assert_eq!(cutoff.len(), CANONICAL_UTC_ISO_LEN);
        assert!(cutoff.ends_with('Z'));
        assert_eq!(&cutoff[10..11], "T");
        assert_eq!(&cutoff[19..20], ".");
        assert!(
            cutoff < crate::db::now_utc_iso(),
            "a positive bound must move the cutoff into the past"
        );

        let error = stale_claim_cutoff(Duration::from_secs(u64::MAX / 2))
            .expect_err("an unrepresentable bound must be refused, not clamped");
        assert!(
            matches!(error, MemoryError::InvalidArg(_)),
            "unexpected error variant: {error:?}"
        );
    }
}

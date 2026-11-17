//! Durable delivery spine (#1679) — the delivery plane of the terminal
//! capability cutover.
//!
//! Three states stay INDEPENDENT (tachi#1636 law, frozen): `execution_state`
//! (its own planes: `dispatch_outcomes`, `harness_session_*`), this
//! `delivery_state`, and `adjudication_state`
//! (`dispatch_adjudications`). "Message not delivered" is never "execution
//! failed": every writer in this module touches ONLY `delivery_intents` and
//! `delivery_events` — structurally no execution or adjudication table is
//! reachable from here, and no writer here can mint or rewrite one.
//!
//! The frozen delivery-state machine (issue #1679 body):
//!
//! ```text
//! not_ready -> ready -> requester_queued -> delivered
//!                    \        |  ^          (delivery is a receipt plane,
//!                     \       |  |           never an execution verdict)
//!                      blocked <-+-- retrying -.
//!                         ^          |          |
//!                         '-- explicit re-arm only -'
//! ```
//!
//! Law enforced here (issue #1679 body + zeroclaw #205 TB-7/TB-13):
//! - **Idempotent on the intent.** Mint, claim, and ack are keyed on
//!   idempotency keys. A requester restart re-claims the SAME intent —
//!   never a duplicate delivery, never a second intent.
//! - **Same key, different content is a typed conflict**
//!   ([`MemoryError::DeliveryIdempotencyConflict`]) — the TB-7
//!   `RequestIdConflict` law; never silent acceptance.
//! - **Stale cannot regress.** A result revision mint at or below the
//!   persisted revision is a no-op reconcile; a corrected (higher) revision
//!   re-arms delivery to `ready` and records a `result_superseded` event.
//!   A stale delivery can never overwrite a newer ref.
//! - **The worker never chooses the user/channel.** No mint, claim, ack,
//!   block, resume, or dismiss input names a destination. The requester
//!   binding comes from ADMITTED identity only, and the delivery policy is
//!   one of the four frozen policies carried on the intent — set by the
//!   terminal planes (server-side), never by worker-reported prose.
//! - **Lost-ack visibility contract.** A crash after send but before ack is
//!   indistinguishable from a crash before send AT THE SPINE: the claim
//!   lease expires, the intent re-arms, and the re-claim carries an
//!   incremented `attempt_count`. Exactly-once VISIBLE delivery therefore
//!   requires the host to dedupe its own sends on the receipt identity
//!   `(delivery_id, attempt_count)` — the spine guarantees the same intent,
//!   never a duplicate lifecycle row.
//! - **Ambiguous send never auto-reduplicates.** An
//!   `ambiguous_send_outcome` blocker parks the intent in `blocked`; claims
//!   pick up `ready`/`retrying` only. Only an affirmative requester re-arm
//!   (`resume_requester_operation`) or a corrected result revision moves a
//!   blocked intent back toward delivery, and each such reopen records a
//!   `transition_debt` event (CurrentTruth reopen law applied to delivery).
//! - **Failure-terminal mint residual (documented).** A transient mint
//!   failure on a failure-terminal recording is WARN-logged and leaves that
//!   outcome without an intent; recovery is a later completion or
//!   re-dispatch of the same dispatch (the idempotent mint then re-runs).
//!   An eager retry branch was deliberately rejected: it minted wrong-store
//!   siblings and could re-arm settled completions.
//! - **Single-writer assumption (documented residual).** Decision reads
//!   and writes are serialized by the server's store lock and by SQLite
//!   transactions; cross-process multi-daemon access to one store file is
//!   outside the deployment topology and outside what the CAS + ledger
//!   design can fully serialize. Identity strength is likewise the local
//!   self_asserted admission bar — per-caller authenticated identity
//!   belongs to the identity plane (#55/#1733 lineage), not this spine.
//! - **Dismiss changes only delivery state.** It never deletes execution
//!   evidence and never writes adjudication.
//! - **No raw private content.** Rows carry refs, digests, and bounded
//!   summaries only; a `private` intent must name an admitted requester at
//!   mint time (fail-closed — an unbound private intent is refused).
//!
//! Every mutation appends to `delivery_events` with a deterministic
//! `event_id`; replays collide with `UNIQUE (delivery_id, event_id)` and are
//! suppressed as idempotent receipts, never re-applied.

use crate::TimestampFormat as _;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::common::now_utc_iso;
use crate::error::MemoryError;

/// Frozen delivery-state vocabulary (#1679). Independent of execution and
/// adjudication dimensions by construction: this enum is produced only in
/// this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryState {
    /// No terminal result exists yet for the intent (implicit when no intent
    /// row exists; never written by the current mint path).
    NotReady,
    /// A terminal result exists and is claimable by an admitted requester.
    Ready,
    /// An admitted requester holds an unexpired claim (in-flight delivery).
    RequesterQueued,
    /// The requester acknowledged delivery. Receipt, not an execution or
    /// adjudication verdict.
    Delivered,
    /// Delivery cannot proceed (typed blocker). Never auto-retried.
    Blocked,
    /// A retryable failure; claimable again once `next_retry_at` is due.
    Retrying,
    /// The requester dismissed the delivery. Changes only delivery state.
    Dismissed,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::Ready => "ready",
            Self::RequesterQueued => "requester_queued",
            Self::Delivered => "delivered",
            Self::Blocked => "blocked",
            Self::Retrying => "retrying",
            Self::Dismissed => "dismissed",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "not_ready" => Ok(Self::NotReady),
            "ready" => Ok(Self::Ready),
            "requester_queued" => Ok(Self::RequesterQueued),
            "delivered" => Ok(Self::Delivered),
            "blocked" => Ok(Self::Blocked),
            "retrying" => Ok(Self::Retrying),
            "dismissed" => Ok(Self::Dismissed),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown delivery state '{other}'"
            ))),
        }
    }
}

/// Frozen delivery policies (#1679). The policy is a property of the intent,
/// set by the terminal plane that minted it from server-owned context; the
/// worker has no input into it and the host reads it to decide presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryPolicy {
    /// Deliver against the requester's current call/operation.
    ReturnToCurrentCall,
    /// Resume the requester operation the work came from.
    ResumeRequesterOperation,
    /// Announce into the requester's admitted session.
    AnnounceRequesterSession,
    /// Deposit the artifact; no visible user message.
    SilentArtifactOnly,
}

impl DeliveryPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReturnToCurrentCall => "return_to_current_call",
            Self::ResumeRequesterOperation => "resume_requester_operation",
            Self::AnnounceRequesterSession => "announce_requester_session",
            Self::SilentArtifactOnly => "silent_artifact_only",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "return_to_current_call" => Ok(Self::ReturnToCurrentCall),
            "resume_requester_operation" => Ok(Self::ResumeRequesterOperation),
            "announce_requester_session" => Ok(Self::AnnounceRequesterSession),
            "silent_artifact_only" => Ok(Self::SilentArtifactOnly),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown delivery policy '{other}'"
            ))),
        }
    }
}

/// Which terminal plane minted the intent. The intent binds the terminal
/// receipt, never re-owns execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryExecutionSource {
    /// A managed run's canonical `dispatch_outcomes` receipt.
    ManagedDispatch,
    /// An attached session's terminal event receipt (#1678 spine).
    AttachedSession,
}

impl DeliveryExecutionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ManagedDispatch => "managed_dispatch",
            Self::AttachedSession => "attached_session",
        }
    }
}

/// Closed delivery event vocabulary. `transition_debt` records a reopen-like
/// re-entry (blocked/retrying -> claimed, delivered -> re-armed by a newer
/// revision) so the recovery history stays visible — the CurrentTruth
/// transition-debt law applied to delivery facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryEventKind {
    IntentCreated,
    ResultReady,
    ResultSuperseded,
    Claimed,
    Delivered,
    Blocked,
    RetryScheduled,
    Dismissed,
    ClaimExpired,
    TransitionDebt,
}

impl DeliveryEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IntentCreated => "intent_created",
            Self::ResultReady => "result_ready",
            Self::ResultSuperseded => "result_superseded",
            Self::Claimed => "claimed",
            Self::Delivered => "delivered",
            Self::Blocked => "blocked",
            Self::RetryScheduled => "retry_scheduled",
            Self::Dismissed => "dismissed",
            Self::ClaimExpired => "claim_expired",
            Self::TransitionDebt => "transition_debt",
        }
    }
}

/// Typed blocker classes for `blocked`/`retrying` intents. The frozen law:
/// `ambiguous_send_outcome` parks in `blocked` (never auto-reduplicated);
/// definitive transport failures take `retrying`.
pub mod blocker_class {
    pub const AMBIGUOUS_SEND_OUTCOME: &str = "ambiguous_send_outcome";
    pub const TRANSPORT_FAILURE: &str = "transport_failure";
    pub const DELIVERY_REVOKED: &str = "delivery_revoked";
    pub const REQUESTER_UNAVAILABLE: &str = "requester_unavailable";

    /// Classes permitted to take the `retrying` state. Anything else
    /// (including the ambiguous-send class) is `blocked`.
    pub const RETRYABLE: &[&str] = &[TRANSPORT_FAILURE, REQUESTER_UNAVAILABLE];
}

/// The admitted requester binding an intent may carry. Every field comes
/// from an ADMITTED identity surface at mint time; none is ever accepted
/// from worker-reported prose. `None` fields mean "not admitted" and stay
/// pull-only (any admitted requester may claim).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeliveryRequesterBinding {
    pub agent_identity_id: Option<String>,
    pub host_identity: Option<String>,
    pub session_ref: Option<String>,
}

/// Input to [`mint_delivery_intent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDeliveryIntent {
    /// Mint idempotency key, derived by the calling terminal plane from the
    /// terminal receipt identity (e.g. `managed:<outcome_id>`,
    /// `attached:<attachment_id>:<event_id>`). Same key + same content =
    /// same intent; same key + different content = typed conflict.
    pub idempotency_key: String,
    pub execution_source: DeliveryExecutionSource,
    /// The terminal receipt's own ref (dispatch id / attachment id).
    pub execution_ref: String,
    pub terminal_receipt_revision: i64,
    pub work_claim_id: Option<String>,
    /// Canonical result artifact/evidence ref (bounded; no raw content).
    pub result_ref: String,
    /// Tachi-minted monotone result revision (TB-13).
    pub result_revision: i64,
    /// Digest of the bounded result payload. Never the payload.
    pub payload_digest: String,
    pub visibility_class: DeliveryVisibilityClass,
    pub delivery_policy: DeliveryPolicy,
    pub protocol_capability: String,
    pub requester: DeliveryRequesterBinding,
    /// Optional absolute expiry of the intent itself.
    pub expires_at: Option<String>,
    /// Terminal-plane correction authority: when set, an equal-revision
    /// mint carrying DIFFERENT content supersedes (strictly grows the
    /// result revision) inside the mint transaction instead of hitting the
    /// idempotency conflict. Only Tachi's own terminal planes set this —
    /// it exists so two corrections landing in the same clock tick can
    /// never be silently lost, and the supersede stays serialized by the
    /// write transaction.
    pub correction: bool,
}

/// Visibility of the result ref. A `private` intent MUST bind a requester
/// agent identity at mint time (fail-closed): an unbound private intent
/// would be claimable by any admitted requester.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryVisibilityClass {
    Public,
    Private,
}

impl DeliveryVisibilityClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

/// One durable delivery intent as persisted. Plain data; the row is the
/// receipt of record alongside `delivery_events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryIntent {
    pub delivery_id: String,
    pub idempotency_key: String,
    pub execution_source: String,
    pub execution_ref: String,
    pub terminal_receipt_revision: i64,
    pub work_claim_id: Option<String>,
    pub result_ref: String,
    pub result_revision: i64,
    pub payload_digest: String,
    pub visibility_class: String,
    pub delivery_policy: String,
    pub protocol_capability: String,
    pub requester_agent_identity_id: Option<String>,
    pub requester_host_identity: Option<String>,
    pub requester_session_ref: Option<String>,
    pub delivery_state: String,
    pub blocker_class: Option<String>,
    pub active_claim_key: Option<String>,
    pub claimed_by: Option<String>,
    pub claim_expires_at: Option<String>,
    pub attempt_count: i64,
    pub next_retry_at: Option<String>,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub ready_at: Option<String>,
    pub delivered_at: Option<String>,
    pub dismissed_at: Option<String>,
    /// Monotone compare-and-swap revision for seam calls.
    pub revision: i64,
    pub updated_at: String,
}

/// Bounded delivery view handed to a claiming host. Refs and digests only —
/// the spine has no raw content to leak, and unauthorized callers receive no
/// existence signal for intents they cannot claim (they are filtered out of
/// the result set entirely).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryClaimView {
    pub delivery_id: String,
    pub result_ref: String,
    pub result_revision: i64,
    pub payload_digest: String,
    pub visibility_class: String,
    pub delivery_policy: String,
    pub protocol_capability: String,
    pub execution_source: String,
    pub execution_ref: String,
    pub work_claim_id: Option<String>,
    pub requester_session_ref: Option<String>,
    pub revision: i64,
    pub attempt_count: i64,
}

impl DeliveryClaimView {
    fn from_intent(intent: &DeliveryIntent) -> Self {
        Self {
            delivery_id: intent.delivery_id.clone(),
            result_ref: intent.result_ref.clone(),
            result_revision: intent.result_revision,
            payload_digest: intent.payload_digest.clone(),
            visibility_class: intent.visibility_class.clone(),
            delivery_policy: intent.delivery_policy.clone(),
            protocol_capability: intent.protocol_capability.clone(),
            execution_source: intent.execution_source.clone(),
            execution_ref: intent.execution_ref.clone(),
            work_claim_id: intent.work_claim_id.clone(),
            requester_session_ref: intent.requester_session_ref.clone(),
            revision: intent.revision,
            attempt_count: intent.attempt_count,
        }
    }
}

/// Admitted requester identity a seam call binds (server-verified admission
/// BEFORE this layer; this module re-enforces the private-match rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryCaller {
    pub agent_identity_id: String,
    pub host_identity: String,
}

/// Input to [`claim_ready_delivery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryClaimRequest {
    pub caller: DeliveryCaller,
    /// Claim idempotency key. Replaying the same key for the same intent
    /// returns the same receipt with no state change and no attempt bump.
    pub claim_key: String,
    /// Claim lease TTL seconds (requester crash window). Zero/negative uses
    /// the default.
    pub lease_seconds: i64,
    /// Only claim this delivery id (used by resume-driven re-claim).
    pub only_delivery_id: Option<String>,
}

pub const DEFAULT_CLAIM_LEASE_SECONDS: i64 = 300;

/// Outcome of a claim attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryClaimOutcome {
    /// The intent was claimed (or re-claimed after its own claim expired).
    Claimed(DeliveryClaimView),
    /// The same claim key was replayed for the same intent: the original
    /// receipt, no state change, no duplicate event, no attempt bump.
    ReplayedClaim(DeliveryClaimView),
    /// Nothing claimable for this caller. No hidden-count signal.
    NoneReady,
}

/// Outcome of an acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryAckOutcome {
    /// First acknowledged delivery.
    Acknowledged { revision: i64 },
    /// Deterministic suppression of a replayed or duplicate ack.
    AlreadyDelivered,
}

/// Mint (or reconcile) one durable delivery intent for a terminal receipt.
///
/// - Same idempotency key + same content: returns the persisted intent
///   unchanged (idempotent mint).
/// - Same key + different digest/refs at the SAME revision: typed conflict,
///   nothing written.
/// - Same key + higher `result_revision`: the corrected result re-arms
///   delivery (`ready`), records `result_superseded`, and records
///   `transition_debt` when it re-opens a non-`ready` state (reopen-like).
/// - Same key + lower/equal revision content: stale reconcile no-op.
pub fn mint_delivery_intent(
    conn: &Connection,
    new: &NewDeliveryIntent,
) -> Result<DeliveryIntent, MemoryError> {
    validate_mint(new)?;
    let now = now_utc_iso();
    let delivery_id = delivery_id_for_key(&new.idempotency_key);

    if let Some(existing) = find_intent_by_key(conn, &new.idempotency_key)? {
        return reconcile_mint(conn, existing, new, &now);
    }

    // Row + ledger events land atomically: a mint is never observable as a
    // bare row without its receipts.
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO delivery_intents (
            delivery_id, idempotency_key, execution_source, execution_ref,
            terminal_receipt_revision, work_claim_id, result_ref, result_revision,
            payload_digest, visibility_class, delivery_policy, protocol_capability,
            requester_agent_identity_id, requester_host_identity, requester_session_ref,
            delivery_state, blocker_class, attempt_count, expires_at,
            created_at, ready_at, revision, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                   'ready', NULL, 0, ?16, ?17, ?17, 1, ?17)",
        params![
            delivery_id,
            new.idempotency_key,
            new.execution_source.as_str(),
            new.execution_ref,
            new.terminal_receipt_revision,
            new.work_claim_id,
            new.result_ref,
            new.result_revision,
            new.payload_digest,
            new.visibility_class.as_str(),
            new.delivery_policy.as_str(),
            new.protocol_capability,
            new.requester.agent_identity_id,
            new.requester.host_identity,
            new.requester.session_ref,
            new.expires_at,
            now,
        ],
    )?;

    append_event(
        &tx,
        &delivery_id,
        &format!("intent_created:{delivery_id}"),
        DeliveryEventKind::IntentCreated,
        None,
        Some("durable delivery intent minted from a terminal receipt"),
        Some(&new.payload_digest),
        "tachi",
        &now,
    )?;
    append_event(
        &tx,
        &delivery_id,
        &format!("ready:{delivery_id}:{}", new.result_revision),
        DeliveryEventKind::ResultReady,
        None,
        Some("terminal result ready for requester delivery"),
        Some(&new.payload_digest),
        "tachi",
        &now,
    )?;
    tx.commit()?;

    find_intent_by_id(conn, &delivery_id)?
        .ok_or_else(|| MemoryError::Internal("minted delivery intent not found".to_string()))
}

/// Claim ready work for an ADMITTED requester (host seam:
/// `claim_ready_delivery`).
///
/// Claimable: `ready` intents, and `retrying` intents whose
/// `next_retry_at` is due, where the intent's requester binding matches the
/// caller (exact agent identity match when bound; public intents may be
/// claimed by any admitted caller; host identity must match when bound).
///
/// A stale claim (requester restart window) is released first: a
/// `requester_queued` intent whose claim expired is set back to `ready`
/// with a `claim_expired` event, then becomes claimable again — the SAME
/// intent, never a duplicate.
pub fn claim_ready_delivery(
    conn: &Connection,
    request: &DeliveryClaimRequest,
) -> Result<DeliveryClaimOutcome, MemoryError> {
    validate_caller(&request.caller)?;
    if request.claim_key.trim().is_empty() || request.claim_key.len() > 128 {
        return Err(MemoryError::InvalidArg(
            "claim_key must be 1..=128 characters".to_string(),
        ));
    }
    let now = now_utc_iso();
    let lease = if request.lease_seconds > 0 {
        request.lease_seconds
    } else {
        DEFAULT_CLAIM_LEASE_SECONDS
    };
    let claim_expires_at = lease_expiration(&now, lease)?;

    // Claim-key resolution comes FIRST, before any claim path: the same key
    // replayed for the SAME intent must return the original receipt
    // (replay-safe claim), and the same key found on ANY other intent is a
    // typed conflict — never a second delivery.
    let replayed = resolve_claim_key_replay(conn, request)?;
    if let Some(outcome) = replayed {
        return Ok(outcome);
    }

    release_expired_claims_for_caller(conn, &request.caller, &now)?;

    if let Some(delivery_id) = &request.only_delivery_id {
        let intent = find_intent_by_id(conn, delivery_id)?
            .ok_or_else(|| MemoryError::NotFound(format!("delivery intent {delivery_id}")))?;
        if !claimable_state(&intent, &now) || !requester_matches(&intent, &request.caller) {
            return Ok(DeliveryClaimOutcome::NoneReady);
        }
        return transition_to_claimed(conn, intent, request, &now, &claim_expires_at);
    }

    let mut candidates = load_claimable_intents(conn, &request.caller, &now)?;
    candidates.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then(a.delivery_id.cmp(&b.delivery_id))
    });
    let Some(intent) = candidates.into_iter().next() else {
        return Ok(DeliveryClaimOutcome::NoneReady);
    };
    transition_to_claimed(conn, intent, request, &now, &claim_expires_at)
}

/// Resolve a claim key against recorded claim events. Returns:
/// - `Some(ReplayedClaim)` — the key was recorded for the SAME intent which
///   still holds an unexpired claim from this key: the original receipt,
///   zero writes (deterministic suppression);
/// - `Some(NoneReady)` — the key was recorded for an intent bound to a
///   different requester; the caller learns nothing about it;
/// - `None` — the key is fresh.
///
/// A key recorded on intent X but replayed with content pointing at intent
/// Y (or after X's claim was released/advanced) is a typed conflict.
fn resolve_claim_key_replay(
    conn: &Connection,
    request: &DeliveryClaimRequest,
) -> Result<Option<DeliveryClaimOutcome>, MemoryError> {
    let event_id = format!("claim:{}", request.claim_key);
    let recorded: Option<String> = conn
        .query_row(
            "SELECT delivery_id FROM delivery_events WHERE event_id = ?1 LIMIT 1",
            params![event_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(recorded_id) = recorded else {
        return Ok(None);
    };
    if let Some(pinned) = &request.only_delivery_id {
        if pinned != &recorded_id {
            return Err(MemoryError::DeliveryIdempotencyConflict(format!(
                "claim_key already recorded against delivery intent {recorded_id}"
            )));
        }
    }
    let intent = find_intent_by_id(conn, &recorded_id)?
        .ok_or_else(|| MemoryError::Internal("claim event without intent".to_string()))?;
    if !requester_matches(&intent, &request.caller) {
        // A foreign key must not reveal another requester's intent.
        return Ok(Some(DeliveryClaimOutcome::NoneReady));
    }
    if intent.active_claim_key.as_deref() == Some(request.claim_key.as_str())
        && intent.delivery_state == DeliveryState::RequesterQueued.as_str()
    {
        return Ok(Some(DeliveryClaimOutcome::ReplayedClaim(
            DeliveryClaimView::from_intent(&intent),
        )));
    }
    Err(MemoryError::DeliveryIdempotencyConflict(format!(
        "claim_key already recorded against delivery intent {recorded_id}"
    )))
}

/// Acknowledge delivery of a claimed intent (host seam: `ack_delivered`).
///
/// Only the holder recorded on the active claim may ack, and only from
/// `requester_queued`. A replayed or duplicate ack on an already-delivered
/// intent is deterministically suppressed: `AlreadyDelivered`, no state
/// change, no duplicate event. A lost ack (crash before ack) is recovered
/// by the claim-expiry release + re-claim of the SAME intent.
pub fn ack_delivered(
    conn: &Connection,
    delivery_id: &str,
    caller: &DeliveryCaller,
    ack_key: &str,
    expected_revision: Option<i64>,
) -> Result<DeliveryAckOutcome, MemoryError> {
    validate_caller(caller)?;
    if ack_key.trim().is_empty() || ack_key.len() > 128 {
        return Err(MemoryError::InvalidArg(
            "ack_key must be 1..=128 characters".to_string(),
        ));
    }
    let now = now_utc_iso();
    let intent = find_intent_by_id(conn, delivery_id)?
        .ok_or_else(|| MemoryError::NotFound("delivery intent not found".to_string()))?;

    // Authorization BEFORE state disclosure: a caller outside the intent's
    // requester binding gets the same generic refusal as a missing intent —
    // not an AlreadyDelivered/queued distinction that leaks existence.
    if !requester_matches(&intent, caller) {
        return Err(MemoryError::NotFound(
            "delivery intent not found".to_string(),
        ));
    }
    if intent.delivery_state == DeliveryState::Delivered.as_str() {
        return Ok(DeliveryAckOutcome::AlreadyDelivered);
    }
    if intent.delivery_state != DeliveryState::RequesterQueued.as_str() {
        return Err(MemoryError::DeliveryIncompatibleState(format!(
            "ack_delivered requires requester_queued, intent {delivery_id} is {}",
            intent.delivery_state
        )));
    }
    if intent.claimed_by.as_deref() != Some(caller.host_identity.as_str()) {
        return Err(MemoryError::DeliveryIncompatibleState(format!(
            "ack_delivered refused: intent {delivery_id} is claimed by {:?}, caller is {}",
            intent.claimed_by.unwrap_or_default(),
            caller.host_identity
        )));
    }
    if let Some(expected) = expected_revision {
        if intent.revision != expected {
            return Err(MemoryError::DeliveryRevisionConflict(format!(
                "intent {delivery_id} is at revision {}, caller expected {expected}",
                intent.revision
            )));
        }
    }

    // Ack-key replay for this same intent would collide with the recorded
    // delivered event; suppress deterministically.
    let ack_event_id = format!("ack:{ack_key}");
    let already: bool = conn
        .query_row(
            "SELECT 1 FROM delivery_events WHERE delivery_id = ?1 AND event_id = ?2",
            params![delivery_id, ack_event_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if already {
        return Ok(DeliveryAckOutcome::AlreadyDelivered);
    }

    let next_revision = intent.revision + 1;
    // State + event write atomically, and the update is CAS-guarded on the
    // revision it read: a concurrent writer cannot slip between the check
    // and the write.
    super::common::with_composable_write(conn, |conn| {
        let updated = conn.execute(
            "UPDATE delivery_intents
             SET delivery_state = 'delivered', delivered_at = ?2, revision = ?3,
                 active_claim_key = NULL, claimed_by = NULL, claim_expires_at = NULL,
                 blocker_class = NULL, next_retry_at = NULL, updated_at = ?2
             WHERE delivery_id = ?1 AND revision = ?4",
            params![delivery_id, now, next_revision, intent.revision],
        )?;
        if updated == 0 {
            return Err(MemoryError::DeliveryRevisionConflict(format!(
                "intent {delivery_id} moved during ack (expected revision {})",
                intent.revision
            )));
        }
        if let Err(error) = append_event(
            conn,
            delivery_id,
            &ack_event_id,
            DeliveryEventKind::Delivered,
            Some(intent.revision),
            Some("requester acknowledged delivery"),
            None,
            &caller.host_identity,
            &now,
        ) {
            // Ack-key cross-intent collision at the global partial-unique index
            // (SQLite names the columns, not the index) is the same frozen law
            // as claims: typed conflict, transaction rolled back, nothing
            // written.
            let message = error.to_string();
            if matches!(error, MemoryError::Sqlite(_))
                && (message.contains("UNIQUE constraint failed: delivery_events.event_id")
                    || message.contains("idx_delivery_events_ack_key_global"))
            {
                return Err(MemoryError::DeliveryIdempotencyConflict(format!(
                    "ack_key '{ack_key}' was recorded concurrently elsewhere"
                )));
            }
            return Err(error);
        }
        Ok(DeliveryAckOutcome::Acknowledged {
            revision: next_revision,
        })
    })
}

/// Report a delivery failure (host seam: `reject_or_block`).
///
/// `blocker_class` decides the landing state: retryable classes take
/// `retrying` with a `next_retry_at` schedule; everything else — including
/// an ambiguous send outcome — takes `blocked`, which claims never pick up
/// automatically. Only the claim holder may report from
/// `requester_queued`. Delivery truth never rewrites execution truth: this
/// writes delivery rows only.
pub fn reject_or_block(
    conn: &Connection,
    delivery_id: &str,
    caller: &DeliveryCaller,
    blocker_class: &str,
    detail: Option<&str>,
    retry_in_seconds: Option<i64>,
    expected_revision: Option<i64>,
) -> Result<DeliveryIntent, MemoryError> {
    validate_caller(caller)?;
    if blocker_class.trim().is_empty() || blocker_class.len() > 128 {
        return Err(MemoryError::InvalidArg(
            "blocker_class must be 1..=128 characters".to_string(),
        ));
    }
    // Validate the event detail BEFORE any state mutates: a refused report
    // must leave both the intent and the ledger untouched (atomicity law).
    validate_detail(detail)?;
    let now = now_utc_iso();
    let intent = find_intent_by_id(conn, delivery_id)?
        .ok_or_else(|| MemoryError::NotFound("delivery intent not found".to_string()))?;

    // Authorization BEFORE state disclosure (same law as ack).
    if !requester_matches(&intent, caller) {
        return Err(MemoryError::NotFound(
            "delivery intent not found".to_string(),
        ));
    }
    // Holder-only: only the requester holding the active claim may report a
    // failure. An unrelated admitted caller cannot park (or spy on) another
    // requester's delivery — including a private one.
    if intent.delivery_state != DeliveryState::RequesterQueued.as_str() {
        return Err(MemoryError::DeliveryIncompatibleState(format!(
            "reject_or_block requires requester_queued, intent {delivery_id} is {}",
            intent.delivery_state
        )));
    }
    if intent.claimed_by.as_deref() != Some(caller.host_identity.as_str()) {
        return Err(MemoryError::DeliveryIncompatibleState(format!(
            "reject_or_block refused: intent {delivery_id} is claimed by {:?}, caller is {}",
            intent.claimed_by.unwrap_or_default(),
            caller.host_identity
        )));
    }
    if let Some(expected) = expected_revision {
        if intent.revision != expected {
            return Err(MemoryError::DeliveryRevisionConflict(format!(
                "intent {delivery_id} is at revision {}, caller expected {expected}",
                intent.revision
            )));
        }
    }

    let retryable = blocker_class::RETRYABLE.contains(&blocker_class);
    let next_state = if retryable {
        DeliveryState::Retrying
    } else {
        DeliveryState::Blocked
    };
    let next_retry_at = if retryable {
        Some(lease_expiration(
            &now,
            retry_in_seconds.unwrap_or(0).max(1),
        )?)
    } else {
        None
    };
    let next_revision = intent.revision + 1;
    super::common::with_composable_write(conn, |conn| {
        let updated = conn.execute(
            "UPDATE delivery_intents
             SET delivery_state = ?2, blocker_class = ?3, next_retry_at = ?4,
                 revision = ?5, updated_at = ?6,
                 active_claim_key = NULL, claimed_by = NULL, claim_expires_at = NULL
             WHERE delivery_id = ?1 AND revision = ?7",
            params![
                delivery_id,
                next_state.as_str(),
                blocker_class,
                next_retry_at,
                next_revision,
                now,
                intent.revision
            ],
        )?;
        if updated == 0 {
            return Err(MemoryError::DeliveryRevisionConflict(format!(
                "intent {delivery_id} moved during reject_or_block (expected revision {})",
                intent.revision
            )));
        }
        append_event(
            conn,
            delivery_id,
            &format!("reject:{}:{now}", next_state.as_str()),
            if retryable {
                DeliveryEventKind::RetryScheduled
            } else {
                DeliveryEventKind::Blocked
            },
            Some(intent.revision),
            detail,
            None,
            &caller.host_identity,
            &now,
        )?;
        Ok(())
    })?;
    find_intent_by_id(conn, delivery_id)?
        .ok_or_else(|| MemoryError::Internal("blocked delivery intent not found".to_string()))
}

/// Reconnect reconciliation (host seam: `resume_requester_operation`).
///
/// Releases the caller's expired claims back to `ready` (each recording a
/// `claim_expired` event — the lost-ack recovery seam) and returns every
/// live intent bound to this requester, so a restarting requester resolves
/// the SAME durable intents. Blocked intents are REPORTED, never re-armed
/// here: an ambiguous send is re-delivered only after an affirmative
/// requester re-claim or a corrected result revision.
pub fn resume_requester_operation(
    conn: &Connection,
    caller: &DeliveryCaller,
    rearm_blocked: bool,
) -> Result<Vec<DeliveryIntent>, MemoryError> {
    validate_caller(caller)?;
    let now = now_utc_iso();
    release_expired_claims_for_caller(conn, caller, &now)?;

    if rearm_blocked {
        rearm_blocked_intents_for_caller(conn, caller, &now)?;
    }

    let mut stmt = conn.prepare(&format!(
        "{} WHERE (requester_agent_identity_id IS NULL OR requester_agent_identity_id = ?1)
              AND (requester_host_identity IS NULL OR requester_host_identity = ?2)

              AND delivery_state IN ('ready', 'requester_queued', 'blocked', 'retrying')
            ORDER BY created_at, delivery_id",
        select_intent_sql()
    ))?;
    let bound = stmt.query_map(
        params![caller.agent_identity_id, caller.host_identity],
        row_to_intent,
    )?;
    let mut intents = Vec::new();
    for intent in bound {
        intents.push(intent?);
    }
    Ok(intents)
}

/// Affirmative re-arm (host seam: `resume_requester_operation` with
/// re-arm): the requester, having SEEN the blocked warning on reconnect,
/// affirms a fresh delivery attempt of its own blocked intent. This is the
/// only path out of `blocked` besides a corrected result revision, so an
/// ambiguous send is never re-delivered silently. Requester-bound and
/// atomic like every other mutation.
fn rearm_blocked_intents_for_caller(
    conn: &Connection,
    caller: &DeliveryCaller,
    now: &str,
) -> Result<(), MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "{} WHERE delivery_state = 'blocked'
              AND (requester_agent_identity_id IS NULL OR requester_agent_identity_id = ?1)
              AND (requester_host_identity IS NULL OR requester_host_identity = ?2)
",
        select_intent_sql()
    ))?;
    let bound = stmt.query_map(
        params![caller.agent_identity_id, caller.host_identity],
        row_to_intent,
    )?;
    let mut intents = Vec::new();
    for intent in bound {
        intents.push(intent?);
    }
    for intent in intents {
        let next_revision = intent.revision + 1;
        super::common::with_composable_write(conn, |conn| {
            let updated = conn.execute(
                "UPDATE delivery_intents
                 SET delivery_state = 'ready', ready_at = ?2, revision = ?3, updated_at = ?2,
                     blocker_class = NULL, next_retry_at = NULL,
                     active_claim_key = NULL, claimed_by = NULL, claim_expires_at = NULL
                 WHERE delivery_id = ?1 AND delivery_state = 'blocked' AND revision = ?4",
                params![intent.delivery_id, now, next_revision, intent.revision],
            )?;
            if updated == 0 {
                return Ok(());
            }
            append_event(
                conn,
                &intent.delivery_id,
                &format!("rearm:{}:{next_revision}", intent.delivery_id),
                DeliveryEventKind::TransitionDebt,
                Some(intent.revision),
                Some("requester affirmatively re-armed a blocked delivery after reconnect"),
                None,
                &caller.host_identity,
                now,
            )?;
            Ok(())
        })?;
    }
    Ok(())
}

/// Dismiss a delivery (host seam side effect of requester dismissal).
///
/// Changes ONLY delivery state: execution evidence stays readable and no
/// adjudication surface is touched (structurally: this module has no write
/// path to either). Dismissing a `delivered` intent is a typed refusal —
/// there is nothing left to dismiss.
pub fn dismiss_delivery(
    conn: &Connection,
    delivery_id: &str,
    caller: &DeliveryCaller,
    expected_revision: Option<i64>,
) -> Result<DeliveryIntent, MemoryError> {
    validate_caller(caller)?;
    let now = now_utc_iso();
    let intent = find_intent_by_id(conn, delivery_id)?
        .ok_or_else(|| MemoryError::NotFound(format!("delivery intent {delivery_id}")))?;
    // Same requester-match rule as claiming: an intent bound to requester A
    // cannot be dismissed by requester B (no cross-requester writes, and a
    // private intent yields no existence signal — the caller sees not-found
    // upstream before reaching this check).
    if !requester_matches(&intent, caller) {
        return Err(MemoryError::DeliveryIncompatibleState(format!(
            "dismiss refused: intent {delivery_id} is bound to another requester"
        )));
    }
    if intent.delivery_state == DeliveryState::Delivered.as_str() {
        return Err(MemoryError::DeliveryIncompatibleState(format!(
            "delivered intent {delivery_id} cannot be dismissed"
        )));
    }
    if intent.delivery_state == DeliveryState::Dismissed.as_str() {
        return Ok(intent);
    }
    if let Some(expected) = expected_revision {
        if intent.revision != expected {
            return Err(MemoryError::DeliveryRevisionConflict(format!(
                "intent {delivery_id} is at revision {}, caller expected {expected}",
                intent.revision
            )));
        }
    }
    let next_revision = intent.revision + 1;
    super::common::with_composable_write(conn, |conn| {
        let updated = conn.execute(
            "UPDATE delivery_intents
             SET delivery_state = 'dismissed', dismissed_at = ?2, revision = ?3,
                 updated_at = ?2, active_claim_key = NULL, claimed_by = NULL,
                 claim_expires_at = NULL, blocker_class = NULL, next_retry_at = NULL
             WHERE delivery_id = ?1 AND revision = ?4",
            params![delivery_id, now, next_revision, intent.revision],
        )?;
        if updated == 0 {
            return Err(MemoryError::DeliveryRevisionConflict(format!(
                "intent {delivery_id} moved during dismiss (expected revision {})",
                intent.revision
            )));
        }
        append_event(
            conn,
            delivery_id,
            &format!("dismissed:{delivery_id}:{next_revision}"),
            DeliveryEventKind::Dismissed,
            Some(intent.revision),
            Some("delivery dismissed; execution evidence and adjudication unchanged"),
            None,
            &caller.agent_identity_id,
            &now,
        )?;
        Ok(())
    })?;
    find_intent_by_id(conn, delivery_id)?
        .ok_or_else(|| MemoryError::Internal("dismissed delivery intent not found".to_string()))
}

/// Read one intent by its mint idempotency key (terminal-plane helper for
/// content-aware correction revisions).
pub fn find_delivery_intent_by_idempotency_key(
    conn: &Connection,
    idempotency_key: &str,
) -> Result<Option<DeliveryIntent>, MemoryError> {
    find_intent_by_key(conn, idempotency_key)
}

/// Read one intent (projection observer; no existence signal beyond the
/// caller's own authorization, enforced by the server edge).
pub fn get_delivery_intent(
    conn: &Connection,
    delivery_id: &str,
) -> Result<Option<DeliveryIntent>, MemoryError> {
    find_intent_by_id(conn, delivery_id)
}

/// Observe intents bound to a terminal receipt (projection observer for the
/// #1693 WorkReadModel delivery section).
pub fn observe_delivery_for_execution(
    conn: &Connection,
    execution_source: &str,
    execution_ref: &str,
) -> Result<Vec<DeliveryIntent>, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "{} WHERE execution_source = ?1 AND execution_ref = ?2 ORDER BY created_at, delivery_id",
        select_intent_sql()
    ))?;
    let bound = stmt.query_map(params![execution_source, execution_ref], row_to_intent)?;
    let mut intents = Vec::new();
    for intent in bound {
        intents.push(intent?);
    }
    Ok(intents)
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

fn select_intent_sql() -> &'static str {
    "SELECT delivery_id, idempotency_key, execution_source, execution_ref,
            terminal_receipt_revision, work_claim_id, result_ref, result_revision,
            payload_digest, visibility_class, delivery_policy, protocol_capability,
            requester_agent_identity_id, requester_host_identity, requester_session_ref,
            delivery_state, blocker_class, active_claim_key, claimed_by,
            claim_expires_at, attempt_count, next_retry_at, expires_at,
            created_at, ready_at, delivered_at, dismissed_at, revision, updated_at
     FROM delivery_intents"
}

fn row_to_intent(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryIntent> {
    Ok(DeliveryIntent {
        delivery_id: row.get(0)?,
        idempotency_key: row.get(1)?,
        execution_source: row.get(2)?,
        execution_ref: row.get(3)?,
        terminal_receipt_revision: row.get(4)?,
        work_claim_id: row.get(5)?,
        result_ref: row.get(6)?,
        result_revision: row.get(7)?,
        payload_digest: row.get(8)?,
        visibility_class: row.get(9)?,
        delivery_policy: row.get(10)?,
        protocol_capability: row.get(11)?,
        requester_agent_identity_id: row.get(12)?,
        requester_host_identity: row.get(13)?,
        requester_session_ref: row.get(14)?,
        delivery_state: row.get(15)?,
        blocker_class: row.get(16)?,
        active_claim_key: row.get(17)?,
        claimed_by: row.get(18)?,
        claim_expires_at: row.get(19)?,
        attempt_count: row.get(20)?,
        next_retry_at: row.get(21)?,
        expires_at: row.get(22)?,
        created_at: row.get(23)?,
        ready_at: row.get(24)?,
        delivered_at: row.get(25)?,
        dismissed_at: row.get(26)?,
        revision: row.get(27)?,
        updated_at: row.get(28)?,
    })
}

fn find_intent_by_id(
    conn: &Connection,
    delivery_id: &str,
) -> Result<Option<DeliveryIntent>, MemoryError> {
    let mut stmt = conn.prepare(&format!("{} WHERE delivery_id = ?1", select_intent_sql()))?;
    let found = stmt
        .query_row(params![delivery_id], row_to_intent)
        .optional()?;
    Ok(found)
}

fn find_intent_by_key(
    conn: &Connection,
    idempotency_key: &str,
) -> Result<Option<DeliveryIntent>, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "{} WHERE idempotency_key = ?1",
        select_intent_sql()
    ))?;
    let found = stmt
        .query_row(params![idempotency_key], row_to_intent)
        .optional()?;
    Ok(found)
}

/// Deterministic, restart-stable intent id: `di-` + 32 hex of the
/// idempotency key digest. Same intent identity across replays and
/// restarts; no clock input.
fn delivery_id_for_key(idempotency_key: &str) -> String {
    let digest = Sha256::digest(idempotency_key.as_bytes());
    let hex: String = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("");
    format!("di-{}", &hex[..32])
}

fn validate_caller(caller: &DeliveryCaller) -> Result<(), MemoryError> {
    for (name, value) in [
        ("agent_identity_id", &caller.agent_identity_id),
        ("host_identity", &caller.host_identity),
    ] {
        if value.trim().is_empty() || value.len() > 128 {
            return Err(MemoryError::InvalidArg(format!(
                "caller {name} must be 1..=128 characters"
            )));
        }
    }
    Ok(())
}

fn validate_mint(new: &NewDeliveryIntent) -> Result<(), MemoryError> {
    for (name, value) in [
        ("idempotency_key", &new.idempotency_key),
        ("execution_ref", &new.execution_ref),
        ("result_ref", &new.result_ref),
        ("protocol_capability", &new.protocol_capability),
    ] {
        if value.trim().is_empty() {
            return Err(MemoryError::InvalidArg(format!("{name} must not be empty")));
        }
        if value.len() > (if name == "result_ref" { 512 } else { 128 }) {
            return Err(MemoryError::InvalidArg(format!("{name} over length bound")));
        }
    }
    if new.result_revision < 1 {
        return Err(MemoryError::InvalidArg(
            "result_revision must be >= 1".to_string(),
        ));
    }
    if new.terminal_receipt_revision < 0 {
        return Err(MemoryError::InvalidArg(
            "terminal_receipt_revision must be >= 0".to_string(),
        ));
    }
    if new.payload_digest.is_empty()
        || new.payload_digest.len() > 128
        || !new.payload_digest.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'+' | b'=' | b'/' | b'_' | b':' | b'-')
        })
    {
        return Err(MemoryError::InvalidArg(
            "payload_digest must be a bounded opaque digest token".to_string(),
        ));
    }
    if new.visibility_class == DeliveryVisibilityClass::Private
        && new.requester.agent_identity_id.is_none()
    {
        // Fail closed: a private result with no admitted requester binding
        // would be claimable by any admitted caller.
        return Err(MemoryError::InvalidArg(
            "private delivery intents must bind an admitted requester agent identity".to_string(),
        ));
    }
    for value in [
        new.requester.agent_identity_id.as_deref(),
        new.requester.host_identity.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if value.trim().is_empty() || value.len() > 128 {
            return Err(MemoryError::InvalidArg(
                "requester identity fields must be 1..=128 characters".to_string(),
            ));
        }
    }
    if let Some(session_ref) = &new.requester.session_ref {
        if session_ref.trim().is_empty() || session_ref.len() > 256 {
            return Err(MemoryError::InvalidArg(
                "requester session ref must be 1..=256 characters".to_string(),
            ));
        }
    }
    Ok(())
}

fn reconcile_mint(
    conn: &Connection,
    existing: DeliveryIntent,
    new: &NewDeliveryIntent,
    now: &str,
) -> Result<DeliveryIntent, MemoryError> {
    // Content identity: the result ref and digest together decide whether a
    // mint carries new information. The execution identity is fixed by the
    // idempotency key, so it cannot differ on this path.
    let same_content =
        existing.result_ref == new.result_ref && existing.payload_digest == new.payload_digest;

    if new.result_revision < existing.result_revision && !new.correction {
        // STALE: a lower revision without correction authority is older
        // information and can never change the intent.
        return Ok(existing);
    }
    // (A correction-authoritative lower-revision mint falls through: the
    // terminal plane asserts the incoming receipt is the canonical latest,
    // so the supersede below grows the revision to prev+1 — mirroring the
    // plane's truth without ever producing a lower revision than the one
    // it replaces. The attached plane additionally refuses to mint at all
    // when the event revision is below the intent's, so "older payload"
    // cannot slip through there.)
    if new.result_revision == existing.result_revision {
        if same_content {
            return Ok(existing);
        }
        if !new.correction {
            return Err(MemoryError::DeliveryIdempotencyConflict(format!(
                "idempotency_key '{}' already mints intent {} at revision {} with different content",
                new.idempotency_key, existing.delivery_id, existing.result_revision
            )));
        }
        // Terminal-plane correction at an equal wall-clock revision:
        // supersede strictly INSIDE the mint transaction, so two same-tick
        // corrections serialize and none is silently lost.
        return supersede_intent(
            conn,
            existing.clone(),
            new,
            existing.result_revision + 1,
            now,
        );
    }

    // Strictly higher revision (or a correction-authoritative fallthrough
    // from above): the supersede revision floor stays strictly above
    // whatever the intent already carried, so corrections are monotone no
    // matter the incoming hint.
    if same_content {
        // Unchanged content at a higher revision (a delayed replay of the
        // same terminal receipt) carries NO new information: it must not
        // re-arm a settled delivery.
        return Ok(existing);
    }
    let next_result_revision = new.result_revision.max(existing.result_revision + 1);
    supersede_intent(conn, existing, new, next_result_revision, now)
}

/// Re-arm an intent with a corrected result. The whole rewrite + ledger
/// receipts land atomically and the rewrite is CAS-guarded on the revision
/// it reconciled against.
fn supersede_intent(
    conn: &Connection,
    existing: DeliveryIntent,
    new: &NewDeliveryIntent,
    next_result_revision: i64,
    now: &str,
) -> Result<DeliveryIntent, MemoryError> {
    let reopens = existing.delivery_state != DeliveryState::Ready.as_str();
    let next_revision = existing.revision + 1;
    let tx = conn.unchecked_transaction()?;
    let updated = tx.execute(
        "UPDATE delivery_intents
         SET result_ref = ?2, result_revision = ?3, payload_digest = ?4,
             terminal_receipt_revision = ?5, delivery_state = 'ready', ready_at = ?6,
             revision = ?7, updated_at = ?6,
             active_claim_key = NULL, claimed_by = NULL, claim_expires_at = NULL,
             blocker_class = NULL, next_retry_at = NULL
         WHERE delivery_id = ?1 AND revision = ?8",
        params![
            existing.delivery_id,
            new.result_ref,
            next_result_revision,
            new.payload_digest,
            new.terminal_receipt_revision,
            now,
            next_revision,
            existing.revision
        ],
    )?;
    if updated == 0 {
        return Err(MemoryError::DeliveryRevisionConflict(format!(
            "intent {} moved during supersede (expected revision {})",
            existing.delivery_id, existing.revision
        )));
    }
    append_event(
        &tx,
        &existing.delivery_id,
        &format!(
            "superseded:{}:{}",
            existing.delivery_id, next_result_revision
        ),
        DeliveryEventKind::ResultSuperseded,
        Some(existing.revision),
        Some(&format!(
            "corrected result revision {} replaces {}",
            next_result_revision, existing.result_revision
        )),
        Some(&new.payload_digest),
        "tachi",
        now,
    )?;
    if reopens {
        append_event(
            &tx,
            &existing.delivery_id,
            &format!("debt:supersede:{}:{now}", existing.delivery_id),
            DeliveryEventKind::TransitionDebt,
            Some(existing.revision),
            Some(&format!(
                "delivery re-armed from {} by a corrected result revision",
                existing.delivery_state
            )),
            None,
            "tachi",
            now,
        )?;
    }
    tx.commit()?;
    find_intent_by_id(conn, &existing.delivery_id)?
        .ok_or_else(|| MemoryError::Internal("superseded delivery intent not found".to_string()))
}

fn lease_expiration(now: &str, seconds: i64) -> Result<String, MemoryError> {
    let instant = crate::parse_rfc3339_timestamp(now).map_err(|error| {
        MemoryError::Internal(format!("internal clock is not RFC 3339: {error}"))
    })?;
    let expires = instant
        .checked_add(jiff::SignedDuration::from_secs(seconds))
        .map_err(|_| {
            MemoryError::InvalidArg("delivery lease expiry is out of range".to_string())
        })?;
    Ok(expires.to_iso_micros())
}

fn claimable_state(intent: &DeliveryIntent, now: &str) -> bool {
    match intent.delivery_state.as_str() {
        "ready" => true,
        "retrying" => match &intent.next_retry_at {
            Some(at) => instant_or_min(at) <= instant_or_min(now),
            None => true,
        },
        _ => false,
    }
}

fn instant_or_min(ts: &str) -> jiff::Timestamp {
    crate::parse_rfc3339_timestamp(ts).unwrap_or(jiff::Timestamp::MIN)
}

fn requester_matches(intent: &DeliveryIntent, caller: &DeliveryCaller) -> bool {
    // A bound requester identity is an exact-match requirement: a mismatched
    // caller gets no existence signal (filtered out, never an error).
    if let Some(bound) = &intent.requester_agent_identity_id {
        if bound != &caller.agent_identity_id {
            return false;
        }
    }
    if let Some(bound) = &intent.requester_host_identity {
        if bound != &caller.host_identity {
            return false;
        }
    }
    // Managed intents bind the AGENT (the registry identity the owning
    // WorkClaim carried); attached intents additionally bind the admitted
    // host and remote session. Binding strength is therefore the local
    // self_asserted admission bar — per-caller authenticated identity is
    // the identity plane's law, not this spine's.
    true
}

fn load_claimable_intents(
    conn: &Connection,
    caller: &DeliveryCaller,
    now: &str,
) -> Result<Vec<DeliveryIntent>, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "{} WHERE delivery_state IN ('ready', 'retrying')
           AND (requester_agent_identity_id IS NULL OR requester_agent_identity_id = ?1)
           AND (requester_host_identity IS NULL OR requester_host_identity = ?2)
",
        select_intent_sql()
    ))?;
    let bound = stmt.query_map(
        params![caller.agent_identity_id, caller.host_identity],
        row_to_intent,
    )?;
    let mut intents = Vec::new();
    for intent in bound {
        let intent = intent?;
        // Belt and braces: the typed predicate is the law, the SQL is an
        // optimization. Never trust one without the other.
        if claimable_state(&intent, now) && requester_matches(&intent, caller) {
            intents.push(intent);
        }
    }
    Ok(intents)
}

fn release_expired_claims_for_caller(
    conn: &Connection,
    caller: &DeliveryCaller,
    now: &str,
) -> Result<usize, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "{} WHERE delivery_state = 'requester_queued'
           AND (requester_agent_identity_id IS NULL OR requester_agent_identity_id = ?1)
           AND (requester_host_identity IS NULL OR requester_host_identity = ?2)",
        select_intent_sql()
    ))?;
    let bound = stmt.query_map(
        params![caller.agent_identity_id, caller.host_identity],
        row_to_intent,
    )?;
    let mut released = 0;
    for intent in bound {
        let intent = intent?;
        let expired = intent
            .claim_expires_at
            .as_deref()
            .map(|at| instant_or_min(at) < instant_or_min(now))
            .unwrap_or(false);
        if !expired {
            continue;
        }
        release_expired_claim(conn, &intent, now)?;
        released += 1;
    }
    Ok(released)
}

fn release_expired_claim(
    conn: &Connection,
    intent: &DeliveryIntent,
    now: &str,
) -> Result<(), MemoryError> {
    let next_revision = intent.revision + 1;
    super::common::with_composable_write(conn, |conn| {
        let updated = conn.execute(
            "UPDATE delivery_intents
             SET delivery_state = 'ready', ready_at = ?2, revision = ?3, updated_at = ?2,
                 active_claim_key = NULL, claimed_by = NULL, claim_expires_at = NULL
             WHERE delivery_id = ?1 AND delivery_state = 'requester_queued' AND revision = ?4",
            params![intent.delivery_id, now, next_revision, intent.revision],
        )?;
        if updated == 0 {
            // The claim was settled concurrently (acked or re-written): its
            // release receipt would contradict the row — write nothing.
            return Ok(());
        }
        append_event(
            conn,
            &intent.delivery_id,
            &format!("claim_expired:{}:{next_revision}", intent.delivery_id),
            DeliveryEventKind::ClaimExpired,
            Some(intent.revision),
            Some("claim expired without acknowledgement; the same intent is re-claimable"),
            None,
            "tachi",
            now,
        )?;
        Ok(())
    })
}

fn transition_to_claimed(
    conn: &Connection,
    intent: DeliveryIntent,
    request: &DeliveryClaimRequest,
    now: &str,
    claim_expires_at: &str,
) -> Result<DeliveryClaimOutcome, MemoryError> {
    let reopen = intent.delivery_state == DeliveryState::Retrying.as_str();
    let next_revision = intent.revision + 1;
    // State change + canonical receipts land atomically: a claim is never
    // observable as a bare state write without its event(s), so replay
    // resolution can always trust the ledger.
    super::common::with_composable_write(conn, |conn| {
        let updated = conn.execute(
            "UPDATE delivery_intents
             SET delivery_state = 'requester_queued',
                 active_claim_key = ?2, claimed_by = ?3, claim_expires_at = ?4,
                 attempt_count = attempt_count + 1, revision = ?5, updated_at = ?6,
                 next_retry_at = NULL, blocker_class = NULL
             WHERE delivery_id = ?1 AND revision = ?7",
            params![
                intent.delivery_id,
                request.claim_key,
                request.caller.host_identity,
                claim_expires_at,
                next_revision,
                now,
                intent.revision
            ],
        )?;
        if updated == 0 {
            return Err(MemoryError::DeliveryRevisionConflict(format!(
                "intent {} moved during claim (expected revision {})",
                intent.delivery_id, intent.revision
            )));
        }
        if let Err(error) = append_event(
            conn,
            &intent.delivery_id,
            &format!("claim:{}", request.claim_key),
            DeliveryEventKind::Claimed,
            Some(intent.revision),
            Some(&format!("claimed by {}", request.caller.host_identity)),
            None,
            &request.caller.host_identity,
            now,
        ) {
            // The global partial-unique index on claim keys is the race
            // backstop for the pre-transaction replay check: a concurrent
            // claim of the same key on a different intent loses here, inside
            // its own transaction, as a typed conflict.
            // SQLite reports the violated columns, not the partial-index name:
            // the per-intent UNIQUE names both columns, the global guards name
            // event_id alone.
            let message = error.to_string();
            if matches!(error, MemoryError::Sqlite(_))
                && (message.contains("UNIQUE constraint failed: delivery_events.event_id")
                    || message.contains("idx_delivery_events_claim_key_global"))
            {
                return Err(MemoryError::DeliveryIdempotencyConflict(format!(
                    "claim_key '{}' was claimed concurrently elsewhere",
                    request.claim_key
                )));
            }
            return Err(error);
        }
        if reopen {
            append_event(
                conn,
                &intent.delivery_id,
                &format!("debt:claim:{}", request.claim_key),
                DeliveryEventKind::TransitionDebt,
                Some(intent.revision),
                Some("delivery re-entered requester_queued from retrying (reopen-like transition)"),
                None,
                &request.caller.host_identity,
                now,
            )?;
        }
        Ok(())
    })?;
    let claimed = find_intent_by_id(conn, &intent.delivery_id)?
        .ok_or_else(|| MemoryError::Internal("claimed delivery intent not found".to_string()))?;
    Ok(DeliveryClaimOutcome::Claimed(
        DeliveryClaimView::from_intent(&claimed),
    ))
}

/// Event-detail bound validation, callable BEFORE any state mutates so a
/// refused report leaves the intent and ledger untouched.
fn validate_detail(detail: Option<&str>) -> Result<(), MemoryError> {
    if let Some(detail) = detail {
        if detail.is_empty() || detail.len() > 1000 || detail.bytes().any(|b| b == 0) {
            return Err(MemoryError::InvalidArg(
                "delivery event detail must be 1..=1000 bytes without NUL".to_string(),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_event(
    conn: &Connection,
    delivery_id: &str,
    event_id: &str,
    kind: DeliveryEventKind,
    expected_revision: Option<i64>,
    detail: Option<&str>,
    payload_digest: Option<&str>,
    actor: &str,
    now: &str,
) -> Result<(), MemoryError> {
    validate_detail(detail)?;
    conn.execute(
        "INSERT INTO delivery_events (
            delivery_id, event_id, kind, expected_revision, detail,
            payload_digest, actor, occurred_at, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![
            delivery_id,
            event_id,
            kind.as_str(),
            expected_revision,
            detail,
            payload_digest,
            actor,
            now
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;

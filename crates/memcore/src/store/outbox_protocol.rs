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
    error::MemoryError,
    MemoryStore,
};

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
            reclaimed[0].event.state_changed_at > "2020-01-01T00:00:00.000Z",
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

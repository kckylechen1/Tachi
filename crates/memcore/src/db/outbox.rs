//! Durable outbox row CRUD and the typed transition machine for outbound
//! memory mutations (tachi#1643, #1630 workstream A leaf A1).
//!
//! The table this module writes is installed by the v29 sentinel-gated
//! migration (`schema::install_memory_outbox_schema`) and pinned by
//! `schema::validate_memory_outbox_schema`. Read that DDL's doc comment first:
//! it states which columns are caller-supplied, which are derived here, and
//! why the `state` CHECK constraint exists.
//!
//! ## What this layer owns, and what it refuses to own
//!
//! This is **row CRUD plus a state machine**, nothing else. It does not decide
//! what is worth remembering, does not talk to any remote, and does not
//! schedule anything — #1630's contract puts all three outside the kernel.
//! There is no remote transport anywhere in this leaf; every transition out of
//! `pending` is performed by a caller (A2 owns real reconciliation).
//!
//! ## Signature shape: `&Transaction` for writes, `&Connection` for reads
//!
//! Every mutating seam takes `&rusqlite::Transaction<'_>` and is named
//! `_within_tx`, because an outbox event that is not written in the same
//! transaction as the object it announces is precisely the window #1643 exists
//! to close. Read seams take `&Connection`, which a `&Transaction` derefs to,
//! so the same function serves an in-transaction readback and an ordinary
//! read. That split is the existing `db::memory_crud` idiom
//! (`import_snapshot_row_within_tx` takes a transaction, `fetch_by_ids` takes a
//! connection), not a local invention.
//!
//! Everything here is `pub(crate)`: the public surface is the store layer
//! (`crate::store::outbox`), which owns the `BEGIN IMMEDIATE` boundary. A
//! caller holding only these functions could insert an event whose object row
//! was written in a *different* transaction, which is the exact defect the
//! leaf forbids.
//!
//! ## Portable, not product
//!
//! No `#[cfg(feature = "admin")]` anywhere in this module or its registration
//! in `db::mod`. #1630's premise is a host-owned sync loop with no Tachi
//! daemon, so a `StoreProfile::PortableKernel` database carries the outbox and
//! a portable build can drive it.

use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::error::MemoryError;

use super::common::now_utc_iso;

/// Durable lifecycle state of one outbox event (#1630 frozen vocabulary).
///
/// The six tokens here are the same six the v29 `state` CHECK constraint
/// admits. [`OutboxState::as_str`] is the storage authority: the CHECK clause,
/// this enum, and the serde representation below all spell the same tokens,
/// and `schema::validate_memory_outbox_schema` refuses a table whose CHECK has
/// drifted from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxState {
    /// Durably recorded locally, never handed to anyone. The state every
    /// event is born in.
    Pending,
    /// Handed to a consumer that has not reported an outcome yet.
    InFlight,
    /// The consumer accepted it. Terminal except for quarantine.
    Acknowledged,
    /// The consumer refused it. Terminal except for quarantine.
    Rejected,
    /// The consumer reports a divergent state for this object. Terminal
    /// except for quarantine; resolution is a new event, not a rewrite.
    Conflicted,
    /// Withdrawn from the pipeline for operator attention. Reachable from
    /// every state, including itself.
    Quarantined,
}

impl OutboxState {
    /// The exact token stored in `memory_outbox_events.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Acknowledged => "acknowledged",
            Self::Rejected => "rejected",
            Self::Conflicted => "conflicted",
            Self::Quarantined => "quarantined",
        }
    }

    /// Parse a persisted token. An unknown value is an error the caller must
    /// surface (fail-closed, matching `ClaimState::parse`) rather than a
    /// silently-defaulted state — a row the CHECK constraint should have made
    /// impossible means the table was recreated without it, and guessing what
    /// the token meant would launder that into a normal-looking read.
    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "pending" => Ok(Self::Pending),
            "in_flight" => Ok(Self::InFlight),
            "acknowledged" => Ok(Self::Acknowledged),
            "rejected" => Ok(Self::Rejected),
            "conflicted" => Ok(Self::Conflicted),
            "quarantined" => Ok(Self::Quarantined),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown memory_outbox_events state '{other}' (expected 'pending', 'in_flight', \
                 'acknowledged', 'rejected', 'conflicted', or 'quarantined')"
            ))),
        }
    }

    /// The frozen #1643 transition matrix, in full:
    ///
    /// | from | permitted `to` |
    /// |---|---|
    /// | `pending` | `in_flight`, `quarantined` |
    /// | `in_flight` | `acknowledged`, `rejected`, `conflicted`, `quarantined` |
    /// | `acknowledged` | `quarantined` |
    /// | `rejected` | `quarantined` |
    /// | `conflicted` | `quarantined` |
    /// | `quarantined` | `quarantined` |
    ///
    /// Two consequences worth naming, because both are decisions rather than
    /// oversights:
    ///
    /// * There is **no retry edge** (`in_flight -> pending`) and no
    ///   un-quarantine edge. This leaf froze exactly the matrix #1643
    ///   specifies; retry/replay policy belongs to A2's reconciliation, which
    ///   can express a retry as a new event with a new `event_id` without
    ///   rewriting the history of this one.
    /// * `quarantined -> quarantined` is permitted and is an idempotent
    ///   restamp: it advances `state_changed_at` and replaces
    ///   `last_error_class`, so a second quarantine with a new class is not a
    ///   refusal. Every other self-transition (including `pending -> pending`)
    ///   is illegal, because for those a repeat is a caller bug, not a
    ///   re-classification.
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (_, Self::Quarantined)
                | (Self::Pending, Self::InFlight)
                | (
                    Self::InFlight,
                    Self::Acknowledged | Self::Rejected | Self::Conflicted
                )
        )
    }

    /// States that model a failure and therefore require an error class.
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Rejected | Self::Conflicted | Self::Quarantined)
    }
}

impl std::fmt::Display for OutboxState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Upper bound on a classification token (`object_class`, `authority_class`,
/// `last_error_class`).
///
/// These columns are a **vocabulary**, not a message channel. Bounding them
/// keeps the outbox from becoming an accidental content surface the way an
/// unbounded "last error" string would — the same content-free discipline
/// `RecallReplayCompatibilityReason` follows for replay refusals. Identity
/// columns (`event_id`, `object_id`) are deliberately *not* bounded here:
/// `memories.id` carries no length bound anywhere else in this kernel, and
/// inventing one at the outbox seam would refuse commits for rows the store
/// itself accepts.
pub const MAX_OUTBOX_CLASS_BYTES: usize = 64;

/// Length of the lowercase-hex SHA-256 `payload_digest` this table stores.
pub const OUTBOX_PAYLOAD_DIGEST_HEX_LEN: usize = 64;

/// One `memory_outbox_events` row, as stored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxEventRow {
    /// Caller-stable, immutable, unique. The PRIMARY KEY.
    pub event_id: String,
    /// The `memories.id` this event announces.
    pub object_id: String,
    pub object_class: String,
    pub authority_class: String,
    pub source_store: String,
    pub source_partition: String,
    /// `memories.revision` as read from the destination row inside the
    /// enqueueing transaction — never a caller-supplied number.
    pub source_revision: i64,
    /// Lowercase hex SHA-256 over the stored payload. See
    /// `crate::store::outbox` for the exact canonicalization.
    pub payload_digest: String,
    pub state: OutboxState,
    /// Set only while `state` is a failure state; NULL otherwise, so it never
    /// reports a class the current state has moved past.
    pub last_error_class: Option<String>,
    /// Canonical UTC-ISO (millisecond precision + `Z`, tachi#1432).
    pub created_at: String,
    /// Canonical UTC-ISO. Rewritten by every accepted transition.
    pub state_changed_at: String,
}

/// Caller-supplied half of a new outbox event.
///
/// `source_revision` is absent on purpose: it is read from the destination
/// `memories` row inside the same transaction (see
/// [`insert_outbox_event_within_tx`]), so no caller can announce a revision
/// the database does not actually hold. `state`, `created_at` and
/// `state_changed_at` are absent for the same reason — a caller-chosen initial
/// state or timestamp would make the durable order unverifiable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutboxEvent {
    pub event_id: String,
    pub object_id: String,
    pub object_class: String,
    pub authority_class: String,
    pub source_store: String,
    pub source_partition: String,
    /// Lowercase hex SHA-256, computed by the store layer from the payload as
    /// it was actually stored.
    pub payload_digest: String,
}

const OUTBOX_SELECT_COLUMNS: &str = "event_id, object_id, object_class, authority_class, \
     source_store, source_partition, source_revision, payload_digest, state, last_error_class, \
     created_at, state_changed_at";

fn row_to_outbox_event(row: &rusqlite::Row<'_>) -> Result<OutboxEventRow, rusqlite::Error> {
    let state_raw: String = row.get(8)?;
    let state = OutboxState::parse(&state_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            )),
        )
    })?;
    Ok(OutboxEventRow {
        event_id: row.get(0)?,
        object_id: row.get(1)?,
        object_class: row.get(2)?,
        authority_class: row.get(3)?,
        source_store: row.get(4)?,
        source_partition: row.get(5)?,
        source_revision: row.get(6)?,
        payload_digest: row.get(7)?,
        state,
        last_error_class: row.get(9)?,
        created_at: row.get(10)?,
        state_changed_at: row.get(11)?,
    })
}

fn refuse_blank(field: &str, value: &str) -> Result<(), MemoryError> {
    if value.trim().is_empty() {
        return Err(MemoryError::InvalidArg(format!(
            "outbox event {field} must be provided by caller"
        )));
    }
    Ok(())
}

/// Refuse anything that is not a classification token.
///
/// `pub(crate)` rather than private since tachi#1644: the reconciliation
/// protocol validates the tokens a caller reports (an error class, a reporter
/// name) before it opens a transaction, and it must apply *this* rule rather
/// than a second copy of it that could drift from the one the storage layer
/// enforces.
pub(crate) fn refuse_invalid_class(field: &str, value: &str) -> Result<(), MemoryError> {
    refuse_blank(field, value)?;
    if value.len() > MAX_OUTBOX_CLASS_BYTES {
        return Err(MemoryError::InvalidArg(format!(
            "outbox event {field} is {} bytes, over the {MAX_OUTBOX_CLASS_BYTES}-byte limit for a \
             classification token",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(MemoryError::InvalidArg(format!(
            "outbox event {field} must be a classification token, not free text with control \
             characters"
        )));
    }
    Ok(())
}

/// The suffix reserved for a [`OutboxConflictResolution::LocalWins`]
/// resolution's own minted successor id (tachi#1644 review fix).
///
/// Pinned as its own literal rather than imported: `db` is the lower layer
/// and does not depend on `store` (see this module's `//!` doc, "class law"),
/// so this mirrors
/// [`crate::store::outbox_protocol::OUTBOX_LOCAL_WINS_SUCCESSOR_SUFFIX`]
/// rather than referencing it, the same way
/// [`OUTBOX_RESOLVED_CONFLICT_CLASS_LIKE_PATTERN`] mirrors that module's
/// resolved-conflict class constants.
///
/// [`OutboxConflictResolution::LocalWins`]: crate::store::outbox_protocol::OutboxConflictResolution::LocalWins
const OUTBOX_RESERVED_SUCCESSOR_SUFFIX: &str = "::local-wins";

/// Refuse an `event_id` a caller supplied that ends with the reserved
/// successor suffix (tachi#1644 review fix).
///
/// Without this, a caller could mint `"evt-x::local-wins"` directly through
/// the ordinary enqueue path, and a later `LocalWins` resolution of some
/// other conflicted event `"evt-x"` would collide with it on the primary key
/// — a collision [`insert_outbox_event_within_tx`]'s existing duplicate check
/// catches, but only after the caller's own legitimate event already holds
/// the id the kernel needs for a future resolution. Only
/// [`insert_resolution_successor_event_within_tx`] — reached exclusively from
/// `resolve_outbox_conflict`'s `LocalWins` arm, which mints this exact
/// suffix — is exempt from this refusal.
fn refuse_reserved_successor_suffix(field: &str, value: &str) -> Result<(), MemoryError> {
    if value.ends_with(OUTBOX_RESERVED_SUCCESSOR_SUFFIX) {
        return Err(MemoryError::InvalidArg(format!(
            "outbox event {field} '{value}' ends with the reserved successor suffix \
             '{OUTBOX_RESERVED_SUCCESSOR_SUFFIX}', which only a LocalWins conflict resolution's \
             own minted successor id may carry"
        )));
    }
    Ok(())
}

/// Refuse anything that is not a lowercase-hex SHA-256.
///
/// This is a storage-level backstop on the same invariant
/// `crate::store::outbox` establishes by construction: the column is a digest,
/// so a caller (or a future composition seam) cannot quietly park a summary,
/// an error message, or a payload fragment in it.
pub(crate) fn refuse_non_canonical_digest(value: &str) -> Result<(), MemoryError> {
    if value.len() != OUTBOX_PAYLOAD_DIGEST_HEX_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(MemoryError::InvalidArg(format!(
            "outbox event payload_digest must be {OUTBOX_PAYLOAD_DIGEST_HEX_LEN} lowercase hex \
             characters (SHA-256)"
        )));
    }
    Ok(())
}

/// Read the destination object's current revision from inside the enqueueing
/// transaction.
///
/// A missing row is a typed [`MemoryError::NotFound`], and refusing here is
/// what makes "no event without its object" enforceable rather than merely
/// intended: the whole surrounding transaction rolls back, so an event can
/// never become durable announcing an object the database does not hold.
fn read_object_revision_within_tx(
    tx: &Transaction<'_>,
    object_id: &str,
) -> Result<i64, MemoryError> {
    tx.query_row(
        "SELECT revision FROM memories WHERE id = ?1",
        params![object_id],
        |row| row.get::<_, i64>(0),
    )
    .optional()?
    .ok_or_else(|| {
        MemoryError::NotFound(format!(
            "outbox event names object '{object_id}', which has no memories row in this \
             transaction; the object write and its event must share one transaction"
        ))
    })
}

/// Whether an `event_id` is already taken.
pub(crate) fn outbox_event_exists(conn: &Connection, event_id: &str) -> Result<bool, MemoryError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM memory_outbox_events WHERE event_id = ?1",
            params![event_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Insert one `pending` event inside a caller-owned transaction and return the
/// row **as stored**.
///
/// Refusals, in order: blank/oversized/non-canonical caller fields, an
/// `event_id` carrying the suffix reserved for a `LocalWins` successor
/// (tachi#1644 review fix — see [`refuse_reserved_successor_suffix`]), an
/// `event_id` that already exists ([`MemoryError::Duplicate`]), and an
/// `object_id` with no `memories` row in this transaction
/// ([`MemoryError::NotFound`]).
///
/// The `event_id` uniqueness check is a read followed by an insert, which is
/// safe because the caller holds this transaction's `BEGIN IMMEDIATE` writer
/// lock — no other connection can insert between them. The table's PRIMARY KEY
/// remains the actual enforcement; this check exists to turn the constraint
/// violation into a typed refusal naming the duplicated id instead of a bare
/// SQLite error.
///
/// The returned row is read back from the destination rather than assembled
/// from the input, so the caller's receipt reflects what SQLite stored — the
/// tachi#1607 receipt idiom.
///
/// This is the entry point for every **caller-supplied** `event_id`. A
/// resolution's own minted successor id — which legitimately carries the
/// reserved suffix this function refuses — goes through the separate
/// [`insert_resolution_successor_event_within_tx`] entry instead of this one.
pub(crate) fn insert_outbox_event_within_tx(
    tx: &Transaction<'_>,
    event: &NewOutboxEvent,
) -> Result<OutboxEventRow, MemoryError> {
    refuse_reserved_successor_suffix("event_id", &event.event_id)?;
    insert_outbox_event_within_tx_impl(tx, event)
}

/// Insert a [`OutboxConflictResolution::LocalWins`] resolution's own successor
/// event (tachi#1644 review fix).
///
/// Identical to [`insert_outbox_event_within_tx`] except it does **not**
/// apply [`refuse_reserved_successor_suffix`]: this is the one seam whose
/// `event_id` is minted by the kernel itself
/// (`crate::store::outbox_protocol::outbox_local_wins_successor_id`), not
/// supplied by a caller, so the suffix that seam refuses everywhere else is
/// exactly what this insert is expected to carry. Reached from exactly one
/// call site — `resolve_outbox_conflict`'s `LocalWins` arm — through
/// `crate::store::outbox::enqueue_outbox_resolution_successor_event_within_tx`.
/// Every other refusal (`refuse_blank`, `refuse_invalid_class`,
/// `refuse_non_canonical_digest`, the duplicate check, the missing-object
/// check) still applies unchanged.
///
/// [`OutboxConflictResolution::LocalWins`]: crate::store::outbox_protocol::OutboxConflictResolution::LocalWins
pub(crate) fn insert_resolution_successor_event_within_tx(
    tx: &Transaction<'_>,
    event: &NewOutboxEvent,
) -> Result<OutboxEventRow, MemoryError> {
    insert_outbox_event_within_tx_impl(tx, event)
}

fn insert_outbox_event_within_tx_impl(
    tx: &Transaction<'_>,
    event: &NewOutboxEvent,
) -> Result<OutboxEventRow, MemoryError> {
    refuse_blank("event_id", &event.event_id)?;
    refuse_blank("object_id", &event.object_id)?;
    refuse_blank("source_store", &event.source_store)?;
    refuse_blank("source_partition", &event.source_partition)?;
    refuse_invalid_class("object_class", &event.object_class)?;
    refuse_invalid_class("authority_class", &event.authority_class)?;
    refuse_non_canonical_digest(&event.payload_digest)?;

    if outbox_event_exists(tx, &event.event_id)? {
        return Err(MemoryError::Duplicate(format!(
            "outbox event_id '{}' already exists; event ids are caller-stable and immutable, so a \
             replay must reuse the recorded outcome rather than rewrite it",
            event.event_id
        )));
    }

    let source_revision = read_object_revision_within_tx(tx, &event.object_id)?;
    let now = now_utc_iso();
    tx.execute(
        "INSERT INTO memory_outbox_events (event_id, object_id, object_class, authority_class, \
         source_store, source_partition, source_revision, payload_digest, state, \
         last_error_class, created_at, state_changed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?10)",
        params![
            event.event_id,
            event.object_id,
            event.object_class,
            event.authority_class,
            event.source_store,
            event.source_partition,
            source_revision,
            event.payload_digest,
            OutboxState::Pending.as_str(),
            now,
        ],
    )?;

    read_outbox_event(tx, &event.event_id)?.ok_or_else(|| {
        MemoryError::Internal(format!(
            "outbox event '{}' vanished between insert and readback in the same transaction",
            event.event_id
        ))
    })
}

/// Move one event to `next`, or refuse with a typed error.
///
/// * An unknown `event_id` is [`MemoryError::NotFound`].
/// * A transition outside [`OutboxState::can_transition_to`] is
///   [`MemoryError::OutboxIllegalTransition`], carrying both endpoints so a
///   caller can branch on the refusal without parsing prose. Nothing is
///   written: the stored state and `state_changed_at` are untouched.
/// * A failure state ([`OutboxState::is_failure`]) requires `error_class`, and
///   a non-failure state refuses one. This is what keeps
///   `last_error_class` meaningful: it is set exactly when the current state
///   is a failure, and cleared otherwise, so it can never report a class the
///   event has already moved past.
///
/// The UPDATE carries `AND state = <observed>`: inside one transaction the
/// state cannot change under us, so this is not the load-bearing guard — it is
/// a compare-and-swap that makes the read-check-write sequence self-verifying,
/// and it turns any future misuse (a caller reaching this outside a
/// transaction) into a loud `changed != 1` failure instead of a lost update.
pub(crate) fn transition_outbox_event_within_tx(
    tx: &Transaction<'_>,
    event_id: &str,
    next: OutboxState,
    error_class: Option<&str>,
) -> Result<OutboxEventRow, MemoryError> {
    let current = read_outbox_event(tx, event_id)?.ok_or_else(|| {
        MemoryError::NotFound(format!("outbox event '{event_id}' does not exist"))
    })?;

    if !current.state.can_transition_to(next) {
        return Err(MemoryError::OutboxIllegalTransition {
            event_id: event_id.to_string(),
            from: current.state.as_str().to_string(),
            to: next.as_str().to_string(),
        });
    }

    let stored_error_class = match (next.is_failure(), error_class) {
        (true, Some(class)) => {
            refuse_invalid_class("last_error_class", class)?;
            Some(class)
        }
        (true, None) => {
            return Err(MemoryError::InvalidArg(format!(
                "transition of outbox event '{event_id}' to '{next}' requires an error class; a \
                 failure recorded without its class cannot be reported by the health read model"
            )))
        }
        (false, Some(_)) => {
            return Err(MemoryError::InvalidArg(format!(
                "transition of outbox event '{event_id}' to '{next}' must not carry an error \
                 class; '{next}' is not a failure state"
            )))
        }
        (false, None) => None,
    };

    let now = now_utc_iso();
    let changed = tx.execute(
        "UPDATE memory_outbox_events SET state = ?2, last_error_class = ?3, state_changed_at = ?4 \
         WHERE event_id = ?1 AND state = ?5",
        params![
            event_id,
            next.as_str(),
            stored_error_class,
            now,
            current.state.as_str()
        ],
    )?;
    if changed != 1 {
        return Err(MemoryError::Internal(format!(
            "outbox event '{event_id}' changed state under a transaction that had already \
             observed it as '{}'",
            current.state
        )));
    }

    read_outbox_event(tx, event_id)?.ok_or_else(|| {
        MemoryError::Internal(format!(
            "outbox event '{event_id}' vanished between transition and readback in the same \
             transaction"
        ))
    })
}

/// Read one event by its caller-stable id. Takes `&Connection` so the same
/// function serves an in-transaction readback (a `&Transaction` derefs) and an
/// ordinary read.
pub(crate) fn read_outbox_event(
    conn: &Connection,
    event_id: &str,
) -> Result<Option<OutboxEventRow>, MemoryError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {OUTBOX_SELECT_COLUMNS} FROM memory_outbox_events WHERE event_id = ?1"
            ),
            params![event_id],
            row_to_outbox_event,
        )
        .optional()?;
    Ok(row)
}

/// List events in one state, oldest first, bounded by `limit`.
///
/// Order is `created_at ASC, event_id ASC`: enqueue order, with the id as a
/// total tie-break so two events stamped in the same millisecond still come
/// back in a stable order across calls. Lexical ordering of `created_at` is
/// chronological only because every writer here stamps canonical UTC-ISO
/// (tachi#1432) — the same property the health read model depends on.
///
/// `limit` of 0 returns an empty vector rather than a refusal, matching SQL
/// `LIMIT 0`.
pub(crate) fn list_outbox_events_by_state(
    conn: &Connection,
    state: OutboxState,
    limit: usize,
) -> Result<Vec<OutboxEventRow>, MemoryError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {OUTBOX_SELECT_COLUMNS} FROM memory_outbox_events WHERE state = ?1 \
         ORDER BY created_at ASC, event_id ASC LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(
            params![state.as_str(), i64::try_from(limit).unwrap_or(i64::MAX)],
            row_to_outbox_event,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One event a claim moved (or kept) in `in_flight`, with the storage facts
/// the protocol layer needs to name *what kind* of claim it was.
///
/// The `previous_*` fields are the row as this claim observed it before
/// writing, so a receipt built from this can be checked against the durable
/// state rather than asserted: a reclaim carries the stamp it replaced, and a
/// reader can verify that stamp is at or before the cutoff the claim used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimedOutboxRow {
    /// The row as stored after the claim: `state` is
    /// [`OutboxState::InFlight`] and `state_changed_at` is this claim's lease
    /// stamp.
    pub event: OutboxEventRow,
    /// [`OutboxState::Pending`] for a first hand-off, [`OutboxState::InFlight`]
    /// for a takeover of a claim that outlived the caller's staleness bound.
    pub previous_state: OutboxState,
    /// The `state_changed_at` this claim replaced.
    pub previous_state_changed_at: String,
}

/// Renew the lease on an event that is already `in_flight`.
///
/// This is deliberately **not** a transition, and it does not go through
/// [`transition_outbox_event_within_tx`]: `in_flight -> in_flight` is illegal
/// in the frozen #1643 matrix and stays illegal. Nothing about the event's
/// state changes here — the only column touched is `state_changed_at`, which
/// is the lease stamp a staleness bound is measured against.
///
/// Restamping is required for correctness rather than cosmetic: if a takeover
/// left the old stamp in place, the event would remain past the cutoff and
/// every subsequent drain — including the very next one by the same caller —
/// would take it over again, so the bound would stop bounding anything.
///
/// The `AND state = 'in_flight' AND state_changed_at = ?3` clause is a
/// compare-and-swap against the row as observed, for the same reason
/// [`transition_outbox_event_within_tx`] carries one: inside one transaction
/// the row cannot move under us, so a `changed != 1` here means the seam was
/// reached outside a transaction and must fail loudly instead of silently
/// taking over a claim someone else just renewed.
fn renew_outbox_claim_within_tx(
    tx: &Transaction<'_>,
    observed: &OutboxEventRow,
) -> Result<OutboxEventRow, MemoryError> {
    let now = now_utc_iso();
    let changed = tx.execute(
        "UPDATE memory_outbox_events SET state_changed_at = ?2 \
         WHERE event_id = ?1 AND state = 'in_flight' AND state_changed_at = ?3",
        params![observed.event_id, now, observed.state_changed_at],
    )?;
    if changed != 1 {
        return Err(MemoryError::Internal(format!(
            "outbox event '{}' moved under a transaction that had already observed it in flight \
             since {}",
            observed.event_id, observed.state_changed_at
        )));
    }
    read_outbox_event(tx, &observed.event_id)?.ok_or_else(|| {
        MemoryError::Internal(format!(
            "outbox event '{}' vanished between claim renewal and readback in the same transaction",
            observed.event_id
        ))
    })
}

/// Hand a bounded batch of drainable events to one consumer, in one
/// transaction (tachi#1644, #1630 workstream A leaf A2).
///
/// Drainable means either of two things, and the difference is preserved in
/// the returned rows rather than flattened:
///
/// * `pending` — never handed to anyone. Claiming it is the ordinary
///   `pending -> in_flight` edge, taken through the frozen A1 machine.
/// * `in_flight` whose `state_changed_at` is at or before
///   `reclaim_stamped_at_or_before` — a claim whose holder never reported an
///   outcome (the crash case). Claiming it renews the lease via
///   [`renew_outbox_claim_within_tx`]; the state does not change, because a
///   takeover is not a transition.
///
/// `reclaim_stamped_at_or_before` of `None` disables takeover entirely: only
/// `pending` events are claimed. That is the conservative default a caller
/// must opt out of, because taking over another consumer's in-flight event is
/// only safe if the caller can say how long a claim may live.
///
/// The comparison is lexical on canonical UTC-ISO (tachi#1432), which is
/// chronological **only** because every writer in this module stamps that one
/// shape; the caller mints the cutoff with the same formatter. `<=` rather
/// than `<` so a zero-length bound means "every in-flight event is
/// reclaimable", which is the reading a caller passing zero intends.
///
/// Ordering and bounding are `list_outbox_events_by_state`'s: `created_at ASC,
/// event_id ASC`, `LIMIT limit`, and `limit == 0` returns an empty batch
/// rather than a refusal. Candidates are selected first and written after, so
/// no event can appear twice in one batch.
///
/// Terminal events (`acknowledged`, `rejected`, `conflicted`, `quarantined`)
/// are never selected by any bound: an outcome, once reported, is not
/// re-drainable, and a retry is a new event rather than a rewrite of this
/// one's history.
pub(crate) fn claim_outbox_events_within_tx(
    tx: &Transaction<'_>,
    limit: usize,
    reclaim_stamped_at_or_before: Option<&str>,
) -> Result<Vec<ClaimedOutboxRow>, MemoryError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let candidates = {
        let mut stmt = tx.prepare(&format!(
            "SELECT {OUTBOX_SELECT_COLUMNS} FROM memory_outbox_events \
             WHERE state = ?1 \
                OR (state = ?2 AND ?3 IS NOT NULL AND state_changed_at <= ?3) \
             ORDER BY created_at ASC, event_id ASC LIMIT ?4"
        ))?;
        let rows = stmt
            .query_map(
                params![
                    OutboxState::Pending.as_str(),
                    OutboxState::InFlight.as_str(),
                    reclaim_stamped_at_or_before,
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                row_to_outbox_event,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };

    let mut claimed = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let event = match candidate.state {
            OutboxState::Pending => transition_outbox_event_within_tx(
                tx,
                &candidate.event_id,
                OutboxState::InFlight,
                None,
            )?,
            OutboxState::InFlight => renew_outbox_claim_within_tx(tx, &candidate)?,
            other => {
                return Err(MemoryError::Internal(format!(
                    "outbox claim selected event '{}' in state '{other}', which is not drainable",
                    candidate.event_id
                )))
            }
        };
        claimed.push(ClaimedOutboxRow {
            event,
            previous_state: candidate.state,
            previous_state_changed_at: candidate.state_changed_at,
        });
    }
    Ok(claimed)
}

/// Local-store half of the #1643 health read model.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalStoreStatus {
    /// The outbox is readable and no event is quarantined for a reason other
    /// than a resolved conflict (see [`OutboxHealth::resolved_count`]).
    Healthy,
    /// At least one event has been withdrawn for operator attention **and is
    /// still unresolved** (tachi#1644: a conflict a caller already resolved
    /// via [`crate::store::outbox_protocol::OutboxConflictResolution`] does
    /// not count here — it is durably stamped with a
    /// `conflict_resolved_*` class and surfaces in
    /// [`OutboxHealth::resolved_count`] instead). Quarantine is a *local*
    /// condition: it says this store is holding mutations it will not hand
    /// to anyone, which is a durability problem here regardless of what any
    /// remote is doing.
    Quarantined { quarantined_count: u64 },
}

/// Remote-sync half of the #1643 health read model.
///
/// **There is no remote in this leaf.** Nothing in this kernel opens a
/// connection, pushes an event, or receives an acknowledgment; every
/// transition out of `pending` is performed by a caller. So this status is
/// derived *entirely* from the local state distribution and must be read as
/// "what the local queue implies", never as evidence that a remote saw
/// anything. Concretely: [`Self::Drained`] means callers reported every event
/// terminal, and with no A2 sync loop wired up an installation will sit at
/// [`Self::Idle`] or [`Self::Backlogged`] forever, which is the honest answer
/// rather than a fabricated "healthy".
///
/// Derivation, first match wins:
///
/// 1. no rows at all -> [`Self::Idle`]
/// 2. any `in_flight` -> [`Self::InFlight`]
/// 3. any `pending` -> [`Self::Backlogged`]
/// 4. any `rejected` or `conflicted` -> [`Self::Failing`]
/// 5. otherwise -> [`Self::Drained`]
///
/// The ordering is deliberate: live work outranks historical failure, because
/// a queue that is moving is a different operational situation from one that
/// has stopped with failures in it. A store whose only rows are `quarantined`
/// lands on [`Self::Drained`] here and is flagged by
/// [`LocalStoreStatus::Quarantined`] on the other field — quarantine is a
/// local condition, so reporting it as a remote-sync state would misattribute
/// it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSyncStatus {
    /// The outbox has never held an event.
    Idle,
    /// At least one event is with a consumer awaiting an outcome.
    InFlight { in_flight_count: u64 },
    /// Nothing in flight and at least one event waiting. With no sync loop
    /// running, this is the resting state of a store that is recording
    /// mutations nobody is draining.
    Backlogged { pending_count: u64 },
    /// Nothing queued or in flight, and at least one event ended in a
    /// consumer-reported failure.
    Failing {
        rejected_count: u64,
        conflicted_count: u64,
    },
    /// Every event reached a terminal state with no consumer-reported failure.
    Drained,
}

/// The six #1643 health fields, from one consistent snapshot.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxHealth {
    pub local_store_status: LocalStoreStatus,
    pub remote_sync_status: RemoteSyncStatus,
    /// Events in `pending`.
    pub pending_count: u64,
    /// `MIN(created_at)` over `pending` events, or `None` when none are
    /// pending. Chronologically correct because `created_at` is canonical
    /// UTC-ISO, so the lexical minimum is the earliest instant.
    pub oldest_pending_at: Option<String>,
    /// `MAX(state_changed_at)` over `acknowledged` events, or `None`.
    ///
    /// Read this as "when a caller last told this store an event was
    /// accepted", not "when a remote last confirmed anything" — no remote
    /// exists here, and the stamp is written by the local transition seam. It
    /// is `None` on every store that has not had an acknowledgment reported,
    /// which today is every store.
    pub last_successful_sync: Option<String>,
    /// The `last_error_class` of the most recently changed event that carries
    /// one, tie-broken by `event_id` descending for determinism. `None` when
    /// no event is currently in a failure state — the column is cleared on
    /// non-failure transitions, so this never reports a class the outbox has
    /// moved past.
    pub last_error_class: Option<String>,
    /// Quarantined events whose `last_error_class` starts with
    /// `conflict_resolved_` — a conflict a caller already decided through
    /// [`crate::store::outbox_protocol::MemoryStore::resolve_outbox_conflict`],
    /// not an unresolved operator hold (tachi#1644 review fix: before this
    /// field existed, every resolved conflict was indistinguishable from a
    /// live durability problem because both land in `quarantined`). Additive:
    /// `resolved_count` plus the genuinely-quarantined count reported by
    /// [`LocalStoreStatus::Quarantined`] equals the total row count in state
    /// `quarantined`. Never negative, never a subtraction from
    /// `quarantined_count` — a resolved conflict is not excluded from the
    /// table, only from the *degradation* signal.
    pub resolved_count: u64,
}

/// Raw column tuple read back for an outbox row (id, event fields, timestamps, error class).
type OutboxRowColumns = (
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The `LIKE` pattern that marks a `quarantined` row as a resolved conflict
/// rather than a live durability problem (tachi#1644 review fix).
///
/// Every class this pattern is meant to match starts with a Rust constant,
/// not a caller-chosen value:
/// [`crate::store::outbox_protocol::OUTBOX_LOCAL_WINS_RESOLVED_CLASS`]
/// (`"conflict_resolved_local_wins"`, the entire stored value) and
/// [`crate::store::outbox_protocol::OUTBOX_REMOTE_WINS_RESOLVED_CLASS_PREFIX`]
/// (`"conflict_resolved_remote_wins"`, a prefix — the caller's own class
/// follows it). Both prefixes are kernel-fixed strings a caller cannot
/// choose, so this pattern identifies exactly "a conflict this store
/// resolved through `resolve_outbox_conflict`" and nothing a caller could
/// spoof by naming their own quarantine reason similarly — an ordinary
/// operator hold uses a caller-chosen class like `"operator_hold"`, which
/// this pattern does not match.
const OUTBOX_RESOLVED_CONFLICT_CLASS_LIKE_PATTERN: &str = "conflict_resolved_%";

/// Compute all seven health fields in one statement.
///
/// One statement, not several, because the fields are read together and must
/// describe the same instant: two statements on a connection outside a
/// transaction are two snapshots, and a concurrent writer between them could
/// produce a `pending_count` of 0 next to an `oldest_pending_at` of some
/// timestamp — a self-contradictory report. An aggregate query with no
/// `GROUP BY` returns exactly one row even over an empty table, and the
/// correlated subquery for `last_error_class` yields NULL when nothing
/// matches, so the empty-outbox case needs no special path.
pub(crate) fn read_outbox_health(conn: &Connection) -> Result<OutboxHealth, MemoryError> {
    let (
        pending_count,
        in_flight_count,
        acknowledged_count,
        rejected_count,
        conflicted_count,
        quarantined_count,
        resolved_count,
        oldest_pending_at,
        last_successful_sync,
        last_error_class,
    ): OutboxRowColumns = conn.query_row(
        "SELECT
             COALESCE(SUM(state = 'pending'), 0),
             COALESCE(SUM(state = 'in_flight'), 0),
             COALESCE(SUM(state = 'acknowledged'), 0),
             COALESCE(SUM(state = 'rejected'), 0),
             COALESCE(SUM(state = 'conflicted'), 0),
             COALESCE(SUM(state = 'quarantined'
                           AND last_error_class NOT LIKE ?1), 0),
             COALESCE(SUM(state = 'quarantined'
                           AND last_error_class LIKE ?1), 0),
             MIN(CASE WHEN state = 'pending' THEN created_at END),
             MAX(CASE WHEN state = 'acknowledged' THEN state_changed_at END),
             (SELECT last_error_class FROM memory_outbox_events
               WHERE last_error_class IS NOT NULL
               ORDER BY state_changed_at DESC, event_id DESC LIMIT 1)
         FROM memory_outbox_events",
        params![OUTBOX_RESOLVED_CONFLICT_CLASS_LIKE_PATTERN],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        },
    )?;

    let count = |value: i64| u64::try_from(value).unwrap_or(0);
    let pending_count = count(pending_count);
    let in_flight_count = count(in_flight_count);
    let acknowledged_count = count(acknowledged_count);
    let rejected_count = count(rejected_count);
    let conflicted_count = count(conflicted_count);
    let quarantined_count = count(quarantined_count);
    let resolved_count = count(resolved_count);
    let total = pending_count
        + in_flight_count
        + acknowledged_count
        + rejected_count
        + conflicted_count
        + quarantined_count
        + resolved_count;

    let local_store_status = if quarantined_count == 0 {
        LocalStoreStatus::Healthy
    } else {
        LocalStoreStatus::Quarantined { quarantined_count }
    };

    let remote_sync_status = if total == 0 {
        RemoteSyncStatus::Idle
    } else if in_flight_count > 0 {
        RemoteSyncStatus::InFlight { in_flight_count }
    } else if pending_count > 0 {
        RemoteSyncStatus::Backlogged { pending_count }
    } else if rejected_count > 0 || conflicted_count > 0 {
        RemoteSyncStatus::Failing {
            rejected_count,
            conflicted_count,
        }
    } else {
        RemoteSyncStatus::Drained
    };

    Ok(OutboxHealth {
        local_store_status,
        remote_sync_status,
        pending_count,
        oldest_pending_at,
        last_successful_sync,
        last_error_class,
        resolved_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryEntry;

    fn open_conn() -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn memory_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/scratch/outbox".to_string(),
            summary: String::new(),
            text: format!("outbox fixture body for {id}"),
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
            metadata: serde_json::Value::Object(Default::default()),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    /// A syntactically valid lowercase-hex SHA-256. Deliberately built from
    /// the letter half of the alphabet so `to_uppercase` actually changes it —
    /// a digits-only fixture would make the uppercase-refusal test vacuous.
    fn digest(seed: u8) -> String {
        char::from_digit(10 + u32::from(seed % 6), 16)
            .unwrap()
            .to_string()
            .repeat(OUTBOX_PAYLOAD_DIGEST_HEX_LEN)
    }

    fn new_event(event_id: &str, object_id: &str) -> NewOutboxEvent {
        NewOutboxEvent {
            event_id: event_id.to_string(),
            object_id: object_id.to_string(),
            object_class: "memory".to_string(),
            authority_class: "host".to_string(),
            source_store: "global".to_string(),
            source_partition: "default".to_string(),
            payload_digest: digest(1),
        }
    }

    /// Write one memory row and one pending event for it, committing both.
    fn seed_event(conn: &mut Connection, event_id: &str, object_id: &str) -> OutboxEventRow {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        crate::db::upsert_within_tx(&tx, &memory_entry(object_id), false, None).unwrap();
        let row = insert_outbox_event_within_tx(&tx, &new_event(event_id, object_id)).unwrap();
        tx.commit().unwrap();
        row
    }

    fn transition(
        conn: &mut Connection,
        event_id: &str,
        next: OutboxState,
        error_class: Option<&str>,
    ) -> Result<OutboxEventRow, MemoryError> {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let result = transition_outbox_event_within_tx(&tx, event_id, next, error_class);
        if result.is_ok() {
            tx.commit().unwrap();
        }
        result
    }

    fn claim(
        conn: &mut Connection,
        limit: usize,
        reclaim_stamped_at_or_before: Option<&str>,
    ) -> Vec<ClaimedOutboxRow> {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let claimed =
            claim_outbox_events_within_tx(&tx, limit, reclaim_stamped_at_or_before).unwrap();
        tx.commit().unwrap();
        claimed
    }

    fn claimed_ids(claimed: &[ClaimedOutboxRow]) -> Vec<&str> {
        claimed
            .iter()
            .map(|row| row.event.event_id.as_str())
            .collect()
    }

    fn assert_canonical_timestamp(value: &str) {
        assert_eq!(value.len(), 24, "canonical UTC-ISO is 24 chars: {value}");
        assert!(value.ends_with('Z'), "canonical UTC-ISO ends in Z: {value}");
        assert_eq!(&value[10..11], "T", "canonical UTC-ISO has a T: {value}");
        assert_eq!(
            &value[19..20],
            ".",
            "canonical UTC-ISO has millisecond precision: {value}"
        );
    }

    #[test]
    fn insert_reads_back_a_pending_row_with_canonical_timestamps_and_stored_revision() {
        let mut conn = open_conn();
        let row = seed_event(&mut conn, "evt-1", "obj-1");

        assert_eq!(row.event_id, "evt-1");
        assert_eq!(row.object_id, "obj-1");
        assert_eq!(row.state, OutboxState::Pending);
        assert_eq!(row.last_error_class, None);
        assert_eq!(
            row.created_at, row.state_changed_at,
            "a freshly inserted event has never changed state"
        );
        assert_canonical_timestamp(&row.created_at);
        assert_canonical_timestamp(&row.state_changed_at);
        // Read from the destination `memories` row, not from any caller input:
        // the fixture's first write stamps revision 1.
        assert_eq!(row.source_revision, 1);

        let reread = read_outbox_event(&conn, "evt-1").unwrap().unwrap();
        assert_eq!(reread, row, "readback must equal the insert's return value");
    }

    /// The `event_id` is the caller-stable identity #1630 freezes, so a second
    /// insert is a typed refusal, not an overwrite of the recorded outcome.
    #[test]
    fn duplicate_event_id_is_a_typed_duplicate_refusal_that_preserves_the_first_row() {
        let mut conn = open_conn();
        let first = seed_event(&mut conn, "evt-dup", "obj-dup");

        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        crate::db::upsert_within_tx(&tx, &memory_entry("obj-dup-2"), false, None).unwrap();
        let mut second = new_event("evt-dup", "obj-dup-2");
        second.payload_digest = digest(2);
        let error = insert_outbox_event_within_tx(&tx, &second)
            .expect_err("a duplicate event_id must be refused");
        drop(tx);

        assert!(
            matches!(error, MemoryError::Duplicate(_)),
            "unexpected error variant: {error:?}"
        );
        let stored = read_outbox_event(&conn, "evt-dup").unwrap().unwrap();
        assert_eq!(stored, first, "the first event must survive untouched");
    }

    /// An event announcing an object this transaction did not write is refused,
    /// which is the half of the atomicity contract the db layer can enforce on
    /// its own.
    #[test]
    fn event_for_an_object_absent_from_the_transaction_is_refused() {
        let mut conn = open_conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let error = insert_outbox_event_within_tx(&tx, &new_event("evt-orphan", "obj-missing"))
            .expect_err("an event for an absent object must be refused");
        drop(tx);
        assert!(
            matches!(error, MemoryError::NotFound(_)),
            "unexpected error variant: {error:?}"
        );
    }

    #[test]
    fn caller_fields_are_refused_when_blank_oversized_or_not_a_digest() {
        let mut conn = open_conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        crate::db::upsert_within_tx(&tx, &memory_entry("obj-fields"), false, None).unwrap();

        let blank_id = NewOutboxEvent {
            event_id: "   ".to_string(),
            ..new_event("unused", "obj-fields")
        };
        assert!(insert_outbox_event_within_tx(&tx, &blank_id).is_err());

        let oversized = NewOutboxEvent {
            object_class: "c".repeat(MAX_OUTBOX_CLASS_BYTES + 1),
            ..new_event("evt-oversized", "obj-fields")
        };
        assert!(insert_outbox_event_within_tx(&tx, &oversized).is_err());

        let short_digest = NewOutboxEvent {
            payload_digest: "abc".to_string(),
            ..new_event("evt-short-digest", "obj-fields")
        };
        assert!(insert_outbox_event_within_tx(&tx, &short_digest).is_err());

        let uppercase_digest = NewOutboxEvent {
            payload_digest: digest(1).to_uppercase(),
            ..new_event("evt-upper-digest", "obj-fields")
        };
        assert!(
            insert_outbox_event_within_tx(&tx, &uppercase_digest).is_err(),
            "uppercase hex is not the canonical digest form"
        );

        // The whole batch of refusals wrote nothing.
        drop(tx);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_outbox_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    /// tachi#1644 review fix: a caller minting `"<id>::local-wins"` directly
    /// through the ordinary insert path — instead of via a real `LocalWins`
    /// resolution — must be refused, because that id is exactly what a future
    /// resolution of `"<id>"` would need to mint and a caller-owned row
    /// sitting on it first would collide on the primary key.
    #[test]
    fn caller_event_id_carrying_the_reserved_successor_suffix_is_refused() {
        let mut conn = open_conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        crate::db::upsert_within_tx(&tx, &memory_entry("obj-reserved"), false, None).unwrap();

        let error = insert_outbox_event_within_tx(
            &tx,
            &new_event("evt-caller::local-wins", "obj-reserved"),
        )
        .expect_err("a caller-supplied id carrying the reserved suffix must be refused");
        drop(tx);

        assert!(
            matches!(error, MemoryError::InvalidArg(_)),
            "unexpected error variant: {error:?}"
        );
    }

    /// The other half of the same fix: the seam a real `LocalWins` resolution
    /// uses to mint its successor is exempt from the refusal above, because
    /// its id legitimately carries the suffix.
    #[test]
    fn resolution_successor_entry_accepts_the_reserved_suffix_the_caller_entry_refuses() {
        let mut conn = open_conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        crate::db::upsert_within_tx(&tx, &memory_entry("obj-succ"), false, None).unwrap();

        let row = insert_resolution_successor_event_within_tx(
            &tx,
            &new_event("evt-succ::local-wins", "obj-succ"),
        )
        .expect("the resolution-successor entry must accept the reserved suffix");
        assert_eq!(row.event_id, "evt-succ::local-wins");
        tx.commit().unwrap();
    }

    /// The frozen matrix, every ordered pair. This is the test that fails if
    /// anyone widens the state machine (adds a retry edge, an un-quarantine
    /// edge, or a self-transition) without changing the contract.
    #[test]
    fn transition_matrix_is_exactly_the_frozen_contract() {
        use OutboxState::*;
        const ALL: [OutboxState; 6] = [
            Pending,
            InFlight,
            Acknowledged,
            Rejected,
            Conflicted,
            Quarantined,
        ];
        for from in ALL {
            for to in ALL {
                let expected = matches!(
                    (from, to),
                    (_, Quarantined)
                        | (Pending, InFlight)
                        | (InFlight, Acknowledged)
                        | (InFlight, Rejected)
                        | (InFlight, Conflicted)
                );
                assert_eq!(
                    from.can_transition_to(to),
                    expected,
                    "transition {from} -> {to}"
                );
            }
        }
    }

    #[test]
    fn legal_transitions_advance_state_and_stamp_but_illegal_ones_write_nothing() {
        let mut conn = open_conn();
        let inserted = seed_event(&mut conn, "evt-fsm", "obj-fsm");

        // pending -> acknowledged is illegal: it skips in_flight.
        let error = transition(&mut conn, "evt-fsm", OutboxState::Acknowledged, None)
            .expect_err("pending -> acknowledged must be refused");
        match &error {
            MemoryError::OutboxIllegalTransition { event_id, from, to } => {
                assert_eq!(event_id, "evt-fsm");
                assert_eq!(from, "pending");
                assert_eq!(to, "acknowledged");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
        let untouched = read_outbox_event(&conn, "evt-fsm").unwrap().unwrap();
        assert_eq!(
            untouched, inserted,
            "a refused transition must not rewrite state or state_changed_at"
        );

        let in_flight = transition(&mut conn, "evt-fsm", OutboxState::InFlight, None).unwrap();
        assert_eq!(in_flight.state, OutboxState::InFlight);
        assert_eq!(in_flight.created_at, inserted.created_at);
        assert_canonical_timestamp(&in_flight.state_changed_at);
        assert!(
            in_flight.state_changed_at >= inserted.state_changed_at,
            "state_changed_at must not move backwards"
        );

        let acknowledged =
            transition(&mut conn, "evt-fsm", OutboxState::Acknowledged, None).unwrap();
        assert_eq!(acknowledged.state, OutboxState::Acknowledged);
        assert_eq!(acknowledged.last_error_class, None);

        // Terminal except for quarantine.
        assert!(transition(&mut conn, "evt-fsm", OutboxState::InFlight, None).is_err());
        let quarantined = transition(
            &mut conn,
            "evt-fsm",
            OutboxState::Quarantined,
            Some("operator_hold"),
        )
        .unwrap();
        assert_eq!(quarantined.state, OutboxState::Quarantined);
        assert_eq!(
            quarantined.last_error_class.as_deref(),
            Some("operator_hold")
        );
    }

    #[test]
    fn failure_states_require_an_error_class_and_success_states_refuse_one() {
        let mut conn = open_conn();
        seed_event(&mut conn, "evt-class", "obj-class");

        assert!(
            transition(&mut conn, "evt-class", OutboxState::InFlight, Some("boom")).is_err(),
            "in_flight is not a failure state and must refuse an error class"
        );
        transition(&mut conn, "evt-class", OutboxState::InFlight, None).unwrap();
        assert!(
            transition(&mut conn, "evt-class", OutboxState::Rejected, None).is_err(),
            "rejected without a class would leave the health model unable to report it"
        );
        assert!(
            transition(
                &mut conn,
                "evt-class",
                OutboxState::Rejected,
                Some(&"c".repeat(MAX_OUTBOX_CLASS_BYTES + 1))
            )
            .is_err(),
            "an error class is a token, not a message"
        );
        let rejected = transition(
            &mut conn,
            "evt-class",
            OutboxState::Rejected,
            Some("remote_refused"),
        )
        .unwrap();
        assert_eq!(rejected.last_error_class.as_deref(), Some("remote_refused"));
    }

    /// Re-quarantine is the one permitted self-transition: it restamps and
    /// re-classifies rather than refusing.
    #[test]
    fn requarantine_restamps_and_replaces_the_error_class() {
        let mut conn = open_conn();
        seed_event(&mut conn, "evt-quar", "obj-quar");
        let first = transition(
            &mut conn,
            "evt-quar",
            OutboxState::Quarantined,
            Some("first_reason"),
        )
        .unwrap();
        let second = transition(
            &mut conn,
            "evt-quar",
            OutboxState::Quarantined,
            Some("second_reason"),
        )
        .unwrap();
        assert_eq!(second.state, OutboxState::Quarantined);
        assert_eq!(second.last_error_class.as_deref(), Some("second_reason"));
        assert!(second.state_changed_at >= first.state_changed_at);
    }

    #[test]
    fn transition_of_an_unknown_event_is_not_found() {
        let mut conn = open_conn();
        let error = transition(&mut conn, "evt-nope", OutboxState::InFlight, None)
            .expect_err("unknown event must be refused");
        assert!(
            matches!(error, MemoryError::NotFound(_)),
            "unexpected error variant: {error:?}"
        );
    }

    #[test]
    fn list_by_state_is_enqueue_ordered_and_bounded_by_limit() {
        let mut conn = open_conn();
        for index in 0..5 {
            seed_event(&mut conn, &format!("evt-{index}"), &format!("obj-{index}"));
        }
        transition(&mut conn, "evt-2", OutboxState::InFlight, None).unwrap();

        let pending = list_outbox_events_by_state(&conn, OutboxState::Pending, 10).unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|row| row.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["evt-0", "evt-1", "evt-3", "evt-4"]
        );

        let bounded = list_outbox_events_by_state(&conn, OutboxState::Pending, 2).unwrap();
        assert_eq!(bounded.len(), 2);
        assert_eq!(bounded[0].event_id, "evt-0");

        assert!(list_outbox_events_by_state(&conn, OutboxState::Pending, 0)
            .unwrap()
            .is_empty());
        assert_eq!(
            list_outbox_events_by_state(&conn, OutboxState::InFlight, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn health_of_an_empty_outbox_is_idle_and_healthy_with_no_stamps() {
        let conn = open_conn();
        let health = read_outbox_health(&conn).unwrap();
        assert_eq!(health.local_store_status, LocalStoreStatus::Healthy);
        assert_eq!(health.remote_sync_status, RemoteSyncStatus::Idle);
        assert_eq!(health.pending_count, 0);
        assert_eq!(health.oldest_pending_at, None);
        assert_eq!(health.last_successful_sync, None);
        assert_eq!(health.last_error_class, None);
        assert_eq!(health.resolved_count, 0);
    }

    #[test]
    fn health_reports_backlog_oldest_pending_and_the_latest_error_class() {
        let mut conn = open_conn();
        let first = seed_event(&mut conn, "evt-a", "obj-a");
        seed_event(&mut conn, "evt-b", "obj-b");
        seed_event(&mut conn, "evt-c", "obj-c");
        transition(&mut conn, "evt-c", OutboxState::InFlight, None).unwrap();
        transition(
            &mut conn,
            "evt-c",
            OutboxState::Conflicted,
            Some("divergent_revision"),
        )
        .unwrap();

        let health = read_outbox_health(&conn).unwrap();
        assert_eq!(health.pending_count, 2);
        assert_eq!(
            health.oldest_pending_at.as_deref(),
            Some(&*first.created_at)
        );
        assert_eq!(
            health.remote_sync_status,
            RemoteSyncStatus::Backlogged { pending_count: 2 },
            "pending work outranks a historical failure"
        );
        assert_eq!(health.local_store_status, LocalStoreStatus::Healthy);
        assert_eq!(
            health.last_error_class.as_deref(),
            Some("divergent_revision")
        );
        assert_eq!(health.last_successful_sync, None);
        assert_eq!(health.resolved_count, 0);
    }

    #[test]
    fn health_reports_in_flight_ahead_of_pending_and_acknowledged_stamps_the_last_sync() {
        let mut conn = open_conn();
        seed_event(&mut conn, "evt-p", "obj-p");
        seed_event(&mut conn, "evt-q", "obj-q");
        transition(&mut conn, "evt-q", OutboxState::InFlight, None).unwrap();

        let health = read_outbox_health(&conn).unwrap();
        assert_eq!(
            health.remote_sync_status,
            RemoteSyncStatus::InFlight { in_flight_count: 1 }
        );
        assert_eq!(health.pending_count, 1);
        assert_eq!(health.last_successful_sync, None);

        let acknowledged = transition(&mut conn, "evt-q", OutboxState::Acknowledged, None).unwrap();
        transition(&mut conn, "evt-p", OutboxState::InFlight, None).unwrap();
        let acknowledged_p =
            transition(&mut conn, "evt-p", OutboxState::Acknowledged, None).unwrap();
        let health = read_outbox_health(&conn).unwrap();
        assert_eq!(health.remote_sync_status, RemoteSyncStatus::Drained);
        assert_eq!(health.pending_count, 0);
        let newest = acknowledged
            .state_changed_at
            .max(acknowledged_p.state_changed_at);
        assert_eq!(health.last_successful_sync.as_deref(), Some(&*newest));
        assert_eq!(health.last_error_class, None);
        assert_eq!(health.resolved_count, 0);
    }

    #[test]
    fn health_flags_quarantine_as_local_degradation_and_failures_as_remote_sync() {
        let mut conn = open_conn();
        seed_event(&mut conn, "evt-x", "obj-x");
        seed_event(&mut conn, "evt-y", "obj-y");
        transition(&mut conn, "evt-x", OutboxState::InFlight, None).unwrap();
        transition(
            &mut conn,
            "evt-x",
            OutboxState::Rejected,
            Some("schema_refused"),
        )
        .unwrap();
        transition(
            &mut conn,
            "evt-y",
            OutboxState::Quarantined,
            Some("operator_hold"),
        )
        .unwrap();

        let health = read_outbox_health(&conn).unwrap();
        assert_eq!(
            health.local_store_status,
            LocalStoreStatus::Quarantined {
                quarantined_count: 1
            }
        );
        assert_eq!(
            health.remote_sync_status,
            RemoteSyncStatus::Failing {
                rejected_count: 1,
                conflicted_count: 0
            }
        );
        assert_eq!(health.pending_count, 0);
        assert_eq!(health.oldest_pending_at, None);
        assert_eq!(
            health.resolved_count, 0,
            "an operator hold is not a resolved conflict"
        );
    }

    /// tachi#1644 review fix: a conflict a caller resolved through
    /// `resolve_outbox_conflict` lands in `quarantined` exactly like an
    /// operator hold does, but it is not a live durability problem — it is
    /// the durable record of a decision that already landed. The health read
    /// model must tell the two apart by the `conflict_resolved_*` class
    /// prefix, not treat every quarantined row as degradation.
    #[test]
    fn health_excludes_resolved_conflicts_from_local_degradation_but_counts_them() {
        let mut conn = open_conn();
        seed_event(&mut conn, "evt-resolved", "obj-resolved");
        seed_event(&mut conn, "evt-held", "obj-held");

        // A resolved conflict: same terminal state as an operator hold, but
        // stamped with the kernel-fixed class `resolve_outbox_conflict`
        // writes (mirrors OUTBOX_LOCAL_WINS_RESOLVED_CLASS in
        // `store::outbox_protocol` — this layer does not depend on that
        // constant, so the literal is pinned here too).
        transition(
            &mut conn,
            "evt-resolved",
            OutboxState::Quarantined,
            Some("conflict_resolved_local_wins"),
        )
        .unwrap();
        // A genuine, still-unresolved quarantine.
        transition(
            &mut conn,
            "evt-held",
            OutboxState::Quarantined,
            Some("operator_hold"),
        )
        .unwrap();

        let health = read_outbox_health(&conn).unwrap();
        assert_eq!(
            health.local_store_status,
            LocalStoreStatus::Quarantined {
                quarantined_count: 1
            },
            "the resolved conflict must not count toward the degradation flag"
        );
        assert_eq!(
            health.resolved_count, 1,
            "the resolved conflict is reported additively, not silently dropped"
        );
    }

    /// The all-resolved case: every quarantined row is a resolved conflict,
    /// so the store reads back Healthy even though the row count is nonzero.
    #[test]
    fn health_of_an_outbox_with_only_resolved_conflicts_is_healthy() {
        let mut conn = open_conn();
        seed_event(&mut conn, "evt-lw", "obj-lw");
        transition(
            &mut conn,
            "evt-lw",
            OutboxState::Quarantined,
            Some("conflict_resolved_local_wins"),
        )
        .unwrap();

        let health = read_outbox_health(&conn).unwrap();
        assert_eq!(health.local_store_status, LocalStoreStatus::Healthy);
        assert_eq!(health.resolved_count, 1);
    }

    #[test]
    fn claim_hands_out_pending_events_in_enqueue_order_and_bounded_by_limit() {
        let mut conn = open_conn();
        for index in 0..4 {
            seed_event(&mut conn, &format!("evt-{index}"), &format!("obj-{index}"));
        }

        let first = claim(&mut conn, 2, None);
        assert_eq!(claimed_ids(&first), vec!["evt-0", "evt-1"]);
        for row in &first {
            assert_eq!(row.event.state, OutboxState::InFlight);
            assert_eq!(row.previous_state, OutboxState::Pending);
            assert!(row.event.state_changed_at >= row.previous_state_changed_at);
        }

        // With no staleness bound, a second drain never takes what the first
        // one is still holding.
        assert_eq!(
            claimed_ids(&claim(&mut conn, 10, None)),
            vec!["evt-2", "evt-3"]
        );
        assert!(claim(&mut conn, 10, None).is_empty());
        assert!(claim(&mut conn, 0, None).is_empty());
    }

    /// The crash case: an `in_flight` event whose holder never reported an
    /// outcome is re-claimable once it is older than the caller's bound — and
    /// the takeover restamps the lease, so the same bound does not hand it out
    /// again on the very next drain.
    #[test]
    fn claim_reclaims_only_the_in_flight_events_at_or_before_the_cutoff() {
        let mut conn = open_conn();
        seed_event(&mut conn, "evt-stale", "obj-stale");
        seed_event(&mut conn, "evt-live", "obj-live");
        assert_eq!(claim(&mut conn, 10, None).len(), 2);

        // Age one lease deterministically rather than by sleeping.
        conn.execute(
            "UPDATE memory_outbox_events SET state_changed_at = '2020-01-01T00:00:00.000Z' \
             WHERE event_id = 'evt-stale'",
            [],
        )
        .unwrap();

        let reclaimed = claim(&mut conn, 10, Some("2021-01-01T00:00:00.000Z"));
        assert_eq!(claimed_ids(&reclaimed), vec!["evt-stale"]);
        let taken = &reclaimed[0];
        assert_eq!(
            taken.previous_state,
            OutboxState::InFlight,
            "a takeover is not a state change"
        );
        assert_eq!(taken.previous_state_changed_at, "2020-01-01T00:00:00.000Z");
        assert_eq!(taken.event.state, OutboxState::InFlight);
        assert!(
            taken.event.state_changed_at > taken.previous_state_changed_at,
            "the lease must be restamped or the bound stops bounding anything"
        );
        assert!(
            claim(&mut conn, 10, Some("2021-01-01T00:00:00.000Z")).is_empty(),
            "the restamped lease is no longer past the cutoff"
        );
    }

    /// No cutoff, however wide, re-drains an event whose outcome was already
    /// reported. A retry is a new event, not a second hand-off of this one.
    #[test]
    fn claim_never_selects_a_terminal_event_at_any_cutoff() {
        let mut conn = open_conn();
        for (event_id, object_id) in [
            ("evt-ack", "obj-ack"),
            ("evt-rej", "obj-rej"),
            ("evt-con", "obj-con"),
            ("evt-quar", "obj-quar"),
        ] {
            seed_event(&mut conn, event_id, object_id);
        }
        transition(&mut conn, "evt-ack", OutboxState::InFlight, None).unwrap();
        transition(&mut conn, "evt-ack", OutboxState::Acknowledged, None).unwrap();
        transition(&mut conn, "evt-rej", OutboxState::InFlight, None).unwrap();
        transition(
            &mut conn,
            "evt-rej",
            OutboxState::Rejected,
            Some("remote_refused"),
        )
        .unwrap();
        transition(&mut conn, "evt-con", OutboxState::InFlight, None).unwrap();
        transition(
            &mut conn,
            "evt-con",
            OutboxState::Conflicted,
            Some("divergent_revision"),
        )
        .unwrap();
        transition(
            &mut conn,
            "evt-quar",
            OutboxState::Quarantined,
            Some("operator_hold"),
        )
        .unwrap();

        assert!(
            claim(&mut conn, 10, Some("2999-01-01T00:00:00.000Z")).is_empty(),
            "a terminal event is not drainable at any staleness bound"
        );
    }

    /// A state token the CHECK constraint should have made impossible must
    /// fail the read rather than being guessed at.
    #[test]
    fn an_unparseable_stored_state_fails_the_read_closed() {
        assert!(OutboxState::parse("shipped").is_err());
        assert!(OutboxState::parse("Pending").is_err());
        assert_eq!(OutboxState::parse("pending").unwrap(), OutboxState::Pending);
    }
}

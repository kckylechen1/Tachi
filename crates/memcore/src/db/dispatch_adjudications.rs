//! Append-only leader adjudication events for dispatch outcomes (#1035).

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;

use super::common::normalize_utc_iso_or_now;

/// Closed set of legal reason codes for a `not_required` adjudication row
/// (#1035). A `not_required` verdict's reason MUST be one of these — an open
/// free-text field would let every caller mint a new code, defeating the
/// closed-vocabulary contract the append-only ledger relies on for
/// meaningful aggregation.
pub const NOT_REQUIRED_REASONS: &[&str] = &[
    "trivial_change",
    "superseded",
    "duplicate",
    "external_adjudication",
    "expired",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchAdjudicationSignature {
    pub signature_id: String,
    pub evidence_ref: Option<String>,
    pub resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDispatchAdjudication {
    pub adjudication_id: String,
    pub outcome_id: String,
    pub event_key: String,
    pub verdict: Option<String>,
    pub not_required_reason: Option<String>,
    pub actor: String,
    pub evidence_ref: String,
    pub signatures: Vec<DispatchAdjudicationSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchAdjudication {
    pub adjudication_id: String,
    pub outcome_id: String,
    pub event_key: String,
    pub verdict: Option<String>,
    pub not_required_reason: Option<String>,
    pub actor: String,
    pub evidence_ref: String,
    pub created_at: String,
    pub insertion_seq: i64,
    pub signatures: Vec<DispatchAdjudicationSignature>,
}

/// Replaying an event key returns the original event only when its canonical
/// payload is unchanged. Corrections require a new key and therefore preserve
/// the preceding adjudication history.
pub fn append_dispatch_adjudication(
    conn: &Connection,
    new: &NewDispatchAdjudication,
) -> Result<DispatchAdjudication, MemoryError> {
    if let Some(reason) = new.not_required_reason.as_deref() {
        if !NOT_REQUIRED_REASONS.contains(&reason) {
            return Err(MemoryError::InvalidArg(format!(
                "not_required_reason '{reason}' is not in the closed set of legal reason codes {:?}; \
                 use one of these or supply a verdict instead",
                NOT_REQUIRED_REASONS
            )));
        }
    }

    // FIX-1 (#1035): the parent INSERT and every signature INSERT MUST be in
    // one transaction. A unique-constraint violation on a signature row
    // previously left a parentless half-event that the event_key short-circuit
    // then permanently entrenched — the next replay returned the orphan. An
    // unchecked_transaction on &Connection auto-rolls-back on drop (any `?`
    // propagation), so a mid-append failure atomically disappears the entire
    // event, parent row included.
    let tx = conn.unchecked_transaction()?;

    // FIX-1 (#1035 round 4): outcome existence check runs BEFORE the
    // event_key short-circuit. Previously the short-circuit came first, so
    // a replay carrying a valid event_key but an UNKNOWN outcome_id
    // returned the old event instead of erroring — the existence check was
    // unreachable on the replay path. Checking existence first closes that
    // bypass. The check also runs INSIDE the transaction so a missing
    // parent rolls back the entire append.
    let parent_exists: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM dispatch_outcomes WHERE outcome_id = ?1",
            params![new.outcome_id],
            |row| row.get(0),
        )
        .optional()?;
    if parent_exists.is_none() {
        return Err(MemoryError::InvalidArg(format!(
            "unknown outcome_id '{}': cannot append an adjudication for an \
             outcome that has no dispatch_outcomes parent row",
            new.outcome_id
        )));
    }

    if let Some(existing) = get_by_event_key(&tx, &new.event_key)? {
        // FIX-1 (#1035 round 4): a collision guard. The event_key hit must
        // belong to the SAME outcome the caller requested — an event_key
        // tied to a different outcome_id is misuse or a collision, not an
        // idempotent replay. The transaction held only SELECTs; dropping it
        // without commit is a harmless rollback.
        if existing.outcome_id != new.outcome_id {
            return Err(MemoryError::InvalidArg(
                "event_key collision or misuse: existing event belongs to a different outcome"
                    .to_string(),
            ));
        }
        // An event key represents a transport retry only when every semantic
        // payload field is unchanged. Returning `existing` for a changed
        // payload would silently report a correction as recorded while
        // preserving the old row instead. That threatens the append-only
        // ledger's truthfulness invariant, so refuse the replay loudly and
        // leave the original row/signatures untouched.
        if !replay_payload_matches(&existing, new) {
            return Err(MemoryError::InvalidArg(
                "event_key replay payload mismatch: the existing adjudication is not canonically equivalent; use a new event_key for a correction".to_string(),
            ));
        }

        // Idempotent replay: return the canonically identical original row
        // without writing.
        return Ok(existing);
    }

    // FIX-3 (#1035 round 4): durable per-outcome insertion sequence. The
    // hidden rowid is not durable under VACUUM INTO (SQLite may recompact
    // and reorder it), so it cannot serve as a ledger ordering. An explicit
    // per-outcome counter assigned inside the transaction is stable across
    // vacuum/backup cycles.
    let insertion_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(insertion_seq), 0) + 1 FROM dispatch_adjudications WHERE outcome_id = ?1",
        params![new.outcome_id],
        |row| row.get(0),
    )?;

    let created_at = normalize_utc_iso_or_now("");
    tx.execute(
        "INSERT INTO dispatch_adjudications
         (adjudication_id, outcome_id, event_key, verdict, not_required_reason, actor, evidence_ref, created_at, insertion_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![new.adjudication_id, new.outcome_id, new.event_key, new.verdict,
            new.not_required_reason, new.actor, new.evidence_ref, created_at, insertion_seq],
    )?;
    for signature in &new.signatures {
        tx.execute(
            "INSERT INTO dispatch_adjudication_signatures
             (adjudication_id, signature_id, evidence_ref, resolved) VALUES (?1, ?2, ?3, ?4)",
            params![
                new.adjudication_id,
                signature.signature_id,
                signature.evidence_ref,
                signature.resolved as i64
            ],
        )?;
    }
    let result = get_by_event_key(&tx, &new.event_key)?.ok_or_else(|| {
        MemoryError::InvalidArg("inserted dispatch adjudication vanished".to_string())
    })?;
    tx.commit()?;
    Ok(result)
}

/// Compare the semantic content of an existing adjudication against a replay
/// request. `adjudication_id`, timestamps, and insertion sequence identify the
/// persisted event and therefore do not participate; all caller-supplied
/// terminal content does. Signature input order is transport noise, so compare
/// its canonical ordering while retaining every triple field.
fn replay_payload_matches(
    existing: &DispatchAdjudication,
    replay: &NewDispatchAdjudication,
) -> bool {
    existing.outcome_id == replay.outcome_id
        && existing.verdict == replay.verdict
        && existing.not_required_reason == replay.not_required_reason
        && existing.actor == replay.actor
        && existing.evidence_ref == replay.evidence_ref
        && canonical_signatures(&existing.signatures) == canonical_signatures(&replay.signatures)
}

fn canonical_signatures(
    signatures: &[DispatchAdjudicationSignature],
) -> Vec<DispatchAdjudicationSignature> {
    let mut canonical = signatures.to_vec();
    canonical.sort_by(|left, right| {
        left.signature_id
            .cmp(&right.signature_id)
            .then_with(|| left.evidence_ref.cmp(&right.evidence_ref))
            .then_with(|| left.resolved.cmp(&right.resolved))
    });
    canonical
}

pub fn list_adjudications_for_outcome(
    conn: &Connection,
    outcome_id: &str,
) -> Result<Vec<DispatchAdjudication>, MemoryError> {
    // FIX-3 (#1035 round 4): event order = durable per-outcome insertion
    // sequence, NOT the hidden rowid. `rowid` is monotonically assigned on
    // this append-only table but is NOT durable under VACUUM INTO — SQLite
    // may recompact and reorder it. An explicit `insertion_seq` counter
    // assigned inside the append transaction is stable across vacuum/backup
    // cycles and is the honest ledger ordering.
    let mut statement = conn.prepare(
        "SELECT adjudication_id, outcome_id, event_key, verdict, not_required_reason, actor, evidence_ref, created_at
         FROM dispatch_adjudications WHERE outcome_id = ?1 ORDER BY insertion_seq",
    )?;
    let ids = statement
        .query_map([outcome_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    ids.into_iter().map(|id| get_by_id(conn, &id)).collect()
}

/// Whether any terminal adjudication event exists for `outcome_id`. Row
/// present = adjudicated; row absent = pending (#1035 frozen spec: the
/// append-only table keyed on `outcome_id` IS the adjudication state).
pub fn outcome_is_adjudicated(conn: &Connection, outcome_id: &str) -> Result<bool, MemoryError> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dispatch_adjudications WHERE outcome_id = ?1)",
        [outcome_id],
        |row| row.get(0),
    )?;
    Ok(exists)
}

fn get_by_event_key(
    conn: &Connection,
    event_key: &str,
) -> Result<Option<DispatchAdjudication>, MemoryError> {
    let id = conn
        .query_row(
            "SELECT adjudication_id FROM dispatch_adjudications WHERE event_key = ?1",
            [event_key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    id.map(|id| get_by_id(conn, &id)).transpose()
}

fn get_by_id(
    conn: &Connection,
    adjudication_id: &str,
) -> Result<DispatchAdjudication, MemoryError> {
    let event = conn.query_row(
        "SELECT adjudication_id, outcome_id, event_key, verdict, not_required_reason, actor, evidence_ref, created_at, insertion_seq
         FROM dispatch_adjudications WHERE adjudication_id = ?1",
        [adjudication_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
    )?;
    let mut statement = conn.prepare(
        "SELECT signature_id, evidence_ref, resolved FROM dispatch_adjudication_signatures WHERE adjudication_id = ?1 ORDER BY signature_id",
    )?;
    let signatures = statement
        .query_map([adjudication_id], |row| {
            Ok(DispatchAdjudicationSignature {
                signature_id: row.get(0)?,
                evidence_ref: row.get(1)?,
                resolved: row.get::<_, i64>(2)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DispatchAdjudication {
        adjudication_id: event.0,
        outcome_id: event.1,
        event_key: event.2,
        verdict: event.3,
        not_required_reason: event.4,
        actor: event.5,
        evidence_ref: event.6,
        created_at: event.7,
        insertion_seq: event.8,
        signatures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_conn() -> Connection {
        libsimple::enable_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        // FIX-3 (#1035): append_dispatch_adjudication now rejects an orphan
        // outcome_id, so the fixture outcome rows the tests adjudicate must
        // have a parent dispatch_outcomes row. Seed both the verdict-path
        // outcome ("outcome-1") and the not_required-path outcome
        // ("outcome-nr") here so every existing test that calls
        // append_dispatch_adjudication through the shared helpers passes.
        seed_outcome(&conn, "outcome-1");
        seed_outcome(&conn, "outcome-nr");
        conn
    }

    /// Insert a minimal parent `dispatch_outcomes` row so an adjudication
    /// linked to `outcome_id` is not an orphan (FIX-3 existence check).
    fn seed_outcome(conn: &Connection, outcome_id: &str) {
        use crate::db::dispatch_outcomes::{upsert_outcome, NewDispatchOutcome};
        let new = NewDispatchOutcome {
            outcome_id: outcome_id.to_string(),
            dispatch_id: format!("dispatch-{outcome_id}"),
            execution_outcome: "completed".to_string(),
            vendor: "codex".to_string(),
            ..Default::default()
        };
        upsert_outcome(conn, &new).unwrap();
    }

    fn event(key: &str) -> NewDispatchAdjudication {
        NewDispatchAdjudication {
            adjudication_id: format!("adjudication-{key}"),
            outcome_id: "outcome-1".to_string(),
            event_key: key.to_string(),
            verdict: Some("accepted".to_string()),
            not_required_reason: None,
            actor: "leader".to_string(),
            evidence_ref: "run-1".to_string(),
            signatures: vec![
                DispatchAdjudicationSignature {
                    signature_id: "fake_security_fix".to_string(),
                    evidence_ref: Some("review-1".to_string()),
                    resolved: false,
                },
                DispatchAdjudicationSignature {
                    signature_id: "zero_discriminating_test".to_string(),
                    evidence_ref: Some("review-2".to_string()),
                    resolved: true,
                },
            ],
        }
    }

    fn not_required_event(key: &str, reason: &str) -> NewDispatchAdjudication {
        NewDispatchAdjudication {
            adjudication_id: format!("adjudication-{key}"),
            outcome_id: "outcome-nr".to_string(),
            event_key: key.to_string(),
            verdict: None,
            not_required_reason: Some(reason.to_string()),
            actor: "leader".to_string(),
            evidence_ref: "run-nr".to_string(),
            signatures: Vec::new(),
        }
    }

    #[test]
    fn replay_is_idempotent_and_correction_preserves_history() {
        let conn = open_conn();
        let first = append_dispatch_adjudication(&conn, &event("event-1")).unwrap();
        let replay = append_dispatch_adjudication(&conn, &event("event-1")).unwrap();
        assert_eq!(
            replay, first,
            "the same event key cannot duplicate a judgment"
        );
        assert_eq!(
            first.signatures.len(),
            2,
            "one event retains both signatures"
        );

        // FIX-4 (#1035): strengthen from row-count to per-signature field
        // assertions — the two signature rows must be independently correct
        // in evidence_ref and resolved, not merely present in the right count.
        // FIX-5 (#1035): the fixture seeds one resolved and one unresolved
        // signature — both `false` would let a regression that permanently
        // clears `resolved` pass undetected.
        let fake_sig = first
            .signatures
            .iter()
            .find(|s| s.signature_id == "fake_security_fix")
            .expect("fake_security_fix signature present");
        assert_eq!(fake_sig.evidence_ref.as_deref(), Some("review-1"));
        assert!(!fake_sig.resolved, "fake_security_fix unresolved");
        let zero_sig = first
            .signatures
            .iter()
            .find(|s| s.signature_id == "zero_discriminating_test")
            .expect("zero_discriminating_test signature present");
        assert_eq!(zero_sig.evidence_ref.as_deref(), Some("review-2"));
        assert!(
            zero_sig.resolved,
            "zero_discriminating_test resolved — a permanently-false roundtrip \
             must not pass the discrimination check"
        );

        let mut correction = event("event-2");
        correction.verdict = Some("rejected".to_string());
        correction.signatures.clear();
        append_dispatch_adjudication(&conn, &correction).unwrap();
        let history = list_adjudications_for_outcome(&conn, "outcome-1").unwrap();
        assert_eq!(
            history.len(),
            2,
            "correction appends instead of overwriting history"
        );
        assert_eq!(history[0].verdict.as_deref(), Some("accepted"));
        assert_eq!(history[1].verdict.as_deref(), Some("rejected"));
    }

    fn assert_same_key_payload_mismatch_is_rejected(
        case: &str,
        original: NewDispatchAdjudication,
        mutate_replay: impl FnOnce(&mut NewDispatchAdjudication),
    ) {
        let conn = open_conn();
        let first = append_dispatch_adjudication(&conn, &original).unwrap();
        let mut replay = original;
        replay.adjudication_id = format!("replay-{case}");
        mutate_replay(&mut replay);

        let err = append_dispatch_adjudication(&conn, &replay)
            .expect_err("a same-key replay with changed payload must fail loudly");
        assert!(
            err.to_string().contains("payload mismatch"),
            "{case}: error must identify the non-equivalent replay: {err}"
        );

        let history = list_adjudications_for_outcome(&conn, &first.outcome_id).unwrap();
        assert_eq!(
            history,
            vec![first],
            "{case}: the rejected replay must preserve the original row and signatures exactly"
        );
    }

    /// A replay may be idempotent only for canonically identical content. The
    /// table covers every scalar payload field plus all fields in the signature
    /// triples, while the helper asserts that the rejected write leaves the
    /// stored event byte-for-byte unchanged.
    #[test]
    fn replay_with_changed_payload_is_rejected_and_preserves_original() {
        type ReplayMutation = fn(&mut NewDispatchAdjudication);
        let cases: [(&str, ReplayMutation); 6] = [
            ("verdict", |replay| {
                replay.verdict = Some("rejected".to_string());
            }),
            ("actor", |replay| {
                replay.actor = "different-leader".to_string();
            }),
            ("evidence_ref", |replay| {
                replay.evidence_ref = "different-evidence".to_string();
            }),
            ("signature_id", |replay| {
                replay.signatures[0].signature_id = "different_signature".to_string();
            }),
            ("signature_evidence_ref", |replay| {
                replay.signatures[0].evidence_ref = Some("different-review".to_string());
            }),
            ("signature_resolved", |replay| {
                replay.signatures[0].resolved = true;
            }),
        ];

        for (case, mutate_replay) in cases {
            assert_same_key_payload_mismatch_is_rejected(
                case,
                event(&format!("event-payload-mismatch-{case}")),
                mutate_replay,
            );
        }

        assert_same_key_payload_mismatch_is_rejected(
            "not_required_reason",
            not_required_event("event-payload-mismatch-reason", "trivial_change"),
            |replay| replay.not_required_reason = Some("superseded".to_string()),
        );
    }

    #[test]
    fn replay_with_reordered_signature_triples_remains_idempotent() {
        let conn = open_conn();
        let original = event("event-signature-order");
        let first = append_dispatch_adjudication(&conn, &original).unwrap();

        let mut reordered = original;
        reordered.adjudication_id = "adjudication-event-signature-order-replay".to_string();
        reordered.signatures.reverse();
        let replay = append_dispatch_adjudication(&conn, &reordered).unwrap();

        assert_eq!(replay, first, "signature order is not semantic content");
        let history = list_adjudications_for_outcome(&conn, "outcome-1").unwrap();
        assert_eq!(history, vec![first], "reordered retry did not append");
    }

    #[test]
    fn db_check_rejects_incomplete_or_ambiguous_terminal_rows() {
        let conn = open_conn();
        for (verdict, reason, actor) in [
            (None, None, "leader"),
            (Some("accepted"), Some("out_of_scope"), "leader"),
            (Some("accepted"), None, ""),
        ] {
            assert!(conn
                .execute(
                    "INSERT INTO dispatch_adjudications
                     (adjudication_id, outcome_id, event_key, verdict, not_required_reason, actor, evidence_ref, created_at, insertion_seq)
                      VALUES (?1, 'outcome', ?2, ?3, ?4, ?5, 'evidence', 'now', 1)",
                    params![uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string(), verdict, reason, actor],
                )
                .is_err());
        }
    }

    #[test]
    fn not_required_reason_must_be_in_closed_set() {
        let conn = open_conn();
        // Illegal reason codes are rejected at the write chokepoint — the
        // row never reaches the DB.
        let illegal = not_required_event("nr-bad", "out_of_scope");
        let err = append_dispatch_adjudication(&conn, &illegal).unwrap_err();
        assert!(
            err.to_string().contains("not_required_reason")
                && err.to_string().contains("out_of_scope"),
            "illegal reason must be rejected with a message naming it: {err}"
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispatch_adjudications WHERE outcome_id = 'outcome-nr'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "no row written for an illegal reason code");

        // All five legal reason codes land successfully.
        for (i, reason) in NOT_REQUIRED_REASONS.iter().enumerate() {
            let ev = not_required_event(&format!("nr-{i}"), reason);
            let row = append_dispatch_adjudication(&conn, &ev).unwrap();
            assert_eq!(row.not_required_reason.as_deref(), Some(*reason));
            assert!(row.verdict.is_none());
        }
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispatch_adjudications WHERE outcome_id = 'outcome-nr'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 5, "all five legal reason codes landed");
    }

    #[test]
    fn outcome_is_adjudicated_reflects_terminal_event_presence() {
        let conn = open_conn();
        // No adjudication row for a fresh outcome → not adjudicated.
        assert!(!outcome_is_adjudicated(&conn, "outcome-fresh").unwrap());

        // Append a terminal verdict event → now adjudicated.
        append_dispatch_adjudication(&conn, &event("event-adjudicated")).unwrap();
        assert!(outcome_is_adjudicated(&conn, "outcome-1").unwrap());

        // A different outcome with no events → still not adjudicated.
        assert!(!outcome_is_adjudicated(&conn, "outcome-other").unwrap());
    }

    /// FIX-1 (#1035): a unique-constraint violation on a signature INSERT
    /// must roll back the ENTIRE event — parent row included. Before the
    /// transaction wrapper the parent INSERT committed independently, leaving
    /// a signatureless half-event that the event_key short-circuit then
    /// permanently entrenched (a replay returned the orphan row forever). This test
    /// is RED on pre-FIX-1 code: the second signature's PK violation fails
    /// AFTER the parent row is already committed, so the parent row count is
    /// 1 instead of 0.
    #[test]
    fn duplicate_signature_rolls_back_entire_event_atomically() {
        let conn = open_conn();
        let mut duplicate = event("event-dup");
        // Two signatures with the SAME signature_id under the same
        // adjudication_id → PK violation on the second INSERT.
        duplicate.signatures = vec![
            DispatchAdjudicationSignature {
                signature_id: "fake_security_fix".to_string(),
                evidence_ref: Some("review-a".to_string()),
                resolved: false,
            },
            DispatchAdjudicationSignature {
                signature_id: "fake_security_fix".to_string(),
                evidence_ref: Some("review-b".to_string()),
                resolved: true,
            },
        ];

        let err = append_dispatch_adjudication(&conn, &duplicate)
            .expect_err("duplicate signature must fail the entire append");
        assert!(
            err.to_string().contains("UNIQUE") || err.to_string().contains("constraint"),
            "error should be the PK violation: {err}"
        );

        // The parent row must NOT exist — the event atomically disappeared.
        let parent_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispatch_adjudications WHERE event_key = 'event-dup'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            parent_count, 0,
            "FIX-1: parent row must roll back with the failed signature INSERT"
        );

        // And no signature rows either.
        let sig_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispatch_adjudication_signatures \
                 WHERE adjudication_id = 'adjudication-event-dup'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sig_count, 0, "no orphan signature rows");

        // The event_key is now free — a well-formed follow-up appends normally.
        let mut clean = event("event-dup");
        clean.signatures.truncate(1);
        let row = append_dispatch_adjudication(&conn, &clean).unwrap();
        assert_eq!(row.signatures.len(), 1);
        assert_eq!(row.signatures[0].signature_id, "fake_security_fix");
    }

    /// FIX-3 (#1035): an adjudication for an `outcome_id` that has no
    /// `dispatch_outcomes` parent row is rejected — the entire append is
    /// refused and zero rows land. The adjudication table carries no FK, so
    /// without this check a caller could mint a judgment against a
    /// hallucinated outcome_id, producing an orphan that aggregation queries
    /// silently inflate. RED on pre-FIX-3 code: the row is written (no
    /// existence check existed).
    #[test]
    fn unknown_outcome_id_is_rejected_and_writes_zero_rows() {
        let conn = open_conn();
        let mut orphan = event("event-orphan");
        orphan.outcome_id = "outcome-that-does-not-exist".to_string();

        let err = append_dispatch_adjudication(&conn, &orphan)
            .expect_err("an unknown outcome_id must be rejected");
        assert!(
            err.to_string().contains("unknown outcome_id")
                && err.to_string().contains("outcome-that-does-not-exist"),
            "error must name the unknown outcome_id: {err}"
        );

        let parent_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispatch_adjudications \
                 WHERE outcome_id = 'outcome-that-does-not-exist'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            parent_count, 0,
            "FIX-3: zero rows written for an unknown outcome_id"
        );

        let sig_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispatch_adjudication_signatures \
                 WHERE adjudication_id = 'adjudication-event-orphan'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sig_count, 0, "no orphan signature rows either");
    }

    /// FIX-1 (#1035 round 4): a replay carrying a VALID event_key but an
    /// UNKNOWN outcome_id must be rejected — NOT silently return the old
    /// event. Pre-fix, `get_by_event_key` short-circuited BEFORE the
    /// outcome existence check, so the replay path never validated
    /// outcome_id at all. RED on pre-round-4 code: the second append
    /// returns `Ok(old_event)` instead of `Err(InvalidArg)`.
    #[test]
    fn replay_with_unknown_outcome_id_is_rejected_not_short_circuited() {
        let conn = open_conn();
        // First: append a legitimate event for outcome-1.
        let first = append_dispatch_adjudication(&conn, &event("event-replay-bypass")).unwrap();

        // Second: SAME event_key but an outcome_id that does not exist.
        let mut replay = event("event-replay-bypass");
        replay.outcome_id = "outcome-that-does-not-exist".to_string();

        let err = append_dispatch_adjudication(&conn, &replay)
            .expect_err("a replay with an unknown outcome_id must not short-circuit");
        assert!(
            err.to_string().contains("unknown outcome_id"),
            "error must name the unknown outcome_id, not return the old event: {err}"
        );

        // The original event is untouched.
        let rows = list_adjudications_for_outcome(&conn, "outcome-1").unwrap();
        assert_eq!(rows.len(), 1, "original event still the only row");
        assert_eq!(rows[0].adjudication_id, first.adjudication_id);
    }

    /// FIX-1 (#1035 round 4): a replay carrying a VALID event_key and a
    /// DIFFERENT but real outcome_id must be rejected as a collision — the
    /// event_key is bound to a specific outcome. Pre-fix, the short-circuit
    /// returned the old event with no outcome_id comparison. RED on
    /// pre-round-4 code: the second append returns `Ok(old_event)` instead
    /// of `Err(InvalidArg)`.
    #[test]
    fn replay_with_different_real_outcome_id_is_collision() {
        let conn = open_conn();
        // First: append for outcome-1.
        append_dispatch_adjudication(&conn, &event("event-collision")).unwrap();

        // Second: SAME event_key but outcome-nr (which DOES exist).
        let mut replay = event("event-collision");
        replay.outcome_id = "outcome-nr".to_string();

        let err = append_dispatch_adjudication(&conn, &replay)
            .expect_err("an event_key bound to a different outcome must be rejected");
        assert!(
            err.to_string().contains("collision"),
            "error must name the collision, not return the old event: {err}"
        );

        // No row landed for outcome-nr.
        let rows_nr = list_adjudications_for_outcome(&conn, "outcome-nr").unwrap();
        assert!(rows_nr.is_empty(), "no adjudication for outcome-nr");
        // outcome-1 still has exactly its one event.
        let rows_1 = list_adjudications_for_outcome(&conn, "outcome-1").unwrap();
        assert_eq!(rows_1.len(), 1);
    }

    /// FIX-3 (#1035 round 4): insertion_seq is a durable per-outcome
    /// counter. Three events for the same outcome get insertion_seq 1, 2,
    /// 3, and `list_adjudications_for_outcome` returns them in that order.
    /// RED on pre-round-4 code: `insertion_seq` column does not exist.
    #[test]
    fn insertion_seq_orders_events_per_outcome() {
        let conn = open_conn();

        let mut e1 = event("seq-1");
        e1.verdict = Some("accepted".to_string());
        let row1 = append_dispatch_adjudication(&conn, &e1).unwrap();
        assert_eq!(row1.insertion_seq, 1, "first event gets insertion_seq 1");

        let mut e2 = event("seq-2");
        e2.verdict = Some("rejected".to_string());
        let row2 = append_dispatch_adjudication(&conn, &e2).unwrap();
        assert_eq!(row2.insertion_seq, 2, "second event gets insertion_seq 2");

        let mut e3 = event("seq-3");
        e3.verdict = Some("accepted".to_string());
        let row3 = append_dispatch_adjudication(&conn, &e3).unwrap();
        assert_eq!(row3.insertion_seq, 3, "third event gets insertion_seq 3");

        // List returns them in insertion_seq order.
        let history = list_adjudications_for_outcome(&conn, "outcome-1").unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].insertion_seq, 1);
        assert_eq!(history[1].insertion_seq, 2);
        assert_eq!(history[2].insertion_seq, 3);
        assert_eq!(history[0].verdict.as_deref(), Some("accepted"));
        assert_eq!(history[1].verdict.as_deref(), Some("rejected"));
        assert_eq!(history[2].verdict.as_deref(), Some("accepted"));
    }

    /// The per-outcome sequence is schema-enforced, not merely
    /// convention-enforced: two rows sharing (outcome_id, insertion_seq)
    /// violate the UNIQUE constraint, so a concurrent append that read the
    /// same MAX fails loudly instead of silently breaking total ordering.
    #[test]
    fn duplicate_insertion_seq_violates_schema_constraint() {
        let conn = open_conn();
        seed_outcome(&conn, "outcome-1");
        let insert_raw = |id: &str, seq: i64| {
            conn.execute(
                "INSERT INTO dispatch_adjudications
                 (adjudication_id, outcome_id, event_key, verdict, not_required_reason,
                  actor, evidence_ref, created_at, insertion_seq)
                 VALUES (?1, 'outcome-1', ?1, 'confirmed', NULL, 'leader', 'e', '', ?2)",
                params![id, seq],
            )
        };
        insert_raw("adj-1", 1).expect("first row at seq 1");
        let err = insert_raw("adj-2", 1).expect_err("duplicate (outcome_id, insertion_seq)");
        assert!(
            err.to_string().to_lowercase().contains("unique"),
            "duplicate sequence must fail the UNIQUE constraint, got: {err}"
        );
        insert_raw("adj-3", 2).expect("next sequence value is accepted");
    }
}

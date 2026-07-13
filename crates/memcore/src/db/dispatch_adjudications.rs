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
    pub signatures: Vec<DispatchAdjudicationSignature>,
}

/// Replaying an event key returns the original event. Corrections require a new
/// key and therefore preserve the preceding adjudication history.
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
    if let Some(existing) = get_by_event_key(conn, &new.event_key)? {
        return Ok(existing);
    }
    let created_at = normalize_utc_iso_or_now("");
    conn.execute(
        "INSERT INTO dispatch_adjudications
         (adjudication_id, outcome_id, event_key, verdict, not_required_reason, actor, evidence_ref, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![new.adjudication_id, new.outcome_id, new.event_key, new.verdict,
            new.not_required_reason, new.actor, new.evidence_ref, created_at],
    )?;
    for signature in &new.signatures {
        conn.execute(
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
    get_by_event_key(conn, &new.event_key)?.ok_or_else(|| {
        MemoryError::InvalidArg("inserted dispatch adjudication vanished".to_string())
    })
}

pub fn list_adjudications_for_outcome(
    conn: &Connection,
    outcome_id: &str,
) -> Result<Vec<DispatchAdjudication>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT adjudication_id, outcome_id, event_key, verdict, not_required_reason, actor, evidence_ref, created_at
         FROM dispatch_adjudications WHERE outcome_id = ?1 ORDER BY created_at, adjudication_id",
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
        "SELECT adjudication_id, outcome_id, event_key, verdict, not_required_reason, actor, evidence_ref, created_at
         FROM dispatch_adjudications WHERE adjudication_id = ?1",
        [adjudication_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
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
        conn
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
                    resolved: false,
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
                     (adjudication_id, outcome_id, event_key, verdict, not_required_reason, actor, evidence_ref, created_at)
                      VALUES (?1, 'outcome', ?2, ?3, ?4, ?5, 'evidence', 'now')",
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
            err.to_string().contains("not_required_reason") && err.to_string().contains("out_of_scope"),
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
}

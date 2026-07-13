//! Cross-session presence claims (#1001) — advisory "who's working on what"
//! lease rows in `session_claims`.
//!
//! A claim is NOT a lock: it never blocks or preempts another session, it
//! only lets a briefing surface "who else is touching this issue/lane right
//! now" so a fleet of sessions/agents (Claude leader, codex session,
//! dispatched lane) can see each other instead of colliding blind (#1001's
//! motivating incident: two sessions nearly re-dispatching work the other had
//! already shipped).
//!
//! ## Lease semantics (reuses the #894 `exec_envs` shape)
//!
//! ```text
//!   active ──release──▶ released
//!     ▲                     │
//!     └── (no transition) ◀─┘   releasing an already-released claim is an
//!                               idempotent no-op, never an error.
//! ```
//!
//! There is exactly one function that flips a claim to `released`
//! ([`release_claim`]) — manual `release`, `complete`, and `cancel` all route
//! through it, same discipline as `reclaim_exec_env`.
//!
//! Unlike `exec_envs`, there is no background reaper in this slice: staleness
//! is lazy — [`list_active_claims`] and [`is_claim_stale`] let a caller (the
//! briefing splice) treat a claim whose `heartbeat_at` is older than a TTL as
//! effectively expired without a second write. A stale claim's row is left in
//! place (for audit) until something actually releases it or a fresh claim
//! recycles the same `(session_client, issue_ref, flow_id)` identity triple
//! (the `ON CONFLICT` target `upsert_or_heartbeat_claim` upserts on — see
//! that function's doc comment).

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;

use super::common::normalize_utc_iso_or_now;

/// Claim lifecycle state. Two states only — see the module doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// Registered and (per lazy TTL expiry) presumed live.
    Active,
    /// Released — either explicitly (`release`/`complete`/`cancel`) or
    /// superseded; the row is retained for audit and idempotent release.
    Released,
}

impl ClaimState {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimState::Active => "active",
            ClaimState::Released => "released",
        }
    }

    /// Parse a persisted state string. An unknown/legacy value is an error the
    /// caller must surface (fail-closed), matching `ExecEnvState::parse`.
    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "active" => Ok(ClaimState::Active),
            "released" => Ok(ClaimState::Released),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown session_claim state '{other}' (expected 'active' or 'released')"
            ))),
        }
    }
}

/// A row in `session_claims`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionClaim {
    pub claim_id: String,
    pub session_client: Option<String>,
    pub issue_ref: Option<String>,
    pub flow_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub branch: String,
    pub declared_file_scope: Option<String>,
    pub state: ClaimState,
    pub release_reason: Option<String>,
    pub created_at: String,
    pub heartbeat_at: String,
    pub released_at: Option<String>,
}

/// Fields required to insert a new claim. `claim_id` must be caller-supplied
/// and unique (generated the same nanos-XOR-pid way as `exec_envs.env_id`).
/// `created_at`/`heartbeat_at` default to now when empty.
#[derive(Debug, Clone, Default)]
pub struct NewSessionClaim {
    pub claim_id: String,
    pub session_client: Option<String>,
    pub issue_ref: Option<String>,
    pub flow_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub branch: String,
    pub declared_file_scope: Option<String>,
    pub created_at: String,
}

/// How a release selects its target claim.
#[derive(Debug, Clone)]
pub enum ClaimSelector {
    /// By primary key.
    ClaimId(String),
    /// By dispatch id (the durable identity a dispatched lane already
    /// carries) — matches the *active* claim for that dispatch. Used by
    /// `complete`/`cancel` call sites that only know the dispatch id.
    DispatchId(String),
}

/// Result of a release attempt through the single release path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// A claim was flipped `active` -> `released` on this call.
    Released { claim_id: String },
    /// The claim was already `released`; no state changed (idempotent).
    AlreadyReleased { claim_id: String },
    /// No claim matched the selector.
    NotFound,
}

const SELECT_COLUMNS: &str = "claim_id, session_client, issue_ref, flow_id, dispatch_id, branch, \
     declared_file_scope, state, release_reason, created_at, heartbeat_at, released_at";

fn row_to_claim(row: &rusqlite::Row<'_>) -> Result<SessionClaim, rusqlite::Error> {
    let state_raw: String = row.get(7)?;
    let state = ClaimState::parse(&state_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            )),
        )
    })?;
    Ok(SessionClaim {
        claim_id: row.get(0)?,
        session_client: row.get(1)?,
        issue_ref: row.get(2)?,
        flow_id: row.get(3)?,
        dispatch_id: row.get(4)?,
        branch: row.get(5)?,
        declared_file_scope: row.get(6)?,
        state,
        release_reason: row.get(8)?,
        created_at: row.get(9)?,
        heartbeat_at: row.get(10)?,
        released_at: row.get(11)?,
    })
}

/// Insert a new claim. Fails if `claim_id` already exists — a collision is a
/// bug, never a silent overwrite (same discipline as `insert_exec_env`).
/// `heartbeat_at` is stamped equal to `created_at` on insert.
pub fn insert_claim(conn: &Connection, claim: &NewSessionClaim) -> Result<(), MemoryError> {
    let created_at = if claim.created_at.trim().is_empty() {
        normalize_utc_iso_or_now("")
    } else {
        normalize_utc_iso_or_now(&claim.created_at)
    };
    conn.execute(
        "INSERT INTO session_claims
         (claim_id, session_client, issue_ref, flow_id, dispatch_id, branch,
          declared_file_scope, state, release_reason, created_at, heartbeat_at, released_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', NULL, ?8, ?8, NULL)",
        params![
            claim.claim_id,
            claim.session_client,
            claim.issue_ref,
            claim.flow_id,
            claim.dispatch_id,
            claim.branch,
            claim.declared_file_scope,
            created_at,
        ],
    )?;
    Ok(())
}

/// Fetch a claim by id.
pub fn get_claim(conn: &Connection, claim_id: &str) -> Result<Option<SessionClaim>, MemoryError> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM session_claims WHERE claim_id = ?1");
    let claim = conn
        .query_row(&sql, params![claim_id], row_to_claim)
        .optional()?;
    Ok(claim)
}

/// Fetch the newest claim recorded for a dispatch, including released claims.
/// Terminal delivery resolves its recipient from this dispatch-owned fact, not
/// from a broad tool/profile guess.
pub fn get_claim_for_dispatch(
    conn: &Connection,
    dispatch_id: &str,
) -> Result<Option<SessionClaim>, MemoryError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM session_claims WHERE dispatch_id = ?1 ORDER BY created_at DESC LIMIT 1"
    );
    Ok(conn
        .query_row(&sql, params![dispatch_id], row_to_claim)
        .optional()?)
}

/// List claims, optionally filtered by state. Newest first. Used by the
/// briefing 工位表 section and manual `claim`/`release` bookkeeping.
pub fn list_claims(
    conn: &Connection,
    state: Option<ClaimState>,
) -> Result<Vec<SessionClaim>, MemoryError> {
    let mut out = Vec::new();
    match state {
        Some(state) => {
            let sql = format!(
                "SELECT {SELECT_COLUMNS} FROM session_claims WHERE state = ?1 ORDER BY heartbeat_at DESC"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![state.as_str()], row_to_claim)?;
            for row in rows {
                out.push(row?);
            }
        }
        None => {
            let sql =
                format!("SELECT {SELECT_COLUMNS} FROM session_claims ORDER BY heartbeat_at DESC");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], row_to_claim)?;
            for row in rows {
                out.push(row?);
            }
        }
    }
    Ok(out)
}

/// List claims whose `state = 'active'` AND whose `heartbeat_at` is within
/// `ttl_seconds` of `now_iso` — the lazy-expiry read the briefing splice and
/// collision-warning checks use. `now_iso`/`ttl_seconds` are caller-supplied
/// so tests can inject a deterministic clock instead of sleeping.
pub fn list_active_claims(
    conn: &Connection,
    now_iso: &str,
    ttl_seconds: i64,
) -> Result<Vec<SessionClaim>, MemoryError> {
    let all_active = list_claims(conn, Some(ClaimState::Active))?;
    let now = chrono::DateTime::parse_from_rfc3339(now_iso)
        .map_err(|e| MemoryError::InvalidArg(format!("invalid now_iso '{now_iso}': {e}")))?;
    Ok(all_active
        .into_iter()
        .filter(|claim| !is_claim_stale(claim, now.to_utc(), ttl_seconds))
        .collect())
}

/// Whether `claim`'s heartbeat is older than `ttl_seconds` relative to `now`.
/// A claim with an unparsable `heartbeat_at` is treated as stale (fail-closed
/// — an unreadable heartbeat must never be presented as "fresh").
pub fn is_claim_stale(
    claim: &SessionClaim,
    now: chrono::DateTime<chrono::Utc>,
    ttl_seconds: i64,
) -> bool {
    match chrono::DateTime::parse_from_rfc3339(&claim.heartbeat_at) {
        Ok(hb) => (now - hb.to_utc()).num_seconds() > ttl_seconds,
        Err(_) => true,
    }
}

/// Insert-or-heartbeat: the atomic entrypoint zero-ceremony hooks use.
///
/// If an *active* claim already exists for this `(session_client, issue_ref,
/// flow_id)` triple (a session re-entering briefing/intake/dispatch for work
/// it already claimed), its heartbeat is bumped in place rather than inserting
/// a duplicate row. Otherwise a fresh claim is inserted. Mirrors the
/// exec_envs "one active row per identity" idiom
/// (`find_active_exec_env_by_path`) but keyed on session+issue/lane instead of
/// worktree path, since #1001 grains at session×issue/lane, not per-call.
///
/// ## Atomicity (#1001 round 3, item 2)
///
/// This is a single `INSERT ... ON CONFLICT (...) WHERE state = 'active' DO
/// UPDATE ...` statement, not a read-then-write. The conflict target is the
/// exact expression list and partial-index predicate of
/// `idx_session_claims_identity_active` (`ddl.rs` /
/// `migrations/session_claims_identity.rs`):
/// `(COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id,
/// ''))  WHERE state = 'active'` — SQLite requires the `ON CONFLICT` target to
/// match an existing unique index verbatim (same expressions, same partial
/// predicate) to resolve against it, and a mismatched target here would make
/// this silently fall back to raising `UNIQUE constraint failed` instead of
/// upserting. A prior read-then-write (`SELECT` to check existence, then a
/// separate `UPDATE`/`INSERT`) is a TOCTOU race even inside a transaction:
/// SQLite's default deferred transaction does not take a write lock until its
/// first write, so two concurrent callers could both `SELECT` "no existing
/// row" before either commits, and one of the two `INSERT`s would then fail
/// the unique index with no path to convert that failure into a heartbeat —
/// the very race #1001 round 2's index was added to catch, but the
/// application code never closed. `INSERT ... ON CONFLICT ... DO UPDATE` is
/// a single statement the database resolves under one lock: concurrent
/// same-identity callers each either insert (if they win the race) or update
/// the winner's row (if they lose it) — both succeed, neither errors, and
/// exactly one row exists for the identity afterward.
///
/// Returns the `claim_id` that is now active (either the pre-existing one,
/// heartbeated, or the newly inserted one) via `RETURNING claim_id`, so the
/// caller learns the winning row's id without a second read.
pub fn upsert_or_heartbeat_claim(
    conn: &mut Connection,
    new_claim: &NewSessionClaim,
) -> Result<String, MemoryError> {
    let created_at = if new_claim.created_at.trim().is_empty() {
        normalize_utc_iso_or_now("")
    } else {
        normalize_utc_iso_or_now(&new_claim.created_at)
    };
    let tx = conn.transaction()?;
    let claim_id: String = tx.query_row(
        "INSERT INTO session_claims
         (claim_id, session_client, issue_ref, flow_id, dispatch_id, branch,
          declared_file_scope, state, release_reason, created_at, heartbeat_at, released_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', NULL, ?8, ?8, NULL)
          ON CONFLICT (COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id, ''), \
              COALESCE(CASE WHEN issue_ref IS NULL AND flow_id IS NULL THEN dispatch_id ELSE '' END, '')) \
             WHERE state = 'active'
         DO UPDATE SET \
             heartbeat_at = ?8, \
             dispatch_id = COALESCE(session_claims.dispatch_id, excluded.dispatch_id), \
             branch = CASE WHEN excluded.branch = '' THEN session_claims.branch ELSE excluded.branch END, \
             declared_file_scope = COALESCE(excluded.declared_file_scope, session_claims.declared_file_scope)
         RETURNING claim_id",
        params![
            new_claim.claim_id,
            new_claim.session_client,
            new_claim.issue_ref,
            new_claim.flow_id,
            new_claim.dispatch_id,
            new_claim.branch,
            new_claim.declared_file_scope,
            created_at,
        ],
        |row| row.get(0),
    )?;
    tx.commit()?;
    Ok(claim_id)
}

/// Update `heartbeat_at` to now for the active claim matching `selector`.
/// Best-effort by design (callers treat failures as non-fatal, mirroring the
/// exec_env lease-insert-failure pattern) but the function itself surfaces
/// errors so the caller can decide how to log them. A missing or already-
/// released claim is not an error — heartbeats are advisory.
pub fn heartbeat_claim(conn: &Connection, selector: &ClaimSelector) -> Result<(), MemoryError> {
    let now = normalize_utc_iso_or_now("");
    match selector {
        ClaimSelector::ClaimId(claim_id) => {
            conn.execute(
                "UPDATE session_claims SET heartbeat_at = ?2 WHERE claim_id = ?1 AND state = 'active'",
                params![claim_id, now],
            )?;
        }
        ClaimSelector::DispatchId(dispatch_id) => {
            conn.execute(
                "UPDATE session_claims SET heartbeat_at = ?2 \
                 WHERE dispatch_id = ?1 AND state = 'active'",
                params![dispatch_id, now],
            )?;
        }
    }
    Ok(())
}

/// THE single release path (#1001, mirrors `reclaim_exec_env`): transactionally
/// flip an `active` claim to `released`, stamping `released_at` and an
/// optional reason. Idempotent — a claim that is already `released` returns
/// [`ReleaseOutcome::AlreadyReleased`] without a second write, and a missing
/// claim returns [`ReleaseOutcome::NotFound`]. Manual `release`, `complete`,
/// and `cancel` must all call through here.
pub fn release_claim(
    conn: &mut Connection,
    selector: &ClaimSelector,
    reason: Option<&str>,
) -> Result<ReleaseOutcome, MemoryError> {
    let tx = conn.transaction()?;
    let existing: Option<(String, String)> = match selector {
        ClaimSelector::ClaimId(claim_id) => tx
            .query_row(
                "SELECT claim_id, state FROM session_claims WHERE claim_id = ?1",
                params![claim_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?,
        ClaimSelector::DispatchId(dispatch_id) => tx
            .query_row(
                "SELECT claim_id, state FROM session_claims WHERE dispatch_id = ?1 \
                 ORDER BY CASE state WHEN 'active' THEN 0 ELSE 1 END, heartbeat_at DESC LIMIT 1",
                params![dispatch_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?,
    };

    let outcome = match existing {
        None => ReleaseOutcome::NotFound,
        Some((claim_id, state_raw)) => match ClaimState::parse(&state_raw)? {
            ClaimState::Released => ReleaseOutcome::AlreadyReleased { claim_id },
            ClaimState::Active => {
                let now = normalize_utc_iso_or_now("");
                tx.execute(
                    "UPDATE session_claims SET state = 'released', released_at = ?2, \
                     release_reason = ?3 WHERE claim_id = ?1 AND state = 'active'",
                    params![claim_id, now, reason],
                )?;
                ReleaseOutcome::Released { claim_id }
            }
        },
    };
    tx.commit()?;
    Ok(outcome)
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

    /// A file-backed (not `:memory:`) connection, required for a real
    /// multi-connection concurrency test — two `:memory:` connections are
    /// two independent databases, so they cannot race each other at all.
    fn open_file_conn(path: &std::path::Path) -> Connection {
        libsimple::enable_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open(path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn new_claim(claim_id: &str, issue_ref: &str) -> NewSessionClaim {
        NewSessionClaim {
            claim_id: claim_id.to_string(),
            session_client: Some("claude-code".to_string()),
            issue_ref: Some(issue_ref.to_string()),
            flow_id: Some("flow-1".to_string()),
            dispatch_id: Some("dispatch-1".to_string()),
            branch: "feat/x".to_string(),
            declared_file_scope: Some(r#"["crates/foo/src/lib.rs"]"#.to_string()),
            created_at: String::new(),
        }
    }

    #[test]
    fn insert_and_get_roundtrip_defaults_to_active() {
        let conn = open_conn();
        insert_claim(&conn, &new_claim("claim-1", "org/repo#1001")).unwrap();
        let got = get_claim(&conn, "claim-1").unwrap().expect("claim present");
        assert_eq!(got.claim_id, "claim-1");
        assert_eq!(got.state, ClaimState::Active);
        assert_eq!(got.issue_ref.as_deref(), Some("org/repo#1001"));
        assert_eq!(got.session_client.as_deref(), Some("claude-code"));
        assert!(got.released_at.is_none());
        assert!(!got.created_at.is_empty());
        assert_eq!(
            got.heartbeat_at, got.created_at,
            "heartbeat starts == created_at"
        );
    }

    #[test]
    fn duplicate_claim_id_is_rejected_not_silently_overwritten() {
        let conn = open_conn();
        insert_claim(&conn, &new_claim("claim-dup", "org/repo#1")).unwrap();
        let err = insert_claim(&conn, &new_claim("claim-dup", "org/repo#2"));
        assert!(err.is_err(), "duplicate claim_id must be rejected");
        let got = get_claim(&conn, "claim-dup").unwrap().unwrap();
        assert_eq!(got.issue_ref.as_deref(), Some("org/repo#1"));
    }

    #[test]
    fn release_flips_active_to_released_and_stamps() {
        let mut conn = open_conn();
        insert_claim(&conn, &new_claim("claim-2", "org/repo#2")).unwrap();
        let outcome = release_claim(
            &mut conn,
            &ClaimSelector::ClaimId("claim-2".to_string()),
            Some("manual release"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome::Released {
                claim_id: "claim-2".to_string()
            }
        );
        let got = get_claim(&conn, "claim-2").unwrap().unwrap();
        assert_eq!(got.state, ClaimState::Released);
        assert_eq!(got.release_reason.as_deref(), Some("manual release"));
        assert!(got.released_at.is_some());
    }

    #[test]
    fn release_is_idempotent_no_second_write() {
        let mut conn = open_conn();
        insert_claim(&conn, &new_claim("claim-3", "org/repo#3")).unwrap();
        let first = release_claim(
            &mut conn,
            &ClaimSelector::ClaimId("claim-3".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(
            first,
            ReleaseOutcome::Released {
                claim_id: "claim-3".to_string()
            }
        );
        let stamp_after_first = get_claim(&conn, "claim-3").unwrap().unwrap().released_at;

        let second = release_claim(
            &mut conn,
            &ClaimSelector::ClaimId("claim-3".to_string()),
            Some("second-reason"),
        )
        .unwrap();
        assert_eq!(
            second,
            ReleaseOutcome::AlreadyReleased {
                claim_id: "claim-3".to_string()
            }
        );
        let after = get_claim(&conn, "claim-3").unwrap().unwrap();
        assert_eq!(
            after.released_at, stamp_after_first,
            "stamp unchanged on no-op"
        );
        assert!(
            after.release_reason.as_deref() != Some("second-reason"),
            "reason must not be overwritten by an idempotent release"
        );
    }

    #[test]
    fn release_missing_claim_reports_not_found() {
        let mut conn = open_conn();
        let outcome =
            release_claim(&mut conn, &ClaimSelector::ClaimId("nope".to_string()), None).unwrap();
        assert_eq!(outcome, ReleaseOutcome::NotFound);
    }

    #[test]
    fn release_by_dispatch_id_targets_the_active_claim() {
        let mut conn = open_conn();
        // A stale released row plus a live active row for the same dispatch.
        let mut old = new_claim("claim-old", "org/repo#4");
        old.dispatch_id = Some("dispatch-shared".to_string());
        insert_claim(&conn, &old).unwrap();
        release_claim(
            &mut conn,
            &ClaimSelector::ClaimId("claim-old".to_string()),
            None,
        )
        .unwrap();
        let mut fresh = new_claim("claim-new", "org/repo#4");
        fresh.dispatch_id = Some("dispatch-shared".to_string());
        insert_claim(&conn, &fresh).unwrap();

        let outcome = release_claim(
            &mut conn,
            &ClaimSelector::DispatchId("dispatch-shared".to_string()),
            Some("complete"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome::Released {
                claim_id: "claim-new".to_string()
            }
        );
    }

    #[test]
    fn list_filters_by_state() {
        let mut conn = open_conn();
        insert_claim(&conn, &new_claim("claim-a", "org/repo#a")).unwrap();
        insert_claim(&conn, &new_claim("claim-b", "org/repo#b")).unwrap();
        release_claim(
            &mut conn,
            &ClaimSelector::ClaimId("claim-b".to_string()),
            None,
        )
        .unwrap();

        let active = list_claims(&conn, Some(ClaimState::Active)).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].claim_id, "claim-a");

        let released = list_claims(&conn, Some(ClaimState::Released)).unwrap();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].claim_id, "claim-b");

        let all = list_claims(&conn, None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn state_parse_rejects_unknown() {
        assert!(ClaimState::parse("provisioned").is_err());
        assert_eq!(ClaimState::parse("active").unwrap(), ClaimState::Active);
        assert_eq!(ClaimState::parse("released").unwrap(), ClaimState::Released);
    }

    // --- TTL expiry / heartbeat discrimination (#1001 acceptance) ---------

    fn claim_with_heartbeat(heartbeat_at: &str) -> SessionClaim {
        SessionClaim {
            claim_id: "c".to_string(),
            session_client: None,
            issue_ref: None,
            flow_id: None,
            dispatch_id: None,
            branch: String::new(),
            declared_file_scope: None,
            state: ClaimState::Active,
            release_reason: None,
            created_at: heartbeat_at.to_string(),
            heartbeat_at: heartbeat_at.to_string(),
            released_at: None,
        }
    }

    #[test]
    fn fresh_heartbeat_survives_ttl_boundary() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
            .unwrap()
            .to_utc();
        // Heartbeat 5 minutes ago, TTL 30 minutes: must NOT be stale.
        let claim = claim_with_heartbeat("2026-07-11T11:55:00Z");
        assert!(!is_claim_stale(&claim, now, 1800));
    }

    #[test]
    fn stale_heartbeat_past_ttl_is_reaped() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
            .unwrap()
            .to_utc();
        // Heartbeat 31 minutes ago, TTL 30 minutes: must BE stale.
        let claim = claim_with_heartbeat("2026-07-11T11:29:00Z");
        assert!(is_claim_stale(&claim, now, 1800));
    }

    #[test]
    fn ttl_boundary_is_exact_not_off_by_one() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
            .unwrap()
            .to_utc();
        // Heartbeat exactly TTL seconds ago: boundary is "> ttl", so exactly
        // at the TTL must still be fresh (not stale).
        let claim = claim_with_heartbeat("2026-07-11T11:30:00Z");
        assert!(!is_claim_stale(&claim, now, 1800));
        // One second further back must be stale.
        let claim2 = claim_with_heartbeat("2026-07-11T11:29:59Z");
        assert!(is_claim_stale(&claim2, now, 1800));
    }

    #[test]
    fn unparsable_heartbeat_is_treated_as_stale_fail_closed() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
            .unwrap()
            .to_utc();
        let claim = claim_with_heartbeat("not-a-timestamp");
        assert!(is_claim_stale(&claim, now, 1_000_000_000));
    }

    #[test]
    fn list_active_claims_excludes_stale_and_released() {
        let conn = open_conn();
        insert_claim(&conn, &new_claim("fresh", "org/repo#f")).unwrap();

        // Manually backdate a second claim's heartbeat past the TTL horizon
        // (simulating a dead session that stopped heartbeating), rather than
        // sleeping in the test.
        insert_claim(&conn, &new_claim("stale", "org/repo#s")).unwrap();
        conn.execute(
            "UPDATE session_claims SET heartbeat_at = '2020-01-01T00:00:00Z' WHERE claim_id = 'stale'",
            [],
        )
        .unwrap();

        let now_iso = normalize_utc_iso_or_now("");
        let active = list_active_claims(&conn, &now_iso, 1800).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].claim_id, "fresh");
    }

    #[test]
    fn two_concurrent_sessions_each_see_the_others_claim() {
        // Discrimination for the briefing acceptance criterion: two sessions
        // claim different issues; both rows are visible via list_active_claims
        // (the read path the briefing splice uses) regardless of who inserted
        // which row.
        let conn = open_conn();
        let mut session_a = new_claim("sess-a-claim", "org/repo#100");
        session_a.session_client = Some("claude-code".to_string());
        insert_claim(&conn, &session_a).unwrap();

        let mut session_b = new_claim("sess-b-claim", "org/repo#200");
        session_b.session_client = Some("codex".to_string());
        insert_claim(&conn, &session_b).unwrap();

        let now_iso = normalize_utc_iso_or_now("");
        let active = list_active_claims(&conn, &now_iso, 1800).unwrap();
        assert_eq!(active.len(), 2);
        assert!(active
            .iter()
            .any(|c| c.claim_id == "sess-a-claim"
                && c.session_client.as_deref() == Some("claude-code")));
        assert!(active
            .iter()
            .any(|c| c.claim_id == "sess-b-claim" && c.session_client.as_deref() == Some("codex")));
    }

    // --- upsert_or_heartbeat_claim (#1001 zero-ceremony hook path) --------

    #[test]
    fn upsert_inserts_fresh_claim_when_none_exists() {
        let mut conn = open_conn();
        let claim = new_claim("hook-claim-1", "org/repo#500");
        let claim_id = upsert_or_heartbeat_claim(&mut conn, &claim).unwrap();
        assert_eq!(claim_id, "hook-claim-1");
        let got = get_claim(&conn, "hook-claim-1").unwrap().unwrap();
        assert_eq!(got.state, ClaimState::Active);
    }

    #[test]
    fn upsert_heartbeats_existing_active_claim_for_same_identity_no_duplicate_row() {
        let mut conn = open_conn();
        let first = new_claim("hook-claim-2", "org/repo#501");
        let claim_id_1 = upsert_or_heartbeat_claim(&mut conn, &first).unwrap();

        // Re-entering briefing/intake/dispatch for the SAME session+issue+flow
        // must heartbeat the existing row, not insert a second one — even
        // though `claim_id` on the second call differs (a caller re-generates
        // a fresh candidate id every call; the upsert key is
        // session_client+issue_ref+flow_id, not claim_id).
        let mut second = new_claim("hook-claim-2-b", "org/repo#501");
        second.branch = "feat/x".to_string();
        let claim_id_2 = upsert_or_heartbeat_claim(&mut conn, &second).unwrap();

        assert_eq!(
            claim_id_1, claim_id_2,
            "second call must resolve to the same existing claim, not insert a new row"
        );
        let all = list_claims(&conn, Some(ClaimState::Active)).unwrap();
        assert_eq!(
            all.len(),
            1,
            "must not duplicate the row for the same session+issue+flow"
        );
    }

    #[test]
    fn upsert_creates_separate_claims_for_different_issues() {
        let mut conn = open_conn();
        let mut a = new_claim("hook-claim-3a", "org/repo#600");
        a.flow_id = Some("flow-a".to_string());
        let mut b = new_claim("hook-claim-3b", "org/repo#601");
        b.flow_id = Some("flow-b".to_string());

        upsert_or_heartbeat_claim(&mut conn, &a).unwrap();
        upsert_or_heartbeat_claim(&mut conn, &b).unwrap();

        let all = list_claims(&conn, Some(ClaimState::Active)).unwrap();
        assert_eq!(
            all.len(),
            2,
            "distinct issue/flow identities get distinct rows"
        );
    }

    /// Regression: two issue-bound dispatches from one session sharing an
    /// issue/flow identity must NOT clobber each other's `dispatch_id`. The
    /// conflict-update in `upsert_or_heartbeat_claim` used
    /// `COALESCE(excluded.dispatch_id, session_claims.dispatch_id)`, which
    /// overwrote the existing dispatch_id whenever a later heartbeat from a
    /// DIFFERENT dispatch (same session/issue/flow) supplied its own non-null
    /// dispatch_id. Once clobbered, `get_claim_for_dispatch(original_id)`
    /// returned `None`, so the original dispatch's terminal receipt could not
    /// resolve its recipient and `release_claim_for_dispatch` could not
    /// release its claim.
    ///
    /// Fix: `dispatch_id` is immutable once set. The conflict-update now
    /// preserves the existing value via
    /// `COALESCE(session_claims.dispatch_id, excluded.dispatch_id)`.
    #[test]
    fn heartbeat_does_not_clobber_existing_dispatch_id_for_issue_bound_dispatches() {
        let mut conn = open_conn();

        // First dispatch registers the claim.
        let mut first = new_claim("claim-issue-first", "org/repo#bug");
        first.dispatch_id = Some("dispatch-original".to_string());
        let id_first = upsert_or_heartbeat_claim(&mut conn, &first).unwrap();

        // Second dispatch — same session/issue/flow, DIFFERENT dispatch_id —
        // heartbeats the existing claim rather than inserting a new row.
        let mut second = new_claim("claim-issue-second", "org/repo#bug");
        second.dispatch_id = Some("dispatch-clobberer".to_string());
        let id_second = upsert_or_heartbeat_claim(&mut conn, &second).unwrap();

        // Both resolved to the same claim row (heartbeat, not duplicate).
        assert_eq!(
            id_first, id_second,
            "same session/issue/flow identity must heartbeat, not duplicate"
        );

        // The original dispatch_id must survive — NOT overwritten by the
        // second heartbeat.
        let claim = get_claim(&conn, &id_first).unwrap().unwrap();
        assert_eq!(
            claim.dispatch_id.as_deref(),
            Some("dispatch-original"),
            "dispatch_id must be immutable once set; a later heartbeat from a \
             different dispatch must not clobber it, got: {:?}",
            claim.dispatch_id
        );

        // The original dispatch can still find its claim by dispatch_id —
        // this is the lookup `emit_terminal_receipt` and
        // `release_claim_for_dispatch` rely on. If the dispatch_id had been
        // clobbered, this would return None.
        let by_original = get_claim_for_dispatch(&conn, "dispatch-original").unwrap();
        assert!(
            by_original.is_some(),
            "get_claim_for_dispatch(original_id) must find the claim after a \
             second dispatch heartbeats it"
        );
        assert_eq!(
            by_original.unwrap().claim_id,
            id_first,
            "the claim found by original dispatch_id must be the heartbeat-resolved claim"
        );

        // The clobberer dispatch_id must NOT have replaced the original —
        // looking it up returns None (it never got its own row).
        let by_clobberer = get_claim_for_dispatch(&conn, "dispatch-clobberer").unwrap();
        assert!(
            by_clobberer.is_none(),
            "the second dispatch_id must not be findable — it heartbeated onto \
             the existing claim whose dispatch_id is immutable"
        );
    }

    /// #1001 round 3, item 2 (codex "identity upsert not atomic" finding):
    /// two REAL concurrent same-identity `upsert_or_heartbeat_claim` callers
    /// (separate OS threads, separate connections, a `Barrier` forcing both
    /// to race the `INSERT ... ON CONFLICT` at the same instant) must both
    /// return Ok, must resolve to exactly one row for the identity, and that
    /// row's heartbeat must reflect the later of the two calls — never a
    /// lost heartbeat, never an error from either side. A read-then-write
    /// upsert loses this race (both threads can observe "no existing row"
    /// before either commits); the `ON CONFLICT ... DO UPDATE` statement
    /// closes it at the database level.
    #[test]
    fn two_concurrent_same_identity_upserts_both_succeed_exactly_one_row() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        // Establish the schema (and the partial unique index) once before
        // spawning the racing connections.
        {
            let _ = open_file_conn(&path);
        }

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let path_a = path.clone();
        let barrier_a = barrier.clone();
        let handle_a = std::thread::spawn(move || {
            let mut conn = open_file_conn(&path_a);
            let mut claim = new_claim("race-claim-a", "org/repo#900");
            claim.dispatch_id = Some("dispatch-a".to_string());
            barrier_a.wait();
            upsert_or_heartbeat_claim(&mut conn, &claim)
        });

        let path_b = path.clone();
        let barrier_b = barrier;
        let handle_b = std::thread::spawn(move || {
            let mut conn = open_file_conn(&path_b);
            let mut claim = new_claim("race-claim-b", "org/repo#900");
            claim.dispatch_id = Some("dispatch-b".to_string());
            barrier_b.wait();
            upsert_or_heartbeat_claim(&mut conn, &claim)
        });

        let result_a = handle_a.join().expect("thread a must not panic");
        let result_b = handle_b.join().expect("thread b must not panic");

        assert!(
            result_a.is_ok(),
            "concurrent identity race must not error the loser: {result_a:?}"
        );
        assert!(
            result_b.is_ok(),
            "concurrent identity race must not error the winner: {result_b:?}"
        );

        let verify_conn = open_file_conn(&path);
        let active = list_claims(&verify_conn, Some(ClaimState::Active))
            .unwrap()
            .into_iter()
            .filter(|c| c.issue_ref.as_deref() == Some("org/repo#900"))
            .collect::<Vec<_>>();
        assert_eq!(
            active.len(),
            1,
            "exactly one active row must exist for the raced identity, got {active:?}"
        );

        // Both calls resolved to the SAME claim_id (whichever inserted first;
        // the other heartbeated onto it) — the two threads never produced two
        // independent claim ids.
        let claim_id_a = result_a.unwrap();
        let claim_id_b = result_b.unwrap();
        assert_eq!(
            claim_id_a, claim_id_b,
            "both concurrent callers must resolve to the identical winning claim_id"
        );
        assert_eq!(active[0].claim_id, claim_id_a);
    }
}

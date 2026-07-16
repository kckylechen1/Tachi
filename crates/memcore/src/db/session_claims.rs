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
//! [`release_claim`] is the single per-row release path — manual `release`,
//! `complete`, and `cancel` all route through it, same discipline as
//! `reclaim_exec_env`. [`gc_session_claims`] (#1001 follow-up) is the one
//! deliberate second writer: a batch sweep, not a per-row selector call, that
//! (a) reaches the identical terminal `released` state (with
//! `release_reason = "gc_stale"`) for `active` rows whose heartbeat has gone
//! dark, and (b) deletes `released` rows past a retention window — see that
//! function's doc comment for why a batch statement is used instead of N
//! calls through `release_claim`.
//!
//! [`list_active_claims`] and [`is_claim_stale`] additionally let a *reader*
//! (the briefing splice, collision-warning checks) treat a claim whose
//! `heartbeat_at` is older than a TTL as effectively expired without any
//! write at all — `gc_session_claims`'s staleness sweep does not change what
//! those reads already do, it only bounds how long a dead row sits
//! unreaped in storage. A fresh claim recycling the same `(session_client,
//! issue_ref, flow_id)` identity triple can also supersede a stale row before
//! GC ever runs (the `ON CONFLICT` target `upsert_or_heartbeat_claim` upserts
//! on — see that function's doc comment).

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
         ON CONFLICT (COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id, '')) \
             WHERE state = 'active'
         DO UPDATE SET \
             heartbeat_at = ?8, \
             dispatch_id = COALESCE(excluded.dispatch_id, session_claims.dispatch_id), \
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

/// Outcome of a [`gc_session_claims`] sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionClaimsGc {
    /// `released` rows deleted outright (aged past the audit window).
    pub released_pruned: usize,
    /// `active` rows server-released as `gc_stale` (dead heartbeat).
    pub active_staled: usize,
}

/// GC sweep for `session_claims` (#1001 follow-up; R2 review of #1007
/// CONCERN, same unbounded-growth shape as the #1029 lesson). Unlike
/// `exec_envs`, this table shipped with no reaper at all: a `released` row
/// is retained forever by design (for audit — see the module doc comment),
/// and staleness detection here is lazy-*read*-only (`is_claim_stale`,
/// `list_active_claims` filter a dead-heartbeat row out of what a *reader*
/// sees, but never write the row) — so a session that crashes or gets
/// killed mid-dispatch without ever calling `release_claim` leaves its
/// `active` row live in storage forever, and every released row (normal or
/// stale) accumulates without end.
///
/// Two independent sweeps, run in this order (order does not matter for
/// correctness — they touch disjoint row sets — but staleness-release runs
/// first so a row it flips this call is deliberately NOT also eligible for
/// the prune below in the same pass, since its fresh `released_at` cannot
/// be older than `released_max_age_days`):
///
/// 1. **Staleness release**: any `active` row whose `heartbeat_at` is older
///    than `active_staleness_days` is flipped to `released` with
///    `release_reason = 'gc_stale'` — the same terminal state a normal
///    release reaches, just server-initiated instead of caller-initiated.
///    This is a batch `UPDATE` over every stale row in one statement, not a
///    per-row call through [`release_claim`] (that function is selector-
///    scoped to one row and only fires from an explicit session action).
/// 2. **Aged-released prune**: any `released` row (from a normal release or
///    from step 1 above, in a prior or this call) whose `released_at` is
///    older than `released_max_age_days` is `DELETE`d outright — the audit
///    retention window is bounded, not permanent.
///
/// Timestamp comparison is lexicographic string comparison against a cutoff
/// formatted with the same fixed-width, zero-padded, millisecond-precision
/// RFC3339 shape every write in this module already stamps via
/// `normalize_utc_iso_or_now` (`common.rs`) — the same idiom `gc_foundry_jobs`
/// uses, safe because that format sorts identically to its chronological
/// order.
pub fn gc_session_claims(
    conn: &Connection,
    now: chrono::DateTime<chrono::Utc>,
    active_staleness_days: i64,
    released_max_age_days: i64,
) -> Result<SessionClaimsGc, MemoryError> {
    let now_iso = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let staleness_cutoff = (now - chrono::Duration::days(active_staleness_days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let active_staled = conn.execute(
        "UPDATE session_claims SET state = 'released', released_at = ?1, \
         release_reason = 'gc_stale' WHERE state = 'active' AND heartbeat_at < ?2",
        params![now_iso, staleness_cutoff],
    )?;

    let released_cutoff = (now - chrono::Duration::days(released_max_age_days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let released_pruned = conn.execute(
        "DELETE FROM session_claims WHERE state = 'released' AND released_at IS NOT NULL \
         AND released_at < ?1",
        params![released_cutoff],
    )?;

    Ok(SessionClaimsGc {
        released_pruned,
        active_staled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_conn() -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    /// A file-backed (not `:memory:`) connection, required for a real
    /// multi-connection concurrency test — two `:memory:` connections are
    /// two independent databases, so they cannot race each other at all.
    fn open_file_conn(path: &std::path::Path) -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open(path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    const AUTO_EXTENSION_RACE_CHILD: &str = "TACHI_SESSION_CLAIMS_AUTO_EXTENSION_RACE_CHILD";

    /// Runs the concurrent file-open path in a fresh test process.  The
    /// extension registry is process-global, so an in-process test could be
    /// accidentally pre-initialized by an earlier test and miss the race.
    #[test]
    fn concurrent_file_opens_do_not_race_simple_auto_extension_registration() {
        if std::env::var_os(AUTO_EXTENSION_RACE_CHILD).is_some() {
            let temp_dir = tempfile::tempdir().expect("tempdir");
            let workers = 32;
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(workers));
            let mut handles = Vec::with_capacity(workers);

            for worker in 0..workers {
                let path = temp_dir.path().join(format!("worker-{worker}.sqlite"));
                let barrier = std::sync::Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    let _conn = open_file_conn(&path);
                }));
            }

            for handle in handles {
                handle.join().expect("file-open worker must not panic");
            }
            return;
        }

        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "db::session_claims::tests::concurrent_file_opens_do_not_race_simple_auto_extension_registration",
            ])
            .env(AUTO_EXTENSION_RACE_CHILD, "1")
            .output()
            .expect("run isolated concurrent file-open test");

        assert!(
            output.status.success(),
            "concurrent file opens must not race extension registration:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
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

        // Prepare both connections before either worker reaches the rendezvous.
        // `open_file_conn` runs schema initialization, which is fallible and
        // must not happen inside one side of a two-party barrier: if it panics,
        // the peer can otherwise wait forever and hide the original failure.
        let db_path = path.to_str().expect("temporary DB path must be UTF-8");
        let mut conn_a = crate::db::open_read_write(db_path).expect("open racing connection A");
        let mut conn_b = crate::db::open_read_write(db_path).expect("open racing connection B");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut claim_a = new_claim("race-claim-a", "org/repo#900");
        claim_a.dispatch_id = Some("dispatch-a".to_string());
        let barrier_a = barrier.clone();
        let handle_a = std::thread::spawn(move || {
            barrier_a.wait();
            upsert_or_heartbeat_claim(&mut conn_a, &claim_a)
        });

        let mut claim_b = new_claim("race-claim-b", "org/repo#900");
        claim_b.dispatch_id = Some("dispatch-b".to_string());
        let barrier_b = barrier;
        let handle_b = std::thread::spawn(move || {
            barrier_b.wait();
            upsert_or_heartbeat_claim(&mut conn_b, &claim_b)
        });

        // Reap both workers before surfacing either panic so one failure cannot
        // detach the other worker from the test harness.
        let join_a = handle_a.join();
        let join_b = handle_b.join();
        let result_a = join_a.expect("thread a must not panic");
        let result_b = join_b.expect("thread b must not panic");

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

    // --- gc_session_claims (#1001 follow-up / R2 review of #1007 CONCERN) -

    #[test]
    fn gc_prunes_released_rows_older_than_max_age() {
        let mut conn = open_conn();
        insert_claim(&conn, &new_claim("aged-released", "org/repo#700")).unwrap();
        release_claim(
            &mut conn,
            &ClaimSelector::ClaimId("aged-released".to_string()),
            Some("manual release"),
        )
        .unwrap();
        // Backdate released_at to 35 days before "now" — past the 30-day
        // prune window.
        conn.execute(
            "UPDATE session_claims SET released_at = '2026-06-06T00:00:00.000Z' \
             WHERE claim_id = 'aged-released'",
            [],
        )
        .unwrap();

        let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T00:00:00Z")
            .unwrap()
            .to_utc();
        let outcome = gc_session_claims(&conn, now, 7, 30).unwrap();
        assert_eq!(outcome.released_pruned, 1);
        assert_eq!(outcome.active_staled, 0);
        assert!(
            get_claim(&conn, "aged-released").unwrap().is_none(),
            "released row older than the 30-day window must be deleted, not just marked"
        );
    }

    #[test]
    fn gc_staleness_releases_dead_active_rows_past_heartbeat_ttl() {
        let conn = open_conn();
        insert_claim(&conn, &new_claim("dead-heartbeat", "org/repo#701")).unwrap();
        // Backdate heartbeat_at to 8 days before "now" — past the 7-day
        // staleness window, simulating a crashed session that stopped
        // heartbeating and never called release_claim.
        conn.execute(
            "UPDATE session_claims SET heartbeat_at = '2026-07-03T00:00:00.000Z' \
             WHERE claim_id = 'dead-heartbeat'",
            [],
        )
        .unwrap();

        let now = chrono::DateTime::parse_from_rfc3339("2026-07-11T00:00:00Z")
            .unwrap()
            .to_utc();
        let outcome = gc_session_claims(&conn, now, 7, 30).unwrap();
        assert_eq!(outcome.active_staled, 1);
        assert_eq!(outcome.released_pruned, 0);

        let got = get_claim(&conn, "dead-heartbeat").unwrap().unwrap();
        assert_eq!(
            got.state,
            ClaimState::Released,
            "dead-heartbeat active row must be server-released, not left active forever"
        );
        assert_eq!(got.release_reason.as_deref(), Some("gc_stale"));
        assert!(got.released_at.is_some());
    }

    #[test]
    fn gc_leaves_fresh_active_row_untouched() {
        let conn = open_conn();
        insert_claim(&conn, &new_claim("fresh-claim", "org/repo#702")).unwrap();
        // heartbeat_at defaults to created_at == real "now" via new_claim's
        // empty created_at, so use the actual current time as the GC clock
        // too — this row is well within both windows either way.
        let now = chrono::Utc::now();
        let outcome = gc_session_claims(&conn, now, 7, 30).unwrap();
        assert_eq!(
            outcome.active_staled, 0,
            "a fresh active row must not be staleness-released"
        );
        assert_eq!(
            outcome.released_pruned, 0,
            "no released rows exist yet to prune"
        );

        let got = get_claim(&conn, "fresh-claim").unwrap().unwrap();
        assert_eq!(got.state, ClaimState::Active, "fresh active row untouched");
        assert!(got.released_at.is_none());
    }
}

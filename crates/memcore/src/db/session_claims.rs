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
//!   active ──heartbeat TTL elapsed──▶ orphaned
//!      │                         │
//!      └──── versioned release ──┴──▶ released
//!                                └──── versioned handoff ──▶ active
//! ```
//!
//! Legacy presence rows retain the idempotent [`release_claim`] compatibility
//! path used by manual release, complete, and cancel. v21 WorkClaims change
//! ownership only through caller-versioned release or handoff operations.
//! [`gc_session_claims`] never releases a live WorkClaim: it marks stale
//! `active` rows `orphaned` and separately deletes old `released` audit rows.
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

use crate::error::{MemoryError, WorkClaimTransitionReason};

use super::common::normalize_utc_iso_or_now;

/// Claim lifecycle state. Three states only — see the module doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// Registered and (per heartbeat-TTL expiry) presumed live.
    Active,
    /// Heartbeat-TTL expiry is evidence of an interrupted owner, never an implicit release.
    Orphaned,
    /// Released — either explicitly (`release`/`complete`/`cancel`) or
    /// superseded; the row is retained for audit and idempotent release.
    Released,
}

impl ClaimState {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimState::Active => "active",
            ClaimState::Orphaned => "orphaned",
            ClaimState::Released => "released",
        }
    }

    /// Parse a persisted state string. An unknown/legacy value is an error the
    /// caller must surface (fail-closed), matching `ExecEnvState::parse`.
    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "active" => Ok(ClaimState::Active),
            "orphaned" => Ok(ClaimState::Orphaned),
            "released" => Ok(ClaimState::Released),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown session_claim state '{other}' (expected 'active', 'orphaned', or 'released')"
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
    pub worktree_path: Option<String>,
    pub declared_file_scope: Option<String>,
    pub agent_identity_id: Option<String>,
    pub role: Option<String>,
    pub mode: Option<WorkClaimMode>,
    pub expected_head: Option<String>,
    pub lease_expires_at: Option<String>,
    pub transition_version: i64,
    pub exec_env_id: Option<String>,
    pub orphaned_at: Option<String>,
    pub state: ClaimState,
    pub release_reason: Option<String>,
    pub created_at: String,
    pub heartbeat_at: String,
    pub released_at: Option<String>,
}

/// Durable domain name for the legacy physical `session_claims` table.
pub type WorkClaim = SessionClaim;

/// Writable and read-only claims have distinct collision semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkClaimMode {
    ReadOnly,
    Writable,
}
impl WorkClaimMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Writable => "writable",
        }
    }
    fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "read_only" => Ok(Self::ReadOnly),
            "writable" => Ok(Self::Writable),
            _ => Err(MemoryError::WorkClaimIncompatibleState(format!(
                "unknown work claim mode '{raw}'"
            ))),
        }
    }
}

/// Input for a v21 WorkClaim. Unlike the compatibility `NewSessionClaim`, all
/// holder evidence required by the frozen contract is explicit.
#[derive(Debug, Clone)]
pub struct NewWorkClaim {
    pub claim_id: String,
    pub agent_identity_id: String,
    pub session_client: Option<String>,
    pub issue_ref: Option<String>,
    pub flow_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub branch: String,
    /// Canonical worktree path for writable-tree collision protection.
    pub worktree_path: String,
    pub declared_file_scope: String,
    pub role: String,
    pub mode: WorkClaimMode,
    pub expected_head: String,
    pub lease_expires_at: String,
    pub created_at: String,
}

/// Explicit successor data for a versioned WorkClaim handoff. Calling this
/// API is the only path that may move an orphaned claim to a new holder.
#[derive(Debug, Clone)]
pub struct WorkClaimHandoffRequest {
    pub agent_identity_id: String,
    pub role: String,
    pub mode: WorkClaimMode,
    pub worktree_path: String,
    pub declared_file_scope: String,
    pub expected_head: String,
    pub lease_expires_at: String,
}

/// Successful active-only lease refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkClaimHeartbeat {
    pub claim_id: String,
    pub lease_expires_at: String,
    pub transition_version: i64,
}

/// Successful explicit handoff with the new monotonic version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkClaimHandoff {
    pub claim_id: String,
    pub from_agent_identity_id: String,
    pub to_agent_identity_id: String,
    pub transition_version: i64,
}

/// Observable result for the six-way destructive-cleanup holder check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderEvidence {
    Clear,
    NotApplicable,
    Held,
    Contradictory,
    Unavailable,
    Unverifiable,
}

type HolderEvidenceRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

type WorkClaimHandoffRow = (
    Option<String>,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

/// Stable agent seat/capability identity. Display data never substitutes for
/// this opaque id in claim ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    pub agent_identity_id: String,
    pub display_name: Option<String>,
    pub seat: Option<String>,
    pub capability_json: Option<String>,
    pub created_at: String,
}

/// Public admission inputs deliberately exclude `verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnverifiedAdmissionState {
    SelfAsserted,
    Rejected,
    Unavailable,
}
impl UnverifiedAdmissionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::SelfAsserted => "self_asserted",
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
        }
    }
}

pub fn insert_agent_identity(
    conn: &Connection,
    identity: &AgentIdentity,
) -> Result<(), MemoryError> {
    if identity.agent_identity_id.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "agent identity id must be non-empty".into(),
        ));
    }
    let created = if identity.created_at.trim().is_empty() {
        normalize_utc_iso_or_now("")
    } else {
        normalize_utc_iso_or_now(&identity.created_at)
    };
    conn.execute("INSERT INTO agent_identities (agent_identity_id,display_name,seat,capability_json,created_at) VALUES (?1,?2,?3,?4,?5)", params![identity.agent_identity_id, identity.display_name, identity.seat, identity.capability_json, created])?;
    Ok(())
}

/// Record an admission that is explicitly not remote proof. The only public
/// construction path cannot create a `verified` row.
pub fn record_unverified_admission(
    conn: &Connection,
    admission_id: &str,
    agent_identity_id: &str,
    connection_id: &str,
    state: UnverifiedAdmissionState,
) -> Result<(), MemoryError> {
    if admission_id.trim().is_empty()
        || agent_identity_id.trim().is_empty()
        || connection_id.trim().is_empty()
    {
        return Err(MemoryError::InvalidArg(
            "admission id, identity id, and connection id are required".into(),
        ));
    }
    conn.execute("INSERT INTO identity_admissions (admission_id,agent_identity_id,connection_id,state,created_at) VALUES (?1,?2,?3,?4,?5)", params![admission_id, agent_identity_id, connection_id, state.as_str(), normalize_utc_iso_or_now("")])?;
    Ok(())
}

/// Persist a rejected admission even when the connection supplied no stable
/// identity that could truthfully be admitted.
pub fn record_rejected_admission(
    conn: &Connection,
    admission_id: &str,
    connection_id: &str,
    rejection_evidence: &str,
) -> Result<(), MemoryError> {
    if admission_id.trim().is_empty()
        || connection_id.trim().is_empty()
        || rejection_evidence.trim().is_empty()
    {
        return Err(MemoryError::InvalidArg(
            "admission id, connection id, and rejection evidence are required".into(),
        ));
    }
    conn.execute(
        "INSERT INTO identity_admissions \
         (admission_id,agent_identity_id,connection_id,state,rejection_evidence,created_at) \
         VALUES (?1,NULL,?2,'rejected',?3,?4)",
        params![
            admission_id,
            connection_id,
            rejection_evidence,
            normalize_utc_iso_or_now("")
        ],
    )?;
    Ok(())
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
     worktree_path, declared_file_scope, agent_identity_id, role, mode, expected_head, lease_expires_at, \
     transition_version, exec_env_id, orphaned_at, state, release_reason, created_at, heartbeat_at, released_at";

fn row_to_claim(row: &rusqlite::Row<'_>) -> Result<SessionClaim, rusqlite::Error> {
    let mode_raw: Option<String> = row.get(10)?;
    let mode = mode_raw
        .as_deref()
        .map(WorkClaimMode::parse)
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            )
        })?;
    let state_raw: String = row.get(16)?;
    let state = ClaimState::parse(&state_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            16,
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
        worktree_path: row.get(6)?,
        declared_file_scope: row.get(7)?,
        agent_identity_id: row.get(8)?,
        role: row.get(9)?,
        mode,
        expected_head: row.get(11)?,
        lease_expires_at: row.get(12)?,
        transition_version: row.get(13)?,
        exec_env_id: row.get(14)?,
        orphaned_at: row.get(15)?,
        state,
        release_reason: row.get(17)?,
        created_at: row.get(18)?,
        heartbeat_at: row.get(19)?,
        released_at: row.get(20)?,
    })
}

/// Inserts a v21 WorkClaim. Empty holder fields are refused rather than
/// silently producing an identity-unavailable row.
pub fn insert_work_claim(conn: &Connection, claim: &NewWorkClaim) -> Result<(), MemoryError> {
    if [
        claim.agent_identity_id.as_str(),
        claim.role.as_str(),
        claim.expected_head.as_str(),
        claim.lease_expires_at.as_str(),
    ]
    .iter()
    .any(|v| v.trim().is_empty())
    {
        return Err(MemoryError::InvalidArg(
            "WorkClaim identity, role, scope, expected head, and lease expiry are required".into(),
        ));
    }
    require_non_empty_declared_scope(&claim.declared_file_scope)?;
    if claim.mode == WorkClaimMode::Writable && claim.worktree_path.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "writable WorkClaim worktree path is required".into(),
        ));
    }
    let created = if claim.created_at.trim().is_empty() {
        normalize_utc_iso_or_now("")
    } else {
        normalize_utc_iso_or_now(&claim.created_at)
    };
    super::common::with_composable_write(conn, |conn| {
        let known_identity: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_identity_id=?1)",
            params![claim.agent_identity_id],
            |row| row.get(0),
        )?;
        if !known_identity {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "unknown agent identity {}",
                claim.agent_identity_id
            )));
        }
        reject_work_claim_collision(conn, claim)?;
        // v20 binaries resolve their exact active-claim upsert through the legacy
        // identity triple. v21 claims use a claim-specific compatibility key so
        // the legacy uniqueness constraint cannot serialize disjoint same-issue
        // work that the role/mode admission checks permit.
        let legacy_session_client = format!("work-claim:{}", claim.claim_id);
        conn.execute("INSERT INTO session_claims (claim_id, session_client, issue_ref, flow_id, dispatch_id, branch, worktree_path, declared_file_scope, agent_identity_id, role, mode, expected_head, lease_expires_at, transition_version, state, created_at, heartbeat_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,0,'active',?14,?14)", params![claim.claim_id, legacy_session_client, claim.issue_ref, claim.flow_id, claim.dispatch_id, claim.branch, claim.worktree_path, claim.declared_file_scope, claim.agent_identity_id, claim.role, claim.mode.as_str(), claim.expected_head, claim.lease_expires_at, created])?;
        Ok(())
    })
}

/// Only the server's admitted local context may supply this project binding.
/// It is set in the insertion transaction; no transition API can change it.
pub fn insert_local_project_work_claim(
    conn: &Connection,
    claim: &NewWorkClaim,
    connection_id: &str,
    canonical_project: &str,
) -> Result<(), MemoryError> {
    if canonical_project.trim().is_empty() {
        return Err(project_binding_unavailable());
    }
    super::common::with_composable_write(conn, |conn| {
        require_local_claim_admission(conn, &claim.agent_identity_id, connection_id)?;
        if let Some(flow_id) = claim.flow_id.as_deref() {
            let contradictory: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM session_claims WHERE flow_id=?1 \
                 AND canonical_project IS NOT NULL AND canonical_project != ?2)",
                params![flow_id, canonical_project],
                |row| row.get(0),
            )?;
            if contradictory {
                return Err(project_binding_unavailable());
            }
        }
        insert_work_claim(conn, claim)?;
        conn.execute(
            "UPDATE session_claims SET canonical_project=?1 WHERE claim_id=?2 AND canonical_project IS NULL",
            params![canonical_project, claim.claim_id],
        )?;
        Ok(())
    })
}

fn project_binding_unavailable() -> MemoryError {
    MemoryError::InvalidArg("work ownership is unavailable".into())
}

fn require_local_claim_admission(
    conn: &Connection,
    identity: &str,
    connection_id: &str,
) -> Result<(), MemoryError> {
    let admitted: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM identity_admissions WHERE agent_identity_id=?1 \
         AND connection_id=?2 AND state='self_asserted')",
        params![identity, connection_id],
        |row| row.get(0),
    )?;
    if admitted {
        Ok(())
    } else {
        Err(project_binding_unavailable())
    }
}

pub fn work_claim_project(
    conn: &Connection,
    claim_id: &str,
) -> Result<Option<String>, MemoryError> {
    conn.query_row(
        "SELECT canonical_project FROM session_claims WHERE claim_id=?1",
        [claim_id],
        |row| row.get(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(Into::into)
}

/// Filtering precedes row decoding so foreign facts never enter a projection.
pub fn list_project_work_claims(
    conn: &Connection,
    project: &str,
) -> Result<Vec<SessionClaim>, MemoryError> {
    let mut statement = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM session_claims WHERE canonical_project=?1 AND mode IS NOT NULL ORDER BY claim_id"
    ))?;
    let rows = statement.query_map([project], row_to_claim)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Bound project snapshots read at most the caller's fixed internal budget.
pub fn list_project_work_claims_bounded(
    conn: &Connection,
    project: &str,
    limit: usize,
) -> Result<Vec<SessionClaim>, MemoryError> {
    let limit =
        i64::try_from(limit).map_err(|_| MemoryError::InvalidArg("invalid claim limit".into()))?;
    let mut statement = conn.prepare(&format!("SELECT {SELECT_COLUMNS} FROM session_claims WHERE canonical_project=?1 AND mode IS NOT NULL ORDER BY claim_id LIMIT ?2"))?;
    let rows = statement.query_map(params![project, limit], row_to_claim)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Selection alone grants no authority; the caller must use the claim guard.
pub fn project_flow_claim(
    conn: &Connection,
    project: &str,
    flow_id: &str,
) -> Result<SessionClaim, MemoryError> {
    let mut statement = conn.prepare(&format!("SELECT {SELECT_COLUMNS} FROM session_claims WHERE canonical_project=?1 AND flow_id=?2 LIMIT 2"))?;
    let mut rows = statement.query_map(params![project, flow_id], row_to_claim)?;
    let first = rows
        .next()
        .transpose()?
        .ok_or_else(|| MemoryError::InvalidArg("work ownership is unavailable".into()))?;
    if rows.next().transpose()?.is_some() {
        return Err(MemoryError::InvalidArg(
            "work ownership is unavailable".into(),
        ));
    }
    Ok(first)
}

/// The advisory compatibility surface excludes project-bound work before decoding.
pub fn list_unbound_claims(
    conn: &Connection,
    state: Option<ClaimState>,
) -> Result<Vec<SessionClaim>, MemoryError> {
    let mut statement = conn.prepare(&format!("SELECT {SELECT_COLUMNS} FROM session_claims WHERE canonical_project IS NULL AND (?1 IS NULL OR state=?1) ORDER BY heartbeat_at DESC"))?;
    let rows = statement.query_map([state.map(ClaimState::as_str)], row_to_claim)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Legacy publication filters canonical rows in SQL, then preserves its TTL.
pub fn list_active_unbound_claims(
    conn: &Connection,
    now_iso: &str,
    ttl_seconds: i64,
) -> Result<Vec<SessionClaim>, MemoryError> {
    let rows = list_unbound_claims(conn, Some(ClaimState::Active))?;
    let now = chrono::DateTime::parse_from_rfc3339(now_iso)
        .map_err(|e| MemoryError::InvalidArg(format!("invalid now_iso '{now_iso}': {e}")))?;
    Ok(rows
        .into_iter()
        .filter(|claim| !is_claim_stale(claim, now.to_utc(), ttl_seconds))
        .collect())
}

pub fn with_local_project_admission<T>(
    conn: &Connection,
    identity: &str,
    connection_id: &str,
    operation: impl FnOnce(&Connection) -> Result<T, MemoryError>,
) -> Result<T, MemoryError> {
    super::common::with_composable_write(conn, |conn| {
        require_local_claim_admission(conn, identity, connection_id)?;
        operation(conn)
    })
}

/// Routing and local identity alone are not project read authority. A reader
/// must still hold an explicit active canonical claim with a live lease.
pub fn has_current_project_claim(
    conn: &Connection,
    project: &str,
    identity: &str,
    connection_id: &str,
    now: &str,
) -> Result<bool, MemoryError> {
    require_local_claim_admission(conn, identity, connection_id)?;
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_claims WHERE canonical_project=?1 AND agent_identity_id=?2 \
         AND state='active' AND mode IN ('read_only','writable') AND julianday(lease_expires_at)>julianday(?3))",
        params![project,identity,now],|row|row.get(0),
    )?)
}

/// Recheck local admission and the current holder while the database writer
/// lock excludes handoff/release/revocation. The operation must not await.
pub fn with_local_project_claim_write<T>(
    conn: &Connection,
    claim_id: &str,
    flow_id: &str,
    project: &str,
    identity: &str,
    connection_id: &str,
    operation: impl FnOnce(&Connection, &SessionClaim) -> Result<T, MemoryError>,
) -> Result<T, MemoryError> {
    super::common::with_composable_write(conn, |conn| {
        require_local_claim_admission(conn, identity, connection_id)?;
        if work_claim_project(conn, claim_id)?.as_deref() != Some(project) {
            return Err(project_binding_unavailable());
        }
        let claim = get_claim(conn, claim_id)?.ok_or_else(project_binding_unavailable)?;
        let candidates: i64 = conn.query_row(
            "SELECT count(*) FROM session_claims WHERE flow_id=?1",
            [flow_id],
            |row| row.get(0),
        )?;
        if claim.flow_id.as_deref() != Some(flow_id)
            || claim.agent_identity_id.as_deref() != Some(identity)
            || claim.state != ClaimState::Active
            || claim.mode != Some(WorkClaimMode::Writable)
            || candidates != 1
            || !claim
                .lease_expires_at
                .as_deref()
                .and_then(|lease| chrono::DateTime::parse_from_rfc3339(lease).ok())
                .is_some_and(|lease| lease > chrono::Utc::now())
        {
            return Err(project_binding_unavailable());
        }
        operation(conn, &claim)
    })
}

fn reject_work_claim_collision(
    conn: &Connection,
    incoming: &NewWorkClaim,
) -> Result<(), MemoryError> {
    let mut statement = conn.prepare(
        "SELECT claim_id, issue_ref, mode, worktree_path, declared_file_scope, expected_head
         FROM session_claims
         WHERE state IN ('active', 'orphaned') AND claim_id != ?1",
    )?;
    let existing = statement.query_map(params![incoming.claim_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    for row in existing {
        let (claim_id, issue_ref, mode, worktree_path, scope, expected_head) = row?;
        let same_issue = matches!(
            (incoming.issue_ref.as_deref(), issue_ref.as_deref()),
            (Some(incoming_issue), Some(existing_issue)) if incoming_issue == existing_issue
        );
        if same_issue
            && expected_head
                .as_deref()
                .is_some_and(|head| head != incoming.expected_head)
        {
            let issue_ref = incoming
                .issue_ref
                .as_deref()
                .expect("same_issue requires an incoming issue");
            return Err(MemoryError::WorkClaimConflict(format!(
                "issue {issue_ref} expected head conflicts with protected claim {claim_id}"
            )));
        }
        if incoming.mode != WorkClaimMode::Writable || mode.as_deref() != Some("writable") {
            continue;
        }
        if paths_overlap(
            &incoming.worktree_path,
            worktree_path.as_deref().unwrap_or(""),
        ) {
            return Err(MemoryError::WorkClaimConflict(format!(
                "writable worktree path overlaps protected claim {claim_id}"
            )));
        }
        if same_issue
            && scopes_overlap(
                &incoming.declared_file_scope,
                scope.as_deref().unwrap_or(""),
            )
        {
            let issue_ref = incoming
                .issue_ref
                .as_deref()
                .expect("same_issue requires an incoming issue");
            return Err(MemoryError::WorkClaimConflict(format!(
                "issue {issue_ref} writable file scope overlaps protected claim {claim_id}"
            )));
        }
    }
    Ok(())
}

fn scopes_overlap(left: &str, right: &str) -> bool {
    let parse = |scope: &str| {
        serde_json::from_str::<Vec<String>>(scope).unwrap_or_else(|_| vec![scope.to_string()])
    };
    let left = parse(left);
    let right = parse(right);
    left.iter()
        .any(|l| right.iter().any(|r| paths_overlap(l, r)))
}

fn require_non_empty_declared_scope(scope: &str) -> Result<(), MemoryError> {
    if scope.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "WorkClaim declared file scope is required".into(),
        ));
    }
    if let Ok(entries) = serde_json::from_str::<Vec<String>>(scope) {
        if entries.is_empty() || entries.iter().all(|entry| entry.trim().is_empty()) {
            return Err(MemoryError::InvalidArg(
                "WorkClaim declared file scope must contain at least one path".into(),
            ));
        }
    }
    Ok(())
}

fn paths_overlap(left: &str, right: &str) -> bool {
    let normalize = |path: &str| {
        path.trim()
            .trim_start_matches("./")
            .trim_end_matches("/**")
            .trim_end_matches("/*")
            .trim_end_matches('/')
            .to_string()
    };
    let left = normalize(left);
    let right = normalize(right);
    if left.is_empty() || right.is_empty() || left.contains('*') || right.contains('*') {
        return true;
    }
    left == right
        || left
            .strip_prefix(&right)
            .is_some_and(|tail| tail.starts_with('/'))
        || right
            .strip_prefix(&left)
            .is_some_and(|tail| tail.starts_with('/'))
}

/// Compare-and-swap release. A stale version is a typed conflict; orphaned
/// claims require this explicit path and can never be released by expiry.
pub fn heartbeat_work_claim(
    conn: &Connection,
    claim_id: &str,
    caller_identity_id: &str,
    expected_version: i64,
    lease_expires_at: &str,
) -> Result<WorkClaimHeartbeat, MemoryError> {
    if lease_expires_at.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "WorkClaim lease expiry is required".into(),
        ));
    }
    super::common::with_composable_write(conn, |conn| {
        let existing: Option<(Option<String>, String, i64)> = conn
            .query_row(
            "SELECT agent_identity_id, state, transition_version FROM session_claims WHERE claim_id=?1",
            params![claim_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
        let Some((holder_identity_id, state, version)) = existing else {
            return Err(MemoryError::NotFound(format!("claim {claim_id}")));
        };
        if state != "active" {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "claim {claim_id} is {state}; only active claims can heartbeat"
            )));
        }
        if version != expected_version {
            return Err(MemoryError::WorkClaimConflict(format!(
                "claim {claim_id} is at version {version}, expected {expected_version}"
            )));
        }
        let holder_identity_id = holder_identity_id.ok_or_else(|| {
            MemoryError::WorkClaimIncompatibleState(format!(
                "claim {claim_id} has no proven identity"
            ))
        })?;
        if holder_identity_id != caller_identity_id {
            return Err(MemoryError::WorkClaimTransitionRefused {
                reason: WorkClaimTransitionReason::HolderMismatch,
                claim_id: claim_id.to_string(),
                holder_identity_id,
                caller_identity_id: caller_identity_id.to_string(),
            });
        }
        let now = normalize_utc_iso_or_now("");
        let changed = conn.execute(
            "UPDATE session_claims SET heartbeat_at=?3, lease_expires_at=?4, \
             transition_version=transition_version+1 \
             WHERE claim_id=?1 AND state='active' AND transition_version=?2",
            params![claim_id, expected_version, now, lease_expires_at],
        )?;
        if changed != 1 {
            return Err(MemoryError::WorkClaimConflict(format!(
                "claim {claim_id} changed while heartbeating"
            )));
        }
        Ok(WorkClaimHeartbeat {
            claim_id: claim_id.to_string(),
            lease_expires_at: lease_expires_at.to_string(),
            transition_version: expected_version + 1,
        })
    })
}

/// Pure request validation shared with server-side hidden-target admission.
pub fn validate_work_claim_handoff(successor: &WorkClaimHandoffRequest) -> Result<(), MemoryError> {
    require_non_empty_declared_scope(&successor.declared_file_scope)?;
    if successor.mode == WorkClaimMode::Writable && successor.worktree_path.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "writable WorkClaim worktree path is required".into(),
        ));
    }
    Ok(())
}

/// Explicitly transfer an active or orphaned claim after a version check.
/// This is intentionally not an automatic takeover path.
pub fn handoff_work_claim(
    conn: &Connection,
    claim_id: &str,
    caller_identity_id: &str,
    expected_version: i64,
    successor: &WorkClaimHandoffRequest,
) -> Result<WorkClaimHandoff, MemoryError> {
    validate_work_claim_handoff(successor)?;
    super::common::with_composable_write(conn, |conn| {
        let existing: Option<WorkClaimHandoffRow> = conn.query_row(
            "SELECT agent_identity_id, state, transition_version, session_client, issue_ref, flow_id, dispatch_id, branch \
             FROM session_claims WHERE claim_id=?1",
            params![claim_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
        ).optional()?;
        let Some((
            Some(from_identity),
            state,
            version,
            session_client,
            issue_ref,
            flow_id,
            dispatch_id,
            branch,
        )) = existing
        else {
            return match existing {
                Some(_) => Err(MemoryError::WorkClaimIncompatibleState(format!(
                    "claim {claim_id} has no proven identity"
                ))),
                None => Err(MemoryError::NotFound(format!("claim {claim_id}"))),
            };
        };
        if !matches!(state.as_str(), "active" | "orphaned") {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "claim {claim_id} is {state}"
            )));
        }
        if version != expected_version {
            return Err(MemoryError::WorkClaimConflict(format!(
                "claim {claim_id} is at version {version}, expected {expected_version}"
            )));
        }
        if successor.agent_identity_id != caller_identity_id {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "handoff successor {} does not match admitted caller {caller_identity_id}",
                successor.agent_identity_id
            )));
        }
        if state == "active" && from_identity != caller_identity_id {
            return Err(MemoryError::WorkClaimTransitionRefused {
                reason: WorkClaimTransitionReason::HolderMismatch,
                claim_id: claim_id.to_string(),
                holder_identity_id: from_identity,
                caller_identity_id: caller_identity_id.to_string(),
            });
        }
        let known_successor: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_identity_id=?1)",
            params![successor.agent_identity_id],
            |row| row.get(0),
        )?;
        if !known_successor {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "unknown successor identity {}",
                successor.agent_identity_id
            )));
        }
        let collision_candidate = NewWorkClaim {
            claim_id: claim_id.to_string(),
            agent_identity_id: successor.agent_identity_id.clone(),
            session_client,
            issue_ref,
            flow_id,
            dispatch_id,
            branch,
            worktree_path: successor.worktree_path.clone(),
            declared_file_scope: successor.declared_file_scope.clone(),
            role: successor.role.clone(),
            mode: successor.mode,
            expected_head: successor.expected_head.clone(),
            lease_expires_at: successor.lease_expires_at.clone(),
            created_at: String::new(),
        };
        reject_work_claim_collision(conn, &collision_candidate)?;
        let env_id: Option<String> = conn.query_row(
            "SELECT exec_env_id FROM session_claims WHERE claim_id=?1",
            params![claim_id],
            |row| row.get(0),
        )?;
        if let Some(env_id) = env_id {
            let env_matches: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM exec_envs WHERE env_id=?1 AND claim_id=?2 AND agent_identity_id=?3)",
                params![env_id, claim_id, from_identity], |row| row.get(0),
            )?;
            if !env_matches {
                return Err(MemoryError::WorkClaimConflict(format!(
                    "claim {claim_id} has contradictory exec env binding {env_id}"
                )));
            }
            conn.execute("UPDATE exec_envs SET agent_identity_id=?2 WHERE env_id=?1 AND claim_id=?3 AND agent_identity_id=?4", params![env_id, successor.agent_identity_id, claim_id, from_identity])?;
        }
        let now = normalize_utc_iso_or_now("");
        let changed = conn.execute(
            "UPDATE session_claims SET agent_identity_id=?3, role=?4, mode=?5, worktree_path=?6, \
             declared_file_scope=?7, expected_head=?8, lease_expires_at=?9, state='active', \
             orphaned_at=NULL, heartbeat_at=?10, transition_version=transition_version+1 \
             WHERE claim_id=?1 AND transition_version=?2 AND state IN ('active','orphaned')",
            params![
                claim_id,
                expected_version,
                successor.agent_identity_id,
                successor.role,
                successor.mode.as_str(),
                successor.worktree_path,
                successor.declared_file_scope,
                successor.expected_head,
                successor.lease_expires_at,
                now
            ],
        )?;
        if changed != 1 {
            return Err(MemoryError::WorkClaimConflict(format!(
                "claim {claim_id} changed while handing off"
            )));
        }
        Ok(WorkClaimHandoff {
            claim_id: claim_id.to_string(),
            from_agent_identity_id: from_identity,
            to_agent_identity_id: successor.agent_identity_id.clone(),
            transition_version: expected_version + 1,
        })
    })
}

pub fn release_work_claim(
    conn: &Connection,
    claim_id: &str,
    caller_identity_id: &str,
    expected_version: i64,
    reason: &str,
) -> Result<i64, MemoryError> {
    super::common::with_composable_write(conn, |conn| {
        let present: Option<(Option<String>, String, i64)> = conn
            .query_row(
                "SELECT agent_identity_id, state, transition_version FROM session_claims WHERE claim_id=?1",
                params![claim_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((holder_identity_id, state, version)) = present else {
            return Err(MemoryError::NotFound(format!("claim {claim_id}")));
        };
        if !matches!(state.as_str(), "active" | "orphaned") {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "claim {claim_id} is {state}"
            )));
        }
        if version != expected_version {
            return Err(MemoryError::WorkClaimConflict(format!(
                "claim {claim_id} is at version {version}, expected {expected_version}"
            )));
        }
        let holder_identity_id = holder_identity_id.ok_or_else(|| {
            MemoryError::WorkClaimIncompatibleState(format!(
                "claim {claim_id} has no proven identity"
            ))
        })?;
        if holder_identity_id != caller_identity_id {
            return Err(MemoryError::WorkClaimTransitionRefused {
                reason: WorkClaimTransitionReason::HolderMismatch,
                claim_id: claim_id.to_string(),
                holder_identity_id,
                caller_identity_id: caller_identity_id.to_string(),
            });
        }
        let changed = conn.execute("UPDATE session_claims SET state='released', released_at=?4, release_reason=?3, transition_version=transition_version+1 WHERE claim_id=?1 AND transition_version=?2 AND state IN ('active','orphaned')", params![claim_id, expected_version, reason, normalize_utc_iso_or_now("")])?;
        if changed != 1 {
            return Err(MemoryError::WorkClaimConflict(format!(
                "claim {claim_id} changed while releasing"
            )));
        }
        Ok(expected_version + 1)
    })
}

/// Atomically bind a claim and ExecEnv, refusing unproved identities, stale
/// versions, and any existing contradictory binding.
pub fn bind_work_claim_exec_env(
    conn: &mut Connection,
    claim_id: &str,
    env_id: &str,
    expected_version: i64,
) -> Result<i64, MemoryError> {
    let tx = conn.transaction()?;
    let claim: Option<(Option<String>, String, i64, Option<String>)> = tx.query_row("SELECT agent_identity_id,state,transition_version,exec_env_id FROM session_claims WHERE claim_id=?1", params![claim_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((Some(identity), state, version, existing_env)) = claim else {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "claim {claim_id} has no proven identity"
        )));
    };
    if state != "active" {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "claim {claim_id} is {state}"
        )));
    }
    if version != expected_version {
        return Err(MemoryError::WorkClaimConflict(format!(
            "claim {claim_id} is at version {version}, expected {expected_version}"
        )));
    }
    let env: Option<(Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT agent_identity_id,claim_id FROM exec_envs WHERE env_id=?1",
            params![env_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((env_identity, env_claim)) = env else {
        return Err(MemoryError::NotFound(format!("exec env {env_id}")));
    };
    if existing_env.as_deref().is_some_and(|id| id != env_id)
        || env_identity.as_deref().is_some_and(|id| id != identity)
        || env_claim.as_deref().is_some_and(|id| id != claim_id)
    {
        return Err(MemoryError::WorkClaimConflict(format!(
            "claim {claim_id} and exec env {env_id} already have incompatible bindings"
        )));
    }
    let changed = tx.execute("UPDATE session_claims SET exec_env_id=?2, transition_version=transition_version+1 WHERE claim_id=?1 AND transition_version=?3 AND exec_env_id IS NULL", params![claim_id, env_id, expected_version])?;
    if changed != 1 {
        return Err(MemoryError::WorkClaimConflict(format!(
            "claim {claim_id} changed while binding"
        )));
    }
    let changed = tx.execute("UPDATE exec_envs SET agent_identity_id=?2, claim_id=?3 WHERE env_id=?1 AND agent_identity_id IS NULL AND claim_id IS NULL", params![env_id, identity, claim_id])?;
    if changed != 1 {
        return Err(MemoryError::WorkClaimConflict(format!(
            "exec env {env_id} changed while binding"
        )));
    }
    tx.commit()?;
    Ok(expected_version + 1)
}

/// Query the complete holder evidence before a destructive ExecEnv action.
pub fn holder_evidence(conn: &Connection, env_id: &str) -> Result<HolderEvidence, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT e.agent_identity_id,e.claim_id,c.claim_id,c.agent_identity_id,c.state,c.exec_env_id \
         FROM exec_envs e \
         LEFT JOIN session_claims c ON c.claim_id=e.claim_id OR c.exec_env_id=e.env_id \
         WHERE e.env_id=?1 \
         ORDER BY c.claim_id",
    )?;
    let rows = statement
        .query_map(params![env_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<Vec<HolderEvidenceRow>, _>>()?;
    let [(env_identity, env_claim_id, claim_id, claim_identity, state, claim_env)] =
        rows.as_slice()
    else {
        return if rows.is_empty() {
            Ok(HolderEvidence::Unverifiable)
        } else {
            Ok(HolderEvidence::Contradictory)
        };
    };
    match (
        env_identity,
        env_claim_id,
        claim_id,
        claim_identity,
        state,
        claim_env,
    ) {
        (None, None, None, None, None, None) => Ok(HolderEvidence::NotApplicable),
        (_, Some(_), None, None, None, None) => Ok(HolderEvidence::Unverifiable),
        (Some(ei), Some(ec), Some(cid), Some(ci), Some(state), Some(ce))
            if ei == ci && ec == cid && ce == env_id =>
        {
            match state.as_str() {
                "released" => Ok(HolderEvidence::Clear),
                "active" | "orphaned" => Ok(HolderEvidence::Held),
                _ => Ok(HolderEvidence::Unavailable),
            }
        }
        _ => Ok(HolderEvidence::Contradictory),
    }
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

/// Legacy consumers must not decode or authorize project-bound claims.
pub fn get_unbound_claim(
    conn: &Connection,
    claim_id: &str,
) -> Result<Option<SessionClaim>, MemoryError> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM session_claims WHERE claim_id=?1 AND canonical_project IS NULL");
    Ok(conn.query_row(&sql, [claim_id], row_to_claim).optional()?)
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
/// ''))  WHERE state = 'active' AND mode IS NULL` — SQLite requires the `ON
/// CONFLICT` target to match an existing unique index verbatim (same
/// expressions, same partial predicate) to resolve against it, and a
/// mismatched target here would make this silently fall back to raising
/// `UNIQUE constraint failed` instead of upserting. A prior read-then-write
/// (`SELECT` to check existence, then a separate `UPDATE`/`INSERT`) is a
/// TOCTOU race even inside a transaction: SQLite's default deferred
/// transaction does not take a write lock until its first write, so two
/// concurrent callers could both `SELECT` "no existing row" before either
/// commits, and one of the two `INSERT`s would then fail the unique index
/// with no path to convert that failure into a heartbeat — the very race
/// #1001 round 2's index was added to catch, but the application code never
/// closed. `INSERT ... ON CONFLICT ... DO UPDATE` is a single statement the
/// database resolves under one lock: concurrent same-identity callers each
/// either insert (if they win the race) or update the winner's row (if they
/// lose it) — both succeed, neither errors, and exactly one row exists for
/// the identity afterward.
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
             WHERE state = 'active' AND mode IS NULL
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
    let existing: Option<(String, String, Option<String>, Option<String>)> = match selector {
        ClaimSelector::ClaimId(claim_id) => tx
            .query_row(
                "SELECT claim_id, state, mode, agent_identity_id FROM session_claims WHERE claim_id = ?1",
                params![claim_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?,
        ClaimSelector::DispatchId(dispatch_id) => tx
            .query_row(
                "SELECT claim_id, state, mode, agent_identity_id FROM session_claims WHERE dispatch_id = ?1 \
                 AND mode IS NULL AND agent_identity_id IS NULL \
                 ORDER BY CASE state WHEN 'active' THEN 0 ELSE 1 END, heartbeat_at DESC LIMIT 1",
                params![dispatch_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?,
    };
    if existing.is_none() {
        if let ClaimSelector::DispatchId(dispatch_id) = selector {
            let has_v21: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM session_claims WHERE dispatch_id = ?1 \
                 AND (mode IS NOT NULL OR agent_identity_id IS NOT NULL))",
                params![dispatch_id],
                |row| row.get(0),
            )?;
            if has_v21 {
                return Err(MemoryError::WorkClaimIncompatibleState(format!(
                    "dispatch_id {dispatch_id} targets a v21 WorkClaim; use canonical tachi_task release with caller-provided transition_version"
                )));
            }
        }
    }

    let outcome = match existing {
        None => ReleaseOutcome::NotFound,
        Some((claim_id, state_raw, mode, agent_identity_id)) => {
            if mode.is_some() || agent_identity_id.is_some() {
                return Err(MemoryError::WorkClaimIncompatibleState(format!(
                    "claim {claim_id} targets a v21 WorkClaim; use canonical tachi_task release with caller-provided transition_version"
                )));
            }
            match ClaimState::parse(&state_raw)? {
                ClaimState::Released => ReleaseOutcome::AlreadyReleased { claim_id },
                ClaimState::Orphaned => {
                    return Err(MemoryError::WorkClaimIncompatibleState(format!(
                        "claim {claim_id} is orphaned; use versioned release_work_claim"
                    )))
                }
                ClaimState::Active => {
                    let now = normalize_utc_iso_or_now("");
                    tx.execute(
                        "UPDATE session_claims SET state = 'released', released_at = ?2, \
                     release_reason = ?3 WHERE claim_id = ?1 AND state = 'active'",
                        params![claim_id, now, reason],
                    )?;
                    ReleaseOutcome::Released { claim_id }
                }
            }
        }
    };
    tx.commit()?;
    Ok(outcome)
}

/// Outcome of a [`gc_session_claims`] sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionClaimsGc {
    /// `released` rows deleted outright (aged past the audit window).
    pub released_pruned: usize,
    /// `active` rows moved to `orphaned` after lease expiry (dead heartbeat).
    pub active_orphaned: usize,
}

/// GC sweep for `session_claims` (#1001 follow-up; R2 review of #1007
/// CONCERN, same unbounded-growth shape as the #1029 lesson). Unlike
/// `exec_envs`, this table shipped with no reaper at all: a `released` row
/// is retained forever by design (for audit — see the module doc comment),
/// and staleness detection here is lazy-*read*-only (`is_claim_stale`,
/// `list_active_claims` filter a dead-heartbeat row out of what a *reader*
/// sees, but never write the row) — so a session that crashes or gets
/// killed mid-dispatch without ever releasing or handing off its claim leaves
/// its `active` row live in storage forever, and every released row accumulates
/// without end.
///
/// Two independent sweeps, run in this order (order does not matter for
/// correctness — they touch disjoint row sets — but orphaning runs first so
/// stale ownership becomes visible before old released audit rows are pruned):
///
/// 1. **Staleness orphaning**: any `active` row whose `heartbeat_at` is older
///    than `active_staleness_days` is flipped to `orphaned`. The server does
///    not invent release authority; an explicit caller-versioned release or
///    handoff is still required to leave the orphaned state.
/// 2. **Aged-released prune**: any `released` row whose `released_at` is older
///    than `released_max_age_days` is `DELETE`d outright — the audit retention
///    window is bounded, not permanent.
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
    let active_orphaned = conn.execute(
        "UPDATE session_claims SET state = 'orphaned', orphaned_at = ?1, \
         release_reason = NULL WHERE state = 'active' AND heartbeat_at < ?2",
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
        active_orphaned,
    })
}

/// tachi#1679: the requester binding facts of the WorkClaim that owns a
/// managed dispatch. `state` carries the claim's own lifecycle token so the
/// delivery mint can refuse inactive owners.
pub struct ClaimRequesterBinding {
    pub claim_id: String,
    pub agent_identity_id: String,
    pub session_client: Option<String>,
    pub state: String,
}

/// tachi#1679: the requester binding for a managed dispatch, read from the
/// WorkClaim that owns the dispatch (claim id, identity, session client,
/// claim state). `None` when no claim names the dispatch — the delivery
/// intent then mints unbound (pull-only) instead of guessing a requester.
pub fn find_claim_requester_for_dispatch(
    conn: &Connection,
    dispatch_id: &str,
) -> Result<Option<ClaimRequesterBinding>, MemoryError> {
    if dispatch_id.trim().is_empty() {
        return Ok(None);
    }
    let mut stmt = conn.prepare(
        "SELECT claim_id, agent_identity_id, session_client, state FROM session_claims
         WHERE dispatch_id = ?1 AND agent_identity_id IS NOT NULL
         ORDER BY created_at DESC LIMIT 1",
    )?;
    let found = stmt
        .query_row(params![dispatch_id], |row| {
            Ok(ClaimRequesterBinding {
                claim_id: row.get(0)?,
                agent_identity_id: row.get(1)?,
                session_client: row.get(2)?,
                state: row.get(3)?,
            })
        })
        .optional()?;
    Ok(found)
}

#[cfg(test)]
mod tests;

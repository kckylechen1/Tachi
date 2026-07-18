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
//!   active ──lease expiry──▶ orphaned
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

use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::error::{MemoryError, WorkClaimTransitionReason};

use super::common::normalize_utc_iso_or_now;

/// Claim lifecycle state. Two states only — see the module doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// Registered and (per lazy TTL expiry) presumed live.
    Active,
    /// Lease expiry is evidence of an interrupted owner, never an implicit release.
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

/// Persisted admission state. `Verified` is readable but has no public writer
/// until the trusted #1170 verification adapter exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionState {
    SelfAsserted,
    Verified,
    Rejected,
    Unavailable,
}
impl AdmissionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::SelfAsserted => "self_asserted",
            Self::Verified => "verified",
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Public admission inputs deliberately exclude `verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnverifiedAdmissionState {
    SelfAsserted,
    Rejected,
    Unavailable,
}
impl From<UnverifiedAdmissionState> for AdmissionState {
    fn from(value: UnverifiedAdmissionState) -> Self {
        match value {
            UnverifiedAdmissionState::SelfAsserted => Self::SelfAsserted,
            UnverifiedAdmissionState::Rejected => Self::Rejected,
            UnverifiedAdmissionState::Unavailable => Self::Unavailable,
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
    conn.execute("INSERT INTO identity_admissions (admission_id,agent_identity_id,connection_id,state,created_at) VALUES (?1,?2,?3,?4,?5)", params![admission_id, agent_identity_id, connection_id, AdmissionState::from(state).as_str(), normalize_utc_iso_or_now("")])?;
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
pub fn insert_work_claim(conn: &mut Connection, claim: &NewWorkClaim) -> Result<(), MemoryError> {
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
    let known_identity: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_identity_id=?1)",
        params![claim.agent_identity_id],
        |r| r.get(0),
    )?;
    if !known_identity {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "unknown agent identity {}",
            claim.agent_identity_id
        )));
    }
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
    let tx = conn.transaction()?;
    reject_work_claim_collision(&tx, claim)?;
    // v20 binaries resolve their exact active-claim upsert through the legacy
    // identity triple. v21 claims use a claim-specific compatibility key so
    // the legacy uniqueness constraint cannot serialize disjoint same-issue
    // work that the role/mode admission checks permit.
    let legacy_session_client = format!("work-claim:{}", claim.claim_id);
    tx.execute("INSERT INTO session_claims (claim_id, session_client, issue_ref, flow_id, dispatch_id, branch, worktree_path, declared_file_scope, agent_identity_id, role, mode, expected_head, lease_expires_at, transition_version, state, created_at, heartbeat_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,0,'active',?14,?14)", params![claim.claim_id, legacy_session_client, claim.issue_ref, claim.flow_id, claim.dispatch_id, claim.branch, claim.worktree_path, claim.declared_file_scope, claim.agent_identity_id, claim.role, claim.mode.as_str(), claim.expected_head, claim.lease_expires_at, created])?;
    tx.commit()?;
    Ok(())
}

fn reject_work_claim_collision(
    tx: &Transaction<'_>,
    incoming: &NewWorkClaim,
) -> Result<(), MemoryError> {
    let Some(issue_ref) = incoming.issue_ref.as_deref() else {
        return Ok(());
    };
    let mut statement = tx.prepare(
        "SELECT claim_id, mode, worktree_path, declared_file_scope, expected_head
         FROM session_claims
         WHERE issue_ref = ?1 AND state IN ('active', 'orphaned') AND claim_id != ?2",
    )?;
    let existing = statement.query_map(params![issue_ref, incoming.claim_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in existing {
        let (claim_id, mode, worktree_path, scope, expected_head) = row?;
        if expected_head
            .as_deref()
            .is_some_and(|head| head != incoming.expected_head)
        {
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
                "issue {issue_ref} writable worktree path overlaps protected claim {claim_id}"
            )));
        }
        if scopes_overlap(
            &incoming.declared_file_scope,
            scope.as_deref().unwrap_or(""),
        ) {
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
    conn: &mut Connection,
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
    let tx = conn.transaction()?;
    let existing: Option<(Option<String>, String, i64)> = tx
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
        MemoryError::WorkClaimIncompatibleState(format!("claim {claim_id} has no proven identity"))
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
    let changed = tx.execute(
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
    tx.commit()?;
    Ok(WorkClaimHeartbeat {
        claim_id: claim_id.to_string(),
        lease_expires_at: lease_expires_at.to_string(),
        transition_version: expected_version + 1,
    })
}

/// Explicitly transfer an active or orphaned claim after a version check.
/// This is intentionally not an automatic takeover path.
pub fn handoff_work_claim(
    conn: &mut Connection,
    claim_id: &str,
    caller_identity_id: &str,
    expected_version: i64,
    successor: &WorkClaimHandoffRequest,
) -> Result<WorkClaimHandoff, MemoryError> {
    require_non_empty_declared_scope(&successor.declared_file_scope)?;
    let tx = conn.transaction()?;
    let existing: Option<WorkClaimHandoffRow> = tx.query_row(
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
    let known_successor: bool = tx.query_row(
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
    if successor.mode == WorkClaimMode::Writable && successor.worktree_path.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "writable WorkClaim worktree path is required".into(),
        ));
    }
    reject_work_claim_collision(&tx, &collision_candidate)?;
    let env_id: Option<String> = tx.query_row(
        "SELECT exec_env_id FROM session_claims WHERE claim_id=?1",
        params![claim_id],
        |row| row.get(0),
    )?;
    if let Some(env_id) = env_id {
        let env_matches: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM exec_envs WHERE env_id=?1 AND claim_id=?2 AND agent_identity_id=?3)",
            params![env_id, claim_id, from_identity], |row| row.get(0),
        )?;
        if !env_matches {
            return Err(MemoryError::WorkClaimConflict(format!(
                "claim {claim_id} has contradictory exec env binding {env_id}"
            )));
        }
        tx.execute("UPDATE exec_envs SET agent_identity_id=?2 WHERE env_id=?1 AND claim_id=?3 AND agent_identity_id=?4", params![env_id, successor.agent_identity_id, claim_id, from_identity])?;
    }
    let now = normalize_utc_iso_or_now("");
    let changed = tx.execute(
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
    tx.commit()?;
    Ok(WorkClaimHandoff {
        claim_id: claim_id.to_string(),
        from_agent_identity_id: from_identity,
        to_agent_identity_id: successor.agent_identity_id.clone(),
        transition_version: expected_version + 1,
    })
}

pub fn release_work_claim(
    conn: &mut Connection,
    claim_id: &str,
    caller_identity_id: &str,
    expected_version: i64,
    reason: &str,
) -> Result<i64, MemoryError> {
    let tx = conn.transaction()?;
    let present: Option<(Option<String>, String, i64)> = tx
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
        MemoryError::WorkClaimIncompatibleState(format!("claim {claim_id} has no proven identity"))
    })?;
    if holder_identity_id != caller_identity_id {
        return Err(MemoryError::WorkClaimTransitionRefused {
            reason: WorkClaimTransitionReason::HolderMismatch,
            claim_id: claim_id.to_string(),
            holder_identity_id,
            caller_identity_id: caller_identity_id.to_string(),
        });
    }
    let changed = tx.execute("UPDATE session_claims SET state='released', released_at=?4, release_reason=?3, transition_version=transition_version+1 WHERE claim_id=?1 AND transition_version=?2 AND state IN ('active','orphaned')", params![claim_id, expected_version, reason, normalize_utc_iso_or_now("")])?;
    if changed != 1 {
        return Err(MemoryError::WorkClaimConflict(format!(
            "claim {claim_id} changed while releasing"
        )));
    }
    tx.commit()?;
    Ok(expected_version + 1)
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
    fn release_by_claim_id_rejects_v21_work_claim_alias() {
        let mut conn = open_conn();
        identity(&conn, "agent-v21-claim-id");
        insert_work_claim(
            &mut conn,
            &work_claim("claim-v21-direct", "agent-v21-claim-id"),
        )
        .unwrap();

        let err = release_claim(
            &mut conn,
            &ClaimSelector::ClaimId("claim-v21-direct".to_string()),
            Some("legacy alias"),
        )
        .expect_err("legacy release must reject direct v21 WorkClaim aliases");
        let text = err.to_string();
        assert!(text.contains("canonical tachi_task release"), "{text}");
        assert!(text.contains("transition_version"), "{text}");

        let got = get_claim(&conn, "claim-v21-direct").unwrap().unwrap();
        assert_eq!(got.state, ClaimState::Active);
        assert_eq!(got.transition_version, 0);
        assert!(got.released_at.is_none());
        assert!(got.release_reason.is_none());
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
    fn release_by_dispatch_id_rejects_v21_work_claim_alias() {
        let mut conn = open_conn();
        identity(&conn, "agent-v21-dispatch");
        let mut claim = work_claim("claim-v21-dispatch", "agent-v21-dispatch");
        claim.dispatch_id = Some("dispatch-v21-only".to_string());
        insert_work_claim(&mut conn, &claim).unwrap();

        let err = release_claim(
            &mut conn,
            &ClaimSelector::DispatchId("dispatch-v21-only".to_string()),
            Some("complete"),
        )
        .expect_err("legacy release must reject v21 WorkClaim dispatch aliases");
        let text = err.to_string();
        assert!(text.contains("canonical tachi_task release"), "{text}");
        assert!(text.contains("transition_version"), "{text}");

        let got = get_claim(&conn, "claim-v21-dispatch").unwrap().unwrap();
        assert_eq!(got.state, ClaimState::Active);
        assert_eq!(got.transition_version, 0);
        assert!(got.released_at.is_none());
        assert!(got.release_reason.is_none());
    }

    #[test]
    fn release_by_dispatch_id_releases_only_legacy_presence_when_mixed_with_v21_work_claim() {
        let mut conn = open_conn();
        identity(&conn, "agent-v21-mixed");

        let mut v21 = work_claim("claim-v21-mixed", "agent-v21-mixed");
        v21.dispatch_id = Some("dispatch-mixed".to_string());
        v21.created_at = "2030-01-01T00:00:00Z".to_string();
        insert_work_claim(&mut conn, &v21).unwrap();

        let mut legacy = new_claim("claim-legacy-mixed", "org/repo#legacy-mixed");
        legacy.dispatch_id = Some("dispatch-mixed".to_string());
        legacy.created_at = "2020-01-01T00:00:00Z".to_string();
        insert_claim(&conn, &legacy).unwrap();

        let outcome = release_claim(
            &mut conn,
            &ClaimSelector::DispatchId("dispatch-mixed".to_string()),
            Some("complete"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReleaseOutcome::Released {
                claim_id: "claim-legacy-mixed".to_string()
            }
        );

        let released_legacy = get_claim(&conn, "claim-legacy-mixed").unwrap().unwrap();
        assert_eq!(released_legacy.state, ClaimState::Released);
        assert_eq!(released_legacy.release_reason.as_deref(), Some("complete"));
        assert!(released_legacy.released_at.is_some());

        let active_v21 = get_claim(&conn, "claim-v21-mixed").unwrap().unwrap();
        assert_eq!(active_v21.state, ClaimState::Active);
        assert_eq!(active_v21.transition_version, 0);
        assert!(active_v21.released_at.is_none());
        assert!(active_v21.release_reason.is_none());
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
            worktree_path: None,
            declared_file_scope: None,
            agent_identity_id: None,
            role: None,
            mode: None,
            expected_head: None,
            lease_expires_at: None,
            transition_version: 0,
            exec_env_id: None,
            orphaned_at: None,
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
        assert_eq!(outcome.active_orphaned, 0);
        assert!(
            get_claim(&conn, "aged-released").unwrap().is_none(),
            "released row older than the 30-day window must be deleted, not just marked"
        );
    }

    #[test]
    fn gc_staleness_orphans_dead_active_rows_past_heartbeat_ttl() {
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
        assert_eq!(outcome.active_orphaned, 1);
        assert_eq!(outcome.released_pruned, 0);

        let got = get_claim(&conn, "dead-heartbeat").unwrap().unwrap();
        assert_eq!(
            got.state,
            ClaimState::Orphaned,
            "dead-heartbeat active row must be orphaned, never server-released"
        );
        assert!(got.release_reason.is_none());
        assert!(got.orphaned_at.is_some());
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
            outcome.active_orphaned, 0,
            "a fresh active row must not be orphaned"
        );
        assert_eq!(
            outcome.released_pruned, 0,
            "no released rows exist yet to prune"
        );

        let got = get_claim(&conn, "fresh-claim").unwrap().unwrap();
        assert_eq!(got.state, ClaimState::Active, "fresh active row untouched");
        assert!(got.released_at.is_none());
    }

    fn identity(conn: &Connection, id: &str) {
        insert_agent_identity(
            conn,
            &AgentIdentity {
                agent_identity_id: id.into(),
                display_name: Some("display only".into()),
                seat: None,
                capability_json: None,
                created_at: String::new(),
            },
        )
        .unwrap();
    }

    fn work_claim(id: &str, identity_id: &str) -> NewWorkClaim {
        NewWorkClaim {
            claim_id: id.into(),
            agent_identity_id: identity_id.into(),
            session_client: None,
            issue_ref: Some("org/repo#1253".into()),
            flow_id: None,
            dispatch_id: None,
            branch: "lane/1253".into(),
            worktree_path: "/wt/1253".into(),
            declared_file_scope: "crates/memcore/src/db/**".into(),
            role: "executor".into(),
            mode: WorkClaimMode::Writable,
            expected_head: "3c09b425".into(),
            lease_expires_at: "2026-07-19T00:00:00Z".into(),
            created_at: String::new(),
        }
    }

    #[test]
    fn v21_claim_binding_is_atomic_and_holder_evidence_is_six_way_queryable() {
        let mut conn = open_conn();
        identity(&conn, "agent-a");
        record_unverified_admission(
            &conn,
            "admission-a",
            "agent-a",
            "connection-a",
            UnverifiedAdmissionState::SelfAsserted,
        )
        .unwrap();
        insert_work_claim(&mut conn, &work_claim("claim-a", "agent-a")).unwrap();
        crate::db::exec_env::insert_exec_env(
            &conn,
            &crate::db::exec_env::NewExecEnvLease {
                env_id: "env-a".into(),
                kind: "worktree".into(),
                path: "/wt/a".into(),
                repo_root: "/repo".into(),
                branch: "lane/1253".into(),
                base_sha: "3c09b425".into(),
                dispatch_id: None,
                env_class: crate::db::exec_env::EnvClass::EditOnly,
                created_at: String::new(),
            },
        )
        .unwrap();
        assert_eq!(
            holder_evidence(&conn, "env-a").unwrap(),
            HolderEvidence::NotApplicable
        );
        assert_eq!(
            bind_work_claim_exec_env(&mut conn, "claim-a", "env-a", 0).unwrap(),
            1
        );
        assert_eq!(
            holder_evidence(&conn, "env-a").unwrap(),
            HolderEvidence::Held
        );
        assert!(
            matches!(
                bind_work_claim_exec_env(&mut conn, "claim-a", "env-a", 0),
                Err(MemoryError::WorkClaimConflict(_))
            ),
            "stale bind version must not overwrite the first binding"
        );
        assert_eq!(
            release_work_claim(&mut conn, "claim-a", "agent-a", 1, "explicit").unwrap(),
            2
        );
        assert_eq!(
            holder_evidence(&conn, "env-a").unwrap(),
            HolderEvidence::Clear
        );
    }

    #[test]
    fn rejected_admission_keeps_connection_and_evidence_without_identity() {
        let conn = open_conn();
        record_rejected_admission(
            &conn,
            "admission-rejected-no-identity",
            "connection-untrusted",
            "stable identity receipt missing",
        )
        .unwrap();
        let row: (Option<String>, String, String, Option<String>) = conn
            .query_row(
                "SELECT agent_identity_id, connection_id, state, rejection_evidence \
                 FROM identity_admissions WHERE admission_id=?1",
                params!["admission-rejected-no-identity"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(row.0, None, "a rejected row must not fabricate identity");
        assert_eq!(row.1, "connection-untrusted");
        assert_eq!(row.2, "rejected");
        assert_eq!(row.3.as_deref(), Some("stable identity receipt missing"));
    }

    #[test]
    fn incompatible_or_missing_holder_links_refuse_with_distinct_evidence() {
        let mut conn = open_conn();
        crate::db::exec_env::insert_exec_env(
            &conn,
            &crate::db::exec_env::NewExecEnvLease {
                env_id: "env-b".into(),
                kind: "worktree".into(),
                path: "/wt/b".into(),
                repo_root: "/repo".into(),
                branch: String::new(),
                base_sha: String::new(),
                dispatch_id: None,
                env_class: crate::db::exec_env::EnvClass::EditOnly,
                created_at: String::new(),
            },
        )
        .unwrap();
        conn.execute("UPDATE exec_envs SET agent_identity_id='agent-a', claim_id='missing' WHERE env_id='env-b'", []).unwrap();
        assert_eq!(
            holder_evidence(&conn, "env-b").unwrap(),
            HolderEvidence::Unverifiable
        );
        conn.execute(
            "UPDATE exec_envs SET claim_id=NULL WHERE env_id='env-b'",
            [],
        )
        .unwrap();
        assert_eq!(
            holder_evidence(&conn, "env-b").unwrap(),
            HolderEvidence::Contradictory
        );
        conn.execute(
            "UPDATE exec_envs SET agent_identity_id=NULL, claim_id=NULL WHERE env_id='env-b'",
            [],
        )
        .unwrap();
        identity(&conn, "agent-b");
        insert_work_claim(&mut conn, &work_claim("claim-b", "agent-b")).unwrap();
        conn.execute(
            "UPDATE exec_envs SET agent_identity_id='agent-b', claim_id='claim-b' WHERE env_id='env-b'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE session_claims SET exec_env_id='env-b', state='bad' WHERE claim_id='claim-b'",
            [],
        )
        .unwrap();
        assert_eq!(
            holder_evidence(&conn, "env-b").unwrap(),
            HolderEvidence::Unavailable
        );
        assert_eq!(
            holder_evidence(&conn, "missing-env").unwrap(),
            HolderEvidence::Unverifiable
        );
    }

    #[test]
    fn reverse_only_holder_link_is_contradictory_not_legacy_not_applicable() {
        let mut conn = open_conn();
        identity(&conn, "agent-reverse");
        insert_work_claim(&mut conn, &work_claim("claim-reverse", "agent-reverse")).unwrap();
        crate::db::exec_env::insert_exec_env(
            &conn,
            &crate::db::exec_env::NewExecEnvLease {
                env_id: "env-reverse".into(),
                kind: "worktree".into(),
                path: "/wt/reverse".into(),
                repo_root: "/repo".into(),
                branch: "lane/1253".into(),
                base_sha: "3c09b425".into(),
                dispatch_id: None,
                env_class: crate::db::exec_env::EnvClass::EditOnly,
                created_at: String::new(),
            },
        )
        .unwrap();
        conn.execute(
            "UPDATE session_claims SET exec_env_id='env-reverse' WHERE claim_id='claim-reverse'",
            [],
        )
        .unwrap();

        assert_eq!(
            holder_evidence(&conn, "env-reverse").unwrap(),
            HolderEvidence::Contradictory
        );
    }

    #[test]
    fn forward_claim_with_different_reverse_claim_is_contradictory() {
        let mut conn = open_conn();
        identity(&conn, "agent-forward");
        identity(&conn, "agent-reverse-other");
        insert_work_claim(&mut conn, &work_claim("claim-forward", "agent-forward")).unwrap();
        let mut reverse_other = work_claim("claim-reverse-other", "agent-reverse-other");
        reverse_other.issue_ref = Some("org/repo#1253-other".into());
        reverse_other.worktree_path = "/wt/1253-other".into();
        reverse_other.declared_file_scope = "crates/memcore/src/db/other.rs".into();
        insert_work_claim(&mut conn, &reverse_other).unwrap();
        crate::db::exec_env::insert_exec_env(
            &conn,
            &crate::db::exec_env::NewExecEnvLease {
                env_id: "env-split".into(),
                kind: "worktree".into(),
                path: "/wt/split".into(),
                repo_root: "/repo".into(),
                branch: "lane/1253".into(),
                base_sha: "3c09b425".into(),
                dispatch_id: None,
                env_class: crate::db::exec_env::EnvClass::EditOnly,
                created_at: String::new(),
            },
        )
        .unwrap();
        conn.execute(
            "UPDATE exec_envs SET agent_identity_id='agent-forward', claim_id='claim-forward' WHERE env_id='env-split'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE session_claims SET exec_env_id='env-split' WHERE claim_id='claim-reverse-other'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE session_claims SET exec_env_id='env-split' WHERE claim_id='claim-forward'",
            [],
        )
        .unwrap();

        assert_eq!(
            holder_evidence(&conn, "env-split").unwrap(),
            HolderEvidence::Contradictory
        );
    }

    #[test]
    fn work_claim_collision_matrix_keeps_read_only_and_disjoint_writes_but_protects_orphans() {
        let mut conn = open_conn();
        for identity_id in [
            "agent-ro-a",
            "agent-ro-b",
            "agent-write-a",
            "agent-write-b",
            "agent-scope-conflict",
            "agent-path-conflict",
            "agent-head-conflict",
            "agent-orphan-a",
            "agent-orphan-b",
        ] {
            identity(&conn, identity_id);
        }

        let mut read_only_a = work_claim("ro-a", "agent-ro-a");
        read_only_a.mode = WorkClaimMode::ReadOnly;
        read_only_a.worktree_path = String::new();
        read_only_a.declared_file_scope = r#"["crates/one/**"]"#.into();
        insert_work_claim(&mut conn, &read_only_a).unwrap();

        let mut read_only_b = work_claim("ro-b", "agent-ro-b");
        read_only_b.mode = WorkClaimMode::ReadOnly;
        read_only_b.worktree_path = String::new();
        read_only_b.declared_file_scope = r#"["crates/one/**"]"#.into();
        insert_work_claim(&mut conn, &read_only_b).unwrap();

        let mut write_a = work_claim("write-a", "agent-write-a");
        write_a.worktree_path = "/worktrees/a".into();
        write_a.declared_file_scope = r#"["crates/a/**"]"#.into();
        insert_work_claim(&mut conn, &write_a).unwrap();

        let mut write_b = work_claim("write-b", "agent-write-b");
        write_b.worktree_path = "/worktrees/b".into();
        write_b.declared_file_scope = r#"["crates/b/**"]"#.into();
        insert_work_claim(&mut conn, &write_b).unwrap();

        let mut scope_conflict = work_claim("scope-conflict", "agent-scope-conflict");
        scope_conflict.worktree_path = "/worktrees/c".into();
        scope_conflict.declared_file_scope = r#"["crates/a/subtree/**"]"#.into();
        assert!(matches!(
            insert_work_claim(&mut conn, &scope_conflict),
            Err(MemoryError::WorkClaimConflict(_))
        ));

        let mut path_conflict = work_claim("path-conflict", "agent-path-conflict");
        path_conflict.worktree_path = "/worktrees/a/nested".into();
        path_conflict.declared_file_scope = r#"["crates/c/**"]"#.into();
        assert!(matches!(
            insert_work_claim(&mut conn, &path_conflict),
            Err(MemoryError::WorkClaimConflict(_))
        ));

        let mut head_conflict = work_claim("head-conflict", "agent-head-conflict");
        head_conflict.mode = WorkClaimMode::ReadOnly;
        head_conflict.worktree_path = String::new();
        head_conflict.expected_head = "different-head".into();
        assert!(matches!(
            insert_work_claim(&mut conn, &head_conflict),
            Err(MemoryError::WorkClaimConflict(_))
        ));

        let mut orphaned = work_claim("orphaned", "agent-orphan-a");
        orphaned.issue_ref = Some("org/repo#orphan".into());
        orphaned.worktree_path = "/worktrees/orphan".into();
        orphaned.declared_file_scope = r#"["crates/orphan/**"]"#.into();
        insert_work_claim(&mut conn, &orphaned).unwrap();
        conn.execute(
            "UPDATE session_claims SET state='orphaned' WHERE claim_id='orphaned'",
            [],
        )
        .unwrap();
        let mut takeover = work_claim("takeover", "agent-orphan-b");
        takeover.issue_ref = Some("org/repo#orphan".into());
        takeover.worktree_path = "/worktrees/orphan".into();
        takeover.declared_file_scope = r#"["crates/orphan/**"]"#.into();
        assert!(matches!(
            insert_work_claim(&mut conn, &takeover),
            Err(MemoryError::WorkClaimConflict(_))
        ));
    }

    #[test]
    fn work_claim_refuses_an_empty_serialized_scope() {
        let mut conn = open_conn();
        identity(&conn, "agent-empty-scope");
        let mut claim = work_claim("empty-scope", "agent-empty-scope");
        claim.declared_file_scope = "[]".into();
        assert!(matches!(
            insert_work_claim(&mut conn, &claim),
            Err(MemoryError::InvalidArg(_))
        ));
    }

    #[test]
    fn work_claim_transition_authorization_is_holder_enforced_and_typed() {
        let mut conn = open_conn();
        identity(&conn, "agent-holder");
        identity(&conn, "agent-intruder");

        let mut heartbeat = work_claim("auth-heartbeat", "agent-holder");
        heartbeat.issue_ref = Some("org/repo#auth-heartbeat".into());
        heartbeat.mode = WorkClaimMode::ReadOnly;
        heartbeat.worktree_path = String::new();
        insert_work_claim(&mut conn, &heartbeat).unwrap();
        let heartbeat_error = heartbeat_work_claim(
            &mut conn,
            "auth-heartbeat",
            "agent-intruder",
            0,
            "2030-01-02T00:00:00Z",
        )
        .expect_err("non-holder heartbeat must be refused");
        assert!(matches!(
            heartbeat_error,
            MemoryError::WorkClaimTransitionRefused {
                reason: WorkClaimTransitionReason::HolderMismatch,
                ..
            }
        ));

        let mut release = work_claim("auth-release", "agent-holder");
        release.issue_ref = Some("org/repo#auth-release".into());
        release.mode = WorkClaimMode::ReadOnly;
        release.worktree_path = String::new();
        insert_work_claim(&mut conn, &release).unwrap();
        let release_error =
            release_work_claim(&mut conn, "auth-release", "agent-intruder", 0, "intruder")
                .expect_err("non-holder release must be refused");
        assert!(matches!(
            release_error,
            MemoryError::WorkClaimTransitionRefused {
                reason: WorkClaimTransitionReason::HolderMismatch,
                ..
            }
        ));

        let mut handoff = work_claim("auth-handoff", "agent-holder");
        handoff.issue_ref = Some("org/repo#auth-handoff".into());
        handoff.mode = WorkClaimMode::ReadOnly;
        handoff.worktree_path = String::new();
        insert_work_claim(&mut conn, &handoff).unwrap();
        let successor = WorkClaimHandoffRequest {
            agent_identity_id: "agent-intruder".into(),
            role: "executor".into(),
            mode: WorkClaimMode::ReadOnly,
            worktree_path: String::new(),
            declared_file_scope: r#"["crates/auth/**"]"#.into(),
            expected_head: "3c09b425".into(),
            lease_expires_at: "2030-01-02T00:00:00Z".into(),
        };
        let handoff_error =
            handoff_work_claim(&mut conn, "auth-handoff", "agent-intruder", 0, &successor)
                .expect_err("non-holder active handoff must be refused");
        assert!(matches!(
            handoff_error,
            MemoryError::WorkClaimTransitionRefused {
                reason: WorkClaimTransitionReason::HolderMismatch,
                ..
            }
        ));
        let unchanged = get_claim(&conn, "auth-handoff").unwrap().unwrap();
        assert_eq!(unchanged.state, ClaimState::Active);
        assert_eq!(unchanged.transition_version, 0);
        assert_eq!(unchanged.agent_identity_id.as_deref(), Some("agent-holder"));

        conn.execute(
            "UPDATE session_claims SET state='orphaned' WHERE claim_id='auth-handoff'",
            [],
        )
        .unwrap();
        let recovered =
            handoff_work_claim(&mut conn, "auth-handoff", "agent-intruder", 0, &successor)
                .expect("an admitted successor may recover an orphaned claim");
        assert_eq!(recovered.from_agent_identity_id, "agent-holder");
        assert_eq!(recovered.to_agent_identity_id, "agent-intruder");
        assert_eq!(recovered.transition_version, 1);
    }

    #[test]
    fn work_claim_heartbeat_and_handoff_are_versioned_and_never_auto_take_over() {
        let mut conn = open_conn();
        for identity_id in [
            "agent-heartbeat",
            "agent-handoff-from",
            "agent-handoff-to",
            "agent-blocker",
        ] {
            identity(&conn, identity_id);
        }

        let mut heartbeat = work_claim("heartbeat", "agent-heartbeat");
        heartbeat.issue_ref = Some("org/repo#heartbeat".into());
        insert_work_claim(&mut conn, &heartbeat).unwrap();
        let receipt = heartbeat_work_claim(
            &mut conn,
            "heartbeat",
            "agent-heartbeat",
            0,
            "2026-07-20T00:00:00Z",
        )
        .unwrap();
        assert_eq!(receipt.transition_version, 1);
        assert_eq!(receipt.lease_expires_at, "2026-07-20T00:00:00Z");
        assert!(matches!(
            heartbeat_work_claim(
                &mut conn,
                "heartbeat",
                "agent-heartbeat",
                0,
                "2026-07-21T00:00:00Z"
            ),
            Err(MemoryError::WorkClaimConflict(_))
        ));
        release_work_claim(&mut conn, "heartbeat", "agent-heartbeat", 1, "done").unwrap();
        assert!(matches!(
            heartbeat_work_claim(
                &mut conn,
                "heartbeat",
                "agent-heartbeat",
                2,
                "2026-07-21T00:00:00Z"
            ),
            Err(MemoryError::WorkClaimIncompatibleState(_))
        ));
        assert!(matches!(
            heartbeat_work_claim(
                &mut conn,
                "missing",
                "agent-heartbeat",
                0,
                "2026-07-21T00:00:00Z"
            ),
            Err(MemoryError::NotFound(_))
        ));
        let mut orphaned_heartbeat = work_claim("orphaned-heartbeat", "agent-heartbeat");
        orphaned_heartbeat.issue_ref = Some("org/repo#orphaned-heartbeat".into());
        insert_work_claim(&mut conn, &orphaned_heartbeat).unwrap();
        conn.execute(
            "UPDATE session_claims SET state='orphaned' WHERE claim_id='orphaned-heartbeat'",
            [],
        )
        .unwrap();
        assert!(matches!(
            heartbeat_work_claim(
                &mut conn,
                "orphaned-heartbeat",
                "agent-heartbeat",
                0,
                "2026-07-21T00:00:00Z"
            ),
            Err(MemoryError::WorkClaimIncompatibleState(_))
        ));

        let mut handoff = work_claim("handoff", "agent-handoff-from");
        handoff.issue_ref = Some("org/repo#handoff".into());
        handoff.worktree_path = "/worktrees/handoff".into();
        handoff.declared_file_scope = r#"["crates/handoff/**"]"#.into();
        insert_work_claim(&mut conn, &handoff).unwrap();
        conn.execute(
            "UPDATE session_claims SET state='orphaned' WHERE claim_id='handoff'",
            [],
        )
        .unwrap();
        let successor = WorkClaimHandoffRequest {
            agent_identity_id: "agent-handoff-to".into(),
            role: "executor".into(),
            mode: WorkClaimMode::Writable,
            worktree_path: "/worktrees/handoff-new".into(),
            declared_file_scope: r#"["crates/handoff/**"]"#.into(),
            expected_head: "3c09b425".into(),
            lease_expires_at: "2026-07-20T00:00:00Z".into(),
        };
        assert!(matches!(
            handoff_work_claim(&mut conn, "handoff", "agent-handoff-to", 7, &successor),
            Err(MemoryError::WorkClaimConflict(_))
        ));
        let handoff_result =
            handoff_work_claim(&mut conn, "handoff", "agent-handoff-to", 0, &successor).unwrap();
        assert_eq!(handoff_result.from_agent_identity_id, "agent-handoff-from");
        assert_eq!(handoff_result.to_agent_identity_id, "agent-handoff-to");
        let handed_off = get_claim(&conn, "handoff").unwrap().unwrap();
        assert_eq!(handed_off.state, ClaimState::Active);
        assert_eq!(
            handed_off.agent_identity_id.as_deref(),
            Some("agent-handoff-to")
        );
        assert_eq!(handed_off.transition_version, 1);

        let mut blocker = work_claim("blocker", "agent-blocker");
        blocker.issue_ref = Some("org/repo#handoff-conflict".into());
        blocker.worktree_path = "/worktrees/blocker".into();
        blocker.declared_file_scope = r#"["crates/blocker/**"]"#.into();
        insert_work_claim(&mut conn, &blocker).unwrap();
        let mut source = work_claim("handoff-conflict", "agent-handoff-from");
        source.issue_ref = Some("org/repo#handoff-conflict".into());
        source.mode = WorkClaimMode::ReadOnly;
        source.worktree_path = String::new();
        source.declared_file_scope = r#"["crates/source/**"]"#.into();
        insert_work_claim(&mut conn, &source).unwrap();
        let conflicting_successor = WorkClaimHandoffRequest {
            agent_identity_id: "agent-handoff-from".into(),
            role: "executor".into(),
            mode: WorkClaimMode::Writable,
            worktree_path: "/worktrees/blocker/nested".into(),
            declared_file_scope: r#"["crates/other/**"]"#.into(),
            expected_head: "3c09b425".into(),
            lease_expires_at: "2026-07-20T00:00:00Z".into(),
        };
        assert!(matches!(
            handoff_work_claim(
                &mut conn,
                "handoff-conflict",
                "agent-handoff-from",
                0,
                &conflicting_successor
            ),
            Err(MemoryError::WorkClaimConflict(_))
        ));
        let unchanged = get_claim(&conn, "handoff-conflict").unwrap().unwrap();
        assert_eq!(
            unchanged.agent_identity_id.as_deref(),
            Some("agent-handoff-from")
        );
        assert_eq!(unchanged.transition_version, 0);
    }

    #[test]
    fn handoff_refuses_an_empty_serialized_scope() {
        let mut conn = open_conn();
        identity(&conn, "agent-empty-handoff-from");
        identity(&conn, "agent-empty-handoff-to");
        let mut claim = work_claim("empty-handoff", "agent-empty-handoff-from");
        claim.issue_ref = Some("org/repo#empty-handoff".into());
        insert_work_claim(&mut conn, &claim).unwrap();

        let successor = WorkClaimHandoffRequest {
            agent_identity_id: "agent-empty-handoff-to".into(),
            role: "executor".into(),
            mode: WorkClaimMode::Writable,
            worktree_path: "/worktrees/empty-handoff".into(),
            declared_file_scope: "[]".into(),
            expected_head: "3c09b425".into(),
            lease_expires_at: "2026-07-20T00:00:00Z".into(),
        };
        assert!(matches!(
            handoff_work_claim(
                &mut conn,
                "empty-handoff",
                "agent-empty-handoff-to",
                0,
                &successor
            ),
            Err(MemoryError::InvalidArg(_))
        ));
    }
}

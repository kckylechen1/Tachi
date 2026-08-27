//! Execution-environment leases (#894 S1) — daemon-owned `exec_envs` rows.
//!
//! An `ExecEnvLease` is the single source of truth for one provisioned
//! execution environment (today: a managed git worktree). Worktree markers and
//! the global `worktrees.json` registry are read-only projections/backstops for
//! offline tools (the sweep); they never own lease state.
//!
//! ## State machine (S1)
//!
//! ```text
//!   provisioning ──complete resource ledger──▶ active
//!                                                ├──dispatch──▶ dispatching
//!                                                │                 │
//!                                                ◀──── terminal ───┘
//!                                                ├──removal──▶ removing
//!                                                │               ├──success──▶ reclaimed
//!                                                ◀────failure─────┘
//!                                                └──reclaim──▶ reclaimed
//!
//! A crash while `provisioning` or `dispatching` stays fail-closed. Reclaim
//! refuses either state; reconciliation must establish a complete ledger or a
//! clean/fenced terminal outcome.
//! ```
//!
//! Normal terminal actions route through [`reclaim_exec_env`]. A destructive
//! cleaner instead owns the explicit [`claim_exec_env_removal`] →
//! [`complete_exec_env_removal`] protocol so filesystem deletion and dispatch
//! admission cannot race.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::{Component, Path, PathBuf};

use crate::error::MemoryError;

use super::common::normalize_utc_iso_or_now;

fn lexically_normalized(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn path_is_within(candidate: &str, root: &str) -> bool {
    lexically_normalized(Path::new(candidate)).starts_with(lexically_normalized(Path::new(root)))
}

/// Lease lifecycle state. `Dispatching` is the exclusive Required-postflight
/// admission state; it prevents two workers from sharing one preimage/lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecEnvState {
    /// Lease identity is reserved while its complete resource ledger is being
    /// published. Dispatch and destructive cleanup both reject this state.
    Provisioning,
    /// Provisioned and in use — the worktree exists and the lease owns it.
    Active,
    /// Exclusively admitted to one in-flight dispatch. A daemon crash leaves
    /// this state fail-closed until an operator reconciles the lease.
    Dispatching,
    /// Exclusively claimed by a destructive cleaner. Dispatch admission
    /// refuses this state; a crash remains fail-closed until reconciliation.
    Removing,
    /// Reclaimed — the worktree/branch/target have been (or are being) torn
    /// down; the row is retained for audit and idempotent reclaim.
    Reclaimed,
}

impl ExecEnvState {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecEnvState::Provisioning => "provisioning",
            ExecEnvState::Active => "active",
            ExecEnvState::Dispatching => "dispatching",
            ExecEnvState::Removing => "removing",
            ExecEnvState::Reclaimed => "reclaimed",
        }
    }

    /// Parse a persisted state string. An unknown/legacy value is an error the
    /// caller must surface (fail-closed): a corrupted state must never silently
    /// masquerade as `active` (usable) or `reclaimed` (torn down).
    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "provisioning" => Ok(ExecEnvState::Provisioning),
            "active" => Ok(ExecEnvState::Active),
            "dispatching" => Ok(ExecEnvState::Dispatching),
            "removing" => Ok(ExecEnvState::Removing),
            "reclaimed" => Ok(ExecEnvState::Reclaimed),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown exec_env state '{other}' (expected 'provisioning', 'active', 'dispatching', 'removing', or 'reclaimed')"
            ))),
        }
    }
}

/// Provisioning policy class for a lease (#894 S2c). Closed vocabulary.
///
/// ## What this is, and what it is NOT (owner-ratified 2026-07-13)
///
/// It is **not a security boundary**. A worker with a shell and the same UID
/// can run `cargo` inside an `EditOnly` tree no matter what this enum says —
/// PATH/tool-table fences are bypassable by anyone who can spawn a process.
/// Do not build an authority decision on it.
///
/// What it *is*:
/// - **disk**: `EditOnly` allocates no `build_target` resource, so the tree
///   stays ~14MB instead of dragging a multi-GB target dir behind it;
/// - **default routing**: builds are meant to go to the broker's
///   machine-unique serialized executor seat instead of N diverged worktrees
///   all hammering one shared `CARGO_TARGET_DIR` — which is what produced
///   phantom "symbol not found" compile errors on this machine twice in one
///   night (2026-07-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnvClass {
    /// Default. No build target allocated; builds belong in the broker.
    #[default]
    EditOnly,
    /// Normal verification path: this lease submits immutable build tickets to
    /// the serialized executor seat. It owns **no** build target of its own —
    /// and does not hold one of the seat's either. Which target dir a ticket
    /// lands on (the seat's resident target, or its fork scratch target) is a
    /// per-ticket decision the broker makes at run time; a lease-time binding
    /// would be booking a resource the build may never touch.
    BuildTicketed,
    /// Rare: an explicitly approved private target dir with a disk
    /// reservation. Provisioning refuses this class without an approval.
    BuildPrivate,
}

impl EnvClass {
    pub fn as_str(self) -> &'static str {
        match self {
            EnvClass::EditOnly => "edit-only",
            EnvClass::BuildTicketed => "build-ticketed",
            EnvClass::BuildPrivate => "build-private",
        }
    }

    /// Parse a persisted class. Unknown values are an error (fail-closed): a
    /// corrupted class must never silently masquerade as one that allocates
    /// disk, nor as one that doesn't.
    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "edit-only" => Ok(EnvClass::EditOnly),
            "build-ticketed" => Ok(EnvClass::BuildTicketed),
            "build-private" => Ok(EnvClass::BuildPrivate),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown exec_env class '{other}' (expected one of \
                 edit-only/build-ticketed/build-private)"
            ))),
        }
    }

    /// Whether provisioning allocates a `build_target` resource **to this
    /// lease**. `BuildPrivate` is the only class that does.
    ///
    /// `BuildTicketed` is deliberately `false` (#894 S2c round-2). It looks like
    /// it should be `true` — the lease does cause builds — but the target those
    /// builds run in belongs to the *executor seat*, not to the lease:
    ///
    /// - the seat picks resident-vs-scratch per ticket, at run time, from the
    ///   ticket's lineage; provisioning cannot know which dir a future ticket
    ///   will land on, so any lease-time binding is a guess;
    /// - the broker already books the target it actually touches (registering
    ///   the row, recording it on the executor slot, stamping its generation),
    ///   so a second, lease-side booking is a duplicate claim on a dir that
    ///   nothing in the broker consults.
    ///
    /// The pre-round-2 code bound ticketed leases to a target dir resolved from
    /// `default_shared_cargo_target_dir()` — a path the broker never builds in.
    /// The ledger said the lease held a target; the seat used a different one.
    pub fn allocates_build_target(self) -> bool {
        matches!(self, EnvClass::BuildPrivate)
    }

    /// Whether provisioning requires an explicit approval + disk reservation.
    pub fn requires_approval(self) -> bool {
        matches!(self, EnvClass::BuildPrivate)
    }

    /// Whether the build target this class binds is the *private* one (its own
    /// dir) rather than the executor seat's shared resident target.
    pub fn wants_private_target(self) -> bool {
        matches!(self, EnvClass::BuildPrivate)
    }
}

/// A row in `exec_envs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEnvLease {
    pub env_id: String,
    pub kind: String,
    pub path: String,
    pub repo_root: String,
    pub branch: String,
    pub base_sha: String,
    pub dispatch_id: Option<String>,
    /// Opaque v21 holder evidence; ExecEnv never owns identity transitions.
    pub agent_identity_id: Option<String>,
    /// Opaque v21 WorkClaim link; only `bind_work_claim_exec_env` writes it.
    pub claim_id: Option<String>,
    pub env_class: EnvClass,
    pub state: ExecEnvState,
    pub reclaim_reason: Option<String>,
    pub schema_version: i64,
    pub created_at: String,
    pub reclaimed_at: Option<String>,
}

/// Fields required to insert a new lease. `created_at` defaults to now when
/// empty; `env_id` must be caller-supplied and unique.
#[derive(Debug, Clone)]
pub struct NewExecEnvLease {
    pub env_id: String,
    pub kind: String,
    pub path: String,
    pub repo_root: String,
    pub branch: String,
    pub base_sha: String,
    pub dispatch_id: Option<String>,
    pub env_class: EnvClass,
    pub created_at: String,
}

/// How a reclaim selects its target lease.
#[derive(Debug, Clone)]
pub enum ExecEnvSelector {
    /// By primary key.
    EnvId(String),
    /// By workspace path (matches the *active* lease for that path). Used by
    /// the merge/reclaim call sites that only know a worktree path.
    Path(String),
}

/// Result of a reclaim attempt through the single reclaim path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReclaimOutcome {
    /// A lease was flipped `active` -> `reclaimed` on this call.
    Reclaimed { env_id: String },
    /// The lease was already `reclaimed`; no state changed (idempotent).
    AlreadyReclaimed { env_id: String },
    /// No lease matched the selector.
    NotFound,
}

const SELECT_COLUMNS: &str =
    "env_id, kind, path, repo_root, branch, base_sha, dispatch_id, agent_identity_id, claim_id, \
     env_class, state, reclaim_reason, schema_version, created_at, reclaimed_at";

fn conv_err(idx: usize, e: MemoryError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        idx,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        )),
    )
}

fn row_to_lease(row: &rusqlite::Row<'_>) -> Result<ExecEnvLease, rusqlite::Error> {
    let class_raw: String = row.get(9)?;
    let env_class = EnvClass::parse(&class_raw).map_err(|e| conv_err(9, e))?;
    let state_raw: String = row.get(10)?;
    let state = ExecEnvState::parse(&state_raw).map_err(|e| conv_err(10, e))?;
    Ok(ExecEnvLease {
        env_id: row.get(0)?,
        kind: row.get(1)?,
        path: row.get(2)?,
        repo_root: row.get(3)?,
        branch: row.get(4)?,
        base_sha: row.get(5)?,
        dispatch_id: row.get(6)?,
        agent_identity_id: row.get(7)?,
        claim_id: row.get(8)?,
        env_class,
        state,
        reclaim_reason: row.get(11)?,
        schema_version: row.get(12)?,
        created_at: row.get(13)?,
        reclaimed_at: row.get(14)?,
    })
}

/// Insert a new lease. Fails if `env_id` already exists — a lease id collision
/// is a bug, never a silent overwrite that could orphan the prior worktree.
pub fn insert_exec_env(conn: &Connection, lease: &NewExecEnvLease) -> Result<(), MemoryError> {
    insert_exec_env_in_state(conn, lease, ExecEnvState::Active)
}

/// Reserve a lease identity while its resource ledger is still being built.
pub fn insert_provisioning_exec_env(
    conn: &Connection,
    lease: &NewExecEnvLease,
) -> Result<(), MemoryError> {
    insert_exec_env_in_state(conn, lease, ExecEnvState::Provisioning)
}

fn insert_exec_env_in_state(
    conn: &Connection,
    lease: &NewExecEnvLease,
    state: ExecEnvState,
) -> Result<(), MemoryError> {
    let created_at = if lease.created_at.trim().is_empty() {
        normalize_utc_iso_or_now("")
    } else {
        normalize_utc_iso_or_now(&lease.created_at)
    };
    conn.execute(
        "INSERT INTO exec_envs
         (env_id, kind, path, repo_root, branch, base_sha, dispatch_id, agent_identity_id, claim_id,
          env_class, state, reclaim_reason, schema_version, created_at, reclaimed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9, NULL, 1, ?10, NULL)",
        params![
            lease.env_id,
            lease.kind,
            lease.path,
            lease.repo_root,
            lease.branch,
            lease.base_sha,
            lease.dispatch_id,
            lease.env_class.as_str(),
            state.as_str(),
            created_at,
        ],
    )?;
    Ok(())
}

/// Fetch a lease by id.
pub fn get_exec_env(conn: &Connection, env_id: &str) -> Result<Option<ExecEnvLease>, MemoryError> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM exec_envs WHERE env_id = ?1");
    let lease = conn
        .query_row(&sql, params![env_id], row_to_lease)
        .optional()?;
    Ok(lease)
}

/// Fetch the single *active* lease for a workspace path, if any. Reclaimed rows
/// for the same path are ignored so a re-provisioned path resolves to the live
/// lease.
pub fn find_active_exec_env_by_path(
    conn: &Connection,
    path: &str,
) -> Result<Option<ExecEnvLease>, MemoryError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM exec_envs \
         WHERE path = ?1 AND state = 'active' \
         ORDER BY created_at DESC LIMIT 1"
    );
    let lease = conn
        .query_row(&sql, params![path], row_to_lease)
        .optional()?;
    Ok(lease)
}

/// Fetch the newest unreclaimed lease for a workspace path. Destructive
/// consumers use this broader lookup so fail-closed exclusive states cannot
/// disappear behind an active-only query.
pub fn find_live_exec_env_by_path(
    conn: &Connection,
    path: &str,
) -> Result<Option<ExecEnvLease>, MemoryError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM exec_envs \
         WHERE path = ?1 AND state IN ('provisioning', 'active', 'dispatching', 'removing') \
         ORDER BY created_at DESC LIMIT 1"
    );
    let lease = conn
        .query_row(&sql, params![path], row_to_lease)
        .optional()?;
    Ok(lease)
}

/// List leases, optionally filtered by state. Newest first. Used by status
/// surfaces and the sweep backstop (read-only).
pub fn list_exec_envs(
    conn: &Connection,
    state: Option<ExecEnvState>,
) -> Result<Vec<ExecEnvLease>, MemoryError> {
    let mut out = Vec::new();
    match state {
        Some(state) => {
            let sql = format!(
                "SELECT {SELECT_COLUMNS} FROM exec_envs WHERE state = ?1 ORDER BY created_at DESC"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![state.as_str()], row_to_lease)?;
            for row in rows {
                out.push(row?);
            }
        }
        None => {
            let sql = format!("SELECT {SELECT_COLUMNS} FROM exec_envs ORDER BY created_at DESC");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], row_to_lease)?;
            for row in rows {
                out.push(row?);
            }
        }
    }
    Ok(out)
}

/// Atomically claim an active managed worktree for destructive removal.
/// `None` means no unreclaimed managed lease exists (legacy/not-applicable).
/// Holder and resource proofs are re-read under the same IMMEDIATE write
/// transaction as `active -> removing`, making it mutually exclusive with
/// dispatch admission's guarded `active -> dispatching` update.
pub fn claim_exec_env_removal(
    conn: &mut Connection,
    path: &str,
) -> Result<Option<String>, MemoryError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT env_id, state FROM exec_envs \
             WHERE path = ?1 AND state != 'reclaimed' \
             ORDER BY created_at DESC LIMIT 1",
            params![path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((env_id, state_raw)) = existing else {
        tx.commit()?;
        return Ok(None);
    };
    match ExecEnvState::parse(&state_raw)? {
        ExecEnvState::Active => {}
        state => {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "refusing to remove exec env {env_id}: lease is {}",
                state.as_str()
            )))
        }
    }
    match super::session_claims::holder_evidence(&tx, &env_id)? {
        super::session_claims::HolderEvidence::Clear
        | super::session_claims::HolderEvidence::NotApplicable => {}
        evidence => {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "refusing to remove exec env {env_id}: holder evidence is {evidence:?}"
            )))
        }
    }
    if let Some(detail) =
        super::exec_env_resources::exec_env_resource_removal_refusal(&tx, &env_id, path)?
    {
        return Err(MemoryError::WorkClaimIncompatibleState(detail));
    }
    let (resource_id, total_live_bindings): (String, i64) = tx.query_row(
        "SELECT r.resource_id, \
                (SELECT COUNT(*) FROM exec_env_resource_bindings all_b \
                 WHERE all_b.resource_id = r.resource_id AND all_b.released_at IS NULL) \
         FROM exec_env_resource_bindings b \
         JOIN exec_env_resources r ON r.resource_id = b.resource_id \
         WHERE b.env_id = ?1 AND b.released_at IS NULL \
           AND r.kind = 'worktree' AND r.path = ?2 AND r.state = 'active'",
        params![env_id, path],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if total_live_bindings != 1 {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "refusing to remove exec env {env_id}: worktree resource {resource_id} has {total_live_bindings} live bindings"
        )));
    }
    let resource_changed = tx.execute(
        "UPDATE exec_env_resources SET state = 'reclaiming', \
             reclaim_reason = 'worktree removal claim', updated_at = ?2 \
         WHERE resource_id = ?1 AND state = 'active'",
        params![resource_id, normalize_utc_iso_or_now("")],
    )?;
    if resource_changed != 1 {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "worktree resource {resource_id} changed during removal admission"
        )));
    }
    let mut stmt = tx.prepare(
        "SELECT r.resource_id, r.path, r.state, \
                (SELECT COUNT(*) FROM exec_env_resource_bindings all_b \
                 WHERE all_b.resource_id = r.resource_id AND all_b.released_at IS NULL) \
         FROM exec_env_resource_bindings b \
         JOIN exec_env_resources r ON r.resource_id = b.resource_id \
         WHERE b.env_id = ?1 AND b.released_at IS NULL AND r.kind = 'build_target'",
    )?;
    let build_targets: Vec<(String, String, String, i64)> = stmt
        .query_map(params![env_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (target_id, target_path, target_state, target_bindings) in build_targets {
        if !path_is_within(&target_path, path) {
            continue;
        }
        if target_state != "active" || target_bindings != 1 {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "refusing to remove exec env {env_id}: in-worktree build target {target_id} is {target_state} with {target_bindings} live bindings"
            )));
        }
        let changed = tx.execute(
            "UPDATE exec_env_resources SET state = 'reclaiming', \
                 reclaim_reason = 'worktree removal claim', updated_at = ?2 \
             WHERE resource_id = ?1 AND state = 'active'",
            params![target_id, normalize_utc_iso_or_now("")],
        )?;
        if changed != 1 {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "in-worktree build target {target_id} changed during removal admission"
            )));
        }
    }
    let changed = tx.execute(
        "UPDATE exec_envs SET state = 'removing' WHERE env_id = ?1 AND state = 'active'",
        params![env_id],
    )?;
    if changed != 1 {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "exec env {env_id} changed during removal admission"
        )));
    }
    tx.commit()?;
    Ok(Some(env_id))
}

/// Complete a persisted removal claim after filesystem deletion succeeds.
pub fn complete_exec_env_removal(
    conn: &mut Connection,
    env_id: &str,
    reason: Option<&str>,
    reclaimed_bytes: i64,
) -> Result<(), MemoryError> {
    if reclaimed_bytes < 0 {
        return Err(MemoryError::InvalidArg(
            "reclaimed worktree bytes must be non-negative".to_string(),
        ));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = normalize_utc_iso_or_now("");
    let (lease_path, lease_state): (String, String) = tx.query_row(
        "SELECT path, state FROM exec_envs WHERE env_id = ?1",
        params![env_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if lease_state != "removing" {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "exec env {env_id} lost its removal claim before completion"
        )));
    }
    let mut stmt = tx.prepare(
        "SELECT r.resource_id, r.kind, r.path, r.state \
         FROM exec_env_resource_bindings b \
         JOIN exec_env_resources r ON r.resource_id = b.resource_id \
         WHERE b.env_id = ?1 AND b.released_at IS NULL ORDER BY r.resource_id",
    )?;
    let resources: Vec<(String, String, String, String)> = stmt
        .query_map(params![env_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    if resources.is_empty() {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "exec env {env_id} has no live resource bindings at removal completion"
        )));
    }
    for (resource_id, kind, resource_path, state) in resources {
        let removed_with_worktree = kind == "worktree"
            || (kind == "build_target" && path_is_within(&resource_path, &lease_path));
        if removed_with_worktree && state != "reclaiming" {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "removed resource {resource_id} lost its removal claim before completion"
            )));
        }
        let released = tx.execute(
            "UPDATE exec_env_resource_bindings SET released_at = ?3 \
             WHERE env_id = ?1 AND resource_id = ?2 AND released_at IS NULL",
            params![env_id, resource_id, now],
        )?;
        if released != 1 {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "exec env {env_id} lost resource binding {resource_id} before removal completion"
            )));
        }
        if removed_with_worktree {
            let bytes = if kind == "worktree" {
                reclaimed_bytes
            } else {
                0
            };
            let changed = tx.execute(
                "UPDATE exec_env_resources SET state = 'reclaimed', reclaimed_at = ?2, \
                     reclaimed_bytes = ?3, updated_at = ?2 \
                 WHERE resource_id = ?1 AND state = 'reclaiming'",
                params![resource_id, now, bytes],
            )?;
            if changed != 1 {
                return Err(MemoryError::WorkClaimIncompatibleState(format!(
                    "removed resource {resource_id} lost its removal claim before completion"
                )));
            }
        }
    }
    let lease_changed = tx.execute(
        "UPDATE exec_envs SET state = 'reclaimed', reclaimed_at = ?2, \
             reclaim_reason = ?3 WHERE env_id = ?1 AND state = 'removing'",
        params![env_id, now, reason],
    )?;
    if lease_changed != 1 {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "exec env {env_id} lost its removal claim before completion"
        )));
    }
    tx.commit()?;
    Ok(())
}

/// Release a persisted removal claim after deletion did not occur.
pub fn abort_exec_env_removal(conn: &mut Connection, env_id: &str) -> Result<(), MemoryError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let resource_changed = tx.execute(
        "UPDATE exec_env_resources SET state = 'active', reclaim_reason = NULL, updated_at = ?2 \
         WHERE resource_id IN (SELECT resource_id FROM exec_env_resource_bindings \
                               WHERE env_id = ?1 AND released_at IS NULL) \
           AND state = 'reclaiming'",
        params![env_id, normalize_utc_iso_or_now("")],
    )?;
    if resource_changed == 0 {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "exec env {env_id} lost all resource removal claims before abort"
        )));
    }
    let lease_changed = tx.execute(
        "UPDATE exec_envs SET state = 'active' WHERE env_id = ?1 AND state = 'removing'",
        params![env_id],
    )?;
    if lease_changed != 1 {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "exec env {env_id} lost its removal claim before abort"
        )));
    }
    tx.commit()?;
    Ok(())
}

/// The ordinary reclaim path (#894 S1): transactionally flip an `active` lease to
/// `reclaimed`, stamping `reclaimed_at` and an optional reason. Idempotent — a
/// lease that is already `reclaimed` returns [`ReclaimOutcome::AlreadyReclaimed`]
/// without a second write. A missing lease is a typed [`MemoryError::NotFound`]
/// rather than a successful-looking outcome. safe_merge / cancel / terminal-state must all
/// call through here. Destructive cleaners use the separate persisted removal
/// claim protocol because they must own the lease before touching the filesystem.
pub fn reclaim_exec_env(
    conn: &mut Connection,
    selector: &ExecEnvSelector,
    reason: Option<&str>,
) -> Result<ReclaimOutcome, MemoryError> {
    let tx = conn.transaction()?;
    // Resolve existence before holder evidence: a missing ledger row is not
    // unverifiable holder evidence, it is a caller-visible missing target.
    let existing: Option<(String, String)> = match selector {
        ExecEnvSelector::EnvId(env_id) => tx
            .query_row(
                "SELECT env_id, state FROM exec_envs WHERE env_id = ?1",
                params![env_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?,
        ExecEnvSelector::Path(path) => tx
            .query_row(
                "SELECT env_id, state FROM exec_envs WHERE path = ?1 \
                 ORDER BY CASE state WHEN 'active' THEN 0 ELSE 1 END, created_at DESC LIMIT 1",
                params![path],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?,
    };
    let Some((env_id, state_raw)) = existing else {
        return Err(MemoryError::NotFound(match selector {
            ExecEnvSelector::EnvId(env_id) => format!("exec env {env_id}"),
            ExecEnvSelector::Path(path) => format!("exec env at {path}"),
        }));
    };
    // Holder evidence is read in the same transaction as the destructive
    // state transition. A held, contradictory, unavailable, or unverifiable
    // ledger must refuse loudly; it must never look like a harmless no-op.
    match super::session_claims::holder_evidence(&tx, &env_id)? {
        super::session_claims::HolderEvidence::Clear
        | super::session_claims::HolderEvidence::NotApplicable => {}
        evidence => {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "refusing to reclaim exec env {env_id}: holder evidence is {evidence:?}"
            )));
        }
    }
    let outcome = match ExecEnvState::parse(&state_raw)? {
        ExecEnvState::Reclaimed => ReclaimOutcome::AlreadyReclaimed { env_id },
        ExecEnvState::Provisioning => {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "refusing to reclaim exec env {env_id}: provisioning has not published its resource ledger"
            )))
        }
        ExecEnvState::Dispatching => {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "refusing to reclaim exec env {env_id}: an admitted dispatch still owns it"
            )))
        }
        ExecEnvState::Removing => {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "refusing to reclaim exec env {env_id}: a destructive cleaner owns it"
            )))
        }
        ExecEnvState::Active => {
            let now = normalize_utc_iso_or_now("");
            tx.execute(
                "UPDATE exec_envs SET state = 'reclaimed', reclaimed_at = ?2, \
                     reclaim_reason = ?3 WHERE env_id = ?1 AND state = 'active'",
                params![env_id, now, reason],
            )?;
            ReclaimOutcome::Reclaimed { env_id }
        }
    };
    tx.commit()?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_conn() -> Connection {
        // Keep this raw-connection fixture equivalent to MemoryStore's open
        // path: schema initialization creates FTS tables that require the
        // registered simple tokenizer.
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn open_file_conn(path: &std::path::Path) -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open(path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn new_lease(env_id: &str, path: &str) -> NewExecEnvLease {
        NewExecEnvLease {
            env_id: env_id.to_string(),
            kind: "worktree".to_string(),
            path: path.to_string(),
            repo_root: "/repo".to_string(),
            branch: "tachi/894/w".to_string(),
            base_sha: "abc123".to_string(),
            dispatch_id: Some("dispatch-1".to_string()),
            env_class: EnvClass::EditOnly,
            created_at: String::new(),
        }
    }

    #[test]
    fn insert_and_get_roundtrip_defaults_to_active() {
        let conn = open_conn();
        insert_exec_env(&conn, &new_lease("env-1", "/wt/1")).unwrap();
        let got = get_exec_env(&conn, "env-1")
            .unwrap()
            .expect("lease present");
        assert_eq!(got.env_id, "env-1");
        assert_eq!(got.state, ExecEnvState::Active);
        assert_eq!(got.path, "/wt/1");
        assert_eq!(got.base_sha, "abc123");
        assert_eq!(got.dispatch_id.as_deref(), Some("dispatch-1"));
        assert!(got.reclaimed_at.is_none());
        assert!(!got.created_at.is_empty(), "created_at defaults to now");
    }

    #[test]
    fn duplicate_env_id_is_rejected_not_silently_overwritten() {
        let conn = open_conn();
        insert_exec_env(&conn, &new_lease("env-dup", "/wt/a")).unwrap();
        // A colliding id must error, never clobber the prior worktree's lease.
        let err = insert_exec_env(&conn, &new_lease("env-dup", "/wt/b"));
        assert!(err.is_err(), "duplicate env_id must be rejected");
        // Original row is untouched.
        let got = get_exec_env(&conn, "env-dup").unwrap().unwrap();
        assert_eq!(got.path, "/wt/a");
    }

    #[test]
    fn reclaim_flips_active_to_reclaimed_and_stamps() {
        let mut conn = open_conn();
        insert_exec_env(&conn, &new_lease("env-2", "/wt/2")).unwrap();
        let outcome = reclaim_exec_env(
            &mut conn,
            &ExecEnvSelector::EnvId("env-2".to_string()),
            Some("safe_merge"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReclaimOutcome::Reclaimed {
                env_id: "env-2".to_string()
            }
        );
        let got = get_exec_env(&conn, "env-2").unwrap().unwrap();
        assert_eq!(got.state, ExecEnvState::Reclaimed);
        assert_eq!(got.reclaim_reason.as_deref(), Some("safe_merge"));
        assert!(got.reclaimed_at.is_some(), "reclaimed_at stamped");
    }

    #[test]
    fn reclaim_refuses_a_held_work_claim_instead_of_reporting_success() {
        let mut conn = open_conn();
        insert_exec_env(&conn, &new_lease("env-held", "/wt/held")).unwrap();
        conn.execute(
            "INSERT INTO session_claims (claim_id, branch, state, created_at, heartbeat_at, \
             agent_identity_id, exec_env_id) VALUES ('claim-held', '', 'active', '', '', 'agent-held', 'env-held')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE exec_envs SET agent_identity_id='agent-held', claim_id='claim-held' WHERE env_id='env-held'",
            [],
        )
        .unwrap();

        let err = reclaim_exec_env(
            &mut conn,
            &ExecEnvSelector::EnvId("env-held".to_string()),
            Some("cleanup"),
        )
        .unwrap_err();
        assert!(matches!(err, MemoryError::WorkClaimIncompatibleState(_)));
        assert_eq!(
            get_exec_env(&conn, "env-held").unwrap().unwrap().state,
            ExecEnvState::Active
        );
    }

    #[test]
    fn reclaim_is_idempotent_no_second_write() {
        let mut conn = open_conn();
        insert_exec_env(&conn, &new_lease("env-3", "/wt/3")).unwrap();
        let first = reclaim_exec_env(
            &mut conn,
            &ExecEnvSelector::EnvId("env-3".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(
            first,
            ReclaimOutcome::Reclaimed {
                env_id: "env-3".to_string()
            }
        );
        let stamp_after_first = get_exec_env(&conn, "env-3").unwrap().unwrap().reclaimed_at;

        let second = reclaim_exec_env(
            &mut conn,
            &ExecEnvSelector::EnvId("env-3".to_string()),
            Some("second-reason"),
        )
        .unwrap();
        assert_eq!(
            second,
            ReclaimOutcome::AlreadyReclaimed {
                env_id: "env-3".to_string()
            }
        );
        // The idempotent no-op must NOT overwrite the original stamp/reason.
        let after = get_exec_env(&conn, "env-3").unwrap().unwrap();
        assert_eq!(
            after.reclaimed_at, stamp_after_first,
            "stamp unchanged on no-op"
        );
        assert!(
            after.reclaim_reason.as_deref() != Some("second-reason"),
            "reason must not be overwritten by an idempotent reclaim"
        );
    }

    #[test]
    fn reclaim_missing_lease_reports_not_found() {
        let mut conn = open_conn();
        let err = reclaim_exec_env(&mut conn, &ExecEnvSelector::EnvId("nope".to_string()), None)
            .unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(message) if message == "exec env nope"));
    }

    #[test]
    fn find_active_by_path_ignores_reclaimed_rows() {
        let mut conn = open_conn();
        insert_exec_env(&conn, &new_lease("env-4", "/wt/shared")).unwrap();
        reclaim_exec_env(
            &mut conn,
            &ExecEnvSelector::EnvId("env-4".to_string()),
            None,
        )
        .unwrap();
        // No active lease for that path now.
        assert!(find_active_exec_env_by_path(&conn, "/wt/shared")
            .unwrap()
            .is_none());
        // Re-provisioning the same path resolves to the fresh active lease.
        insert_exec_env(&conn, &new_lease("env-5", "/wt/shared")).unwrap();
        let active = find_active_exec_env_by_path(&conn, "/wt/shared")
            .unwrap()
            .unwrap();
        assert_eq!(active.env_id, "env-5");
    }

    #[test]
    fn find_live_by_path_includes_dispatching_and_ignores_reclaimed_rows() {
        let conn = open_conn();
        insert_exec_env(&conn, &new_lease("env-live", "/wt/live")).unwrap();
        conn.execute(
            "UPDATE exec_envs SET state='dispatching' WHERE env_id='env-live'",
            [],
        )
        .unwrap();
        let live = find_live_exec_env_by_path(&conn, "/wt/live")
            .unwrap()
            .expect("dispatching lease remains visible to destructive consumers");
        assert_eq!(live.state, ExecEnvState::Dispatching);

        conn.execute(
            "UPDATE exec_envs SET state='reclaimed' WHERE env_id='env-live'",
            [],
        )
        .unwrap();
        assert!(find_live_exec_env_by_path(&conn, "/wt/live")
            .unwrap()
            .is_none());
    }

    #[test]
    fn removal_claim_and_dispatch_admission_are_mutually_exclusive() {
        let mut conn = open_conn();
        insert_exec_env(&conn, &new_lease("env-remove", "/wt/remove")).unwrap();
        super::super::exec_env_resources::insert_resource(
            &mut conn,
            &super::super::exec_env_resources::NewExecEnvResource {
                resource_id: "res-remove".to_string(),
                kind: super::super::exec_env_resources::ResourceKind::Worktree,
                path: "/wt/remove".to_string(),
                bytes: None,
                created_at: String::new(),
            },
        )
        .unwrap();
        super::super::exec_env_resources::bind_resource(&mut conn, "env-remove", "res-remove")
            .unwrap();

        assert_eq!(
            claim_exec_env_removal(&mut conn, "/wt/remove").unwrap(),
            Some("env-remove".to_string())
        );
        assert_eq!(
            get_exec_env(&conn, "env-remove").unwrap().unwrap().state,
            ExecEnvState::Removing
        );
        let dispatch_changed = conn
            .execute(
                "UPDATE exec_envs SET state='dispatching' WHERE env_id='env-remove' AND state='active'",
                [],
            )
            .unwrap();
        assert_eq!(dispatch_changed, 0, "removal ownership must fence dispatch");

        abort_exec_env_removal(&mut conn, "env-remove").unwrap();
        conn.execute(
            "UPDATE exec_envs SET state='dispatching' WHERE env_id='env-remove' AND state='active'",
            [],
        )
        .unwrap();
        let error = claim_exec_env_removal(&mut conn, "/wt/remove")
            .expect_err("dispatch ownership must fence removal");
        assert!(
            error.to_string().contains("lease is dispatching"),
            "{error}"
        );
    }

    #[test]
    fn concurrent_removal_and_dispatch_claims_have_exactly_one_winner() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("claims.sqlite");
        let mut seed = open_file_conn(&db);
        insert_exec_env(&seed, &new_lease("env-race", "/wt/race")).unwrap();
        super::super::exec_env_resources::insert_resource(
            &mut seed,
            &super::super::exec_env_resources::NewExecEnvResource {
                resource_id: "res-race".to_string(),
                kind: super::super::exec_env_resources::ResourceKind::Worktree,
                path: "/wt/race".to_string(),
                bytes: None,
                created_at: String::new(),
            },
        )
        .unwrap();
        super::super::exec_env_resources::bind_resource(&mut seed, "env-race", "res-race").unwrap();
        drop(seed);

        let mut removal_conn = open_file_conn(&db);
        let mut dispatch_conn = open_file_conn(&db);
        let barrier = Arc::new(Barrier::new(2));
        let removal_barrier = Arc::clone(&barrier);
        let removal = std::thread::spawn(move || {
            removal_barrier.wait();
            matches!(
                claim_exec_env_removal(&mut removal_conn, "/wt/race"),
                Ok(Some(env_id)) if env_id == "env-race"
            )
        });
        let dispatch = std::thread::spawn(move || {
            barrier.wait();
            let tx = dispatch_conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let changed = tx
                .execute(
                    "UPDATE exec_envs SET state='dispatching' WHERE env_id='env-race' AND state='active'",
                    [],
                )
                .unwrap();
            tx.commit().unwrap();
            changed == 1
        });

        let removal_won = removal.join().unwrap();
        let dispatch_won = dispatch.join().unwrap();
        assert_ne!(
            removal_won, dispatch_won,
            "the IMMEDIATE transactions and guarded transitions must admit exactly one owner"
        );
    }

    #[test]
    fn reclaim_by_path_targets_the_active_lease() {
        let mut conn = open_conn();
        // A stale reclaimed row plus a live active row for the same path.
        insert_exec_env(&conn, &new_lease("env-old", "/wt/dup")).unwrap();
        reclaim_exec_env(
            &mut conn,
            &ExecEnvSelector::EnvId("env-old".to_string()),
            None,
        )
        .unwrap();
        insert_exec_env(&conn, &new_lease("env-new", "/wt/dup")).unwrap();

        let outcome = reclaim_exec_env(
            &mut conn,
            &ExecEnvSelector::Path("/wt/dup".to_string()),
            Some("cancel"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ReclaimOutcome::Reclaimed {
                env_id: "env-new".to_string()
            }
        );
    }

    #[test]
    fn list_filters_by_state() {
        let mut conn = open_conn();
        insert_exec_env(&conn, &new_lease("env-a", "/wt/a")).unwrap();
        insert_exec_env(&conn, &new_lease("env-b", "/wt/b")).unwrap();
        reclaim_exec_env(
            &mut conn,
            &ExecEnvSelector::EnvId("env-b".to_string()),
            None,
        )
        .unwrap();

        let active = list_exec_envs(&conn, Some(ExecEnvState::Active)).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].env_id, "env-a");

        let reclaimed = list_exec_envs(&conn, Some(ExecEnvState::Reclaimed)).unwrap();
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].env_id, "env-b");

        let all = list_exec_envs(&conn, None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn state_parse_rejects_unknown() {
        assert!(ExecEnvState::parse("provisioned").is_err());
        assert_eq!(ExecEnvState::parse("active").unwrap(), ExecEnvState::Active);
        assert_eq!(
            ExecEnvState::parse("dispatching").unwrap(),
            ExecEnvState::Dispatching
        );
        assert_eq!(
            ExecEnvState::parse("removing").unwrap(),
            ExecEnvState::Removing
        );
        assert_eq!(
            ExecEnvState::parse("reclaimed").unwrap(),
            ExecEnvState::Reclaimed
        );
    }

    #[test]
    fn env_class_defaults_to_edit_only_and_round_trips() {
        let conn = open_conn();
        // The DDL default is the fail-safe one: a lease that never declared a
        // class does not get a build target.
        insert_exec_env(&conn, &new_lease("env-c1", "/wt/c1")).unwrap();
        let got = get_exec_env(&conn, "env-c1").unwrap().unwrap();
        assert_eq!(got.env_class, EnvClass::EditOnly);
        assert!(!got.env_class.allocates_build_target());

        let mut ticketed = new_lease("env-c2", "/wt/c2");
        ticketed.env_class = EnvClass::BuildTicketed;
        insert_exec_env(&conn, &ticketed).unwrap();
        let got = get_exec_env(&conn, "env-c2").unwrap().unwrap();
        assert_eq!(got.env_class, EnvClass::BuildTicketed);
        // A ticketed lease owns NO build target (#894 S2c round-2): the dir its
        // builds run in belongs to the executor seat and is chosen per ticket,
        // so a lease-time binding would book a resource the build may not touch.
        assert!(!got.env_class.allocates_build_target());
        assert!(!got.env_class.requires_approval());

        let mut private = new_lease("env-c3", "/wt/c3");
        private.env_class = EnvClass::BuildPrivate;
        insert_exec_env(&conn, &private).unwrap();
        let got = get_exec_env(&conn, "env-c3").unwrap().unwrap();
        assert_eq!(got.env_class, EnvClass::BuildPrivate);
        assert!(got.env_class.allocates_build_target());
        assert!(got.env_class.requires_approval());
        assert!(got.env_class.wants_private_target());
    }

    #[test]
    fn env_class_parse_rejects_unknown() {
        assert!(EnvClass::parse("build_ticketed").is_err());
        assert!(EnvClass::parse("").is_err());
        assert_eq!(EnvClass::parse("edit-only").unwrap(), EnvClass::EditOnly);
        assert_eq!(EnvClass::default(), EnvClass::EditOnly);
    }
}

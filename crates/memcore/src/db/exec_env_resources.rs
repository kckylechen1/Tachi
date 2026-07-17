//! Execution-environment resource ledger (#894 S2a, quarantine exit added in
//! S2c) — the BYTES behind a lease.
//!
//! `exec_envs` (S1) answers "which leases exist"; it does NOT answer "is the
//! disk actually free". Today's `reclaimed` lease is a SQLite row state and
//! nothing more (#1029): the row flips, the worktree/target can still be
//! sitting on disk. This module is the ledger where `reclaimed` means the
//! bytes are *gone*.
//!
//! ## Model
//!
//! - `exec_env_resources` — one row per *physical* resource (a worktree dir, a
//!   shared cargo target, a scratch dir, a project DB), keyed by
//!   `(path, kind)`.
//! - `exec_env_resource_bindings` — many-to-many lease↔resource. One shared
//!   `build_target` is bound by every live lease at once; `refcount` = the
//!   bindings still open (`released_at IS NULL`).
//!
//! ## Invariants (frozen for #894 S2)
//!
//! 1. **`reclaimed` means bytes freed, not a row flipped.** The ordering is
//!    `reclaiming` (committed) → caller deletes from the filesystem →
//!    `reclaimed_bytes` + `reclaimed` (committed). memcore never touches the
//!    filesystem: [`reclaim_resource`] takes the deleter as a closure so the
//!    order can't be gotten wrong by a call site.
//! 2. **A resource with a live binding is not reclaimable.** [`reclaim_resource`]
//!    returns a typed [`ResourceReclaimOutcome::BlockedByBinding`] — never a
//!    silent skip that would let the caller believe it freed something. The
//!    converse is enforced on the other side too: [`bind_resource`] re-reads the
//!    resource state *inside its own write transaction*, so "resource is active"
//!    and "binding row exists" are decided atomically. Without that, a bind that
//!    read `active` and a reclaim that counted `0` bindings could both commit —
//!    leaving a live binding pointing at deleted bytes.
//! 3. **Every `state` transition goes through this module's typed writers.**
//!    There are exactly four: [`reclaim_resource`] (active/reclaiming/
//!    reclaim_failed → reclaiming → reclaimed | reclaim_failed),
//!    [`quarantine_resource`] (→ quarantined), [`release_quarantine`]
//!    (quarantined → active, S2c's one way back out — gated on the caller
//!    verifying or clearing the bytes first), and [`insert_resource`]'s
//!    re-registration path (reclaimed → active). No call site writes `state` with
//!    ad-hoc SQL, and each writer takes an `IMMEDIATE` transaction (or, for
//!    [`release_quarantine`], a guarded single-row `UPDATE`) so its
//!    read-then-write is atomic against the others.
//! 4. **refcount = live bindings.** A shared resource is only reclaimable after
//!    the *last* binding is released.
//! 5. **Outcomes never over-claim.** A guarded `UPDATE` that matches zero rows
//!    means somebody else moved the row; the caller is told so
//!    ([`ResourceReclaimOutcome::LostRace`]) instead of being handed a
//!    `Reclaimed { bytes }` the ledger never recorded.
//!
//! ## Re-registration (a reclaimed path can come back)
//!
//! `(path, kind)` is UNIQUE — one directory, one row — but the reclaimer's whole
//! job is to churn the *same* paths (a worktree dir gets provisioned, reclaimed,
//! and provisioned again). So [`insert_resource`] on a path whose row is
//! `reclaimed` **revives that row in place**: the caller's fresh `resource_id`
//! becomes the row's id, `bytes`/`measured_at` are re-stamped from the
//! registration, `state` goes back to `active`, and every `reclaim_*` field is
//! cleared ([`RegisterOutcome::Revived`] reports the retired id and the bytes the
//! previous incarnation freed, so an accounting caller does not lose them).
//! Bindings of the previous incarnation stay attached to the retired
//! `resource_id`, which no longer resolves — they can never revive the new
//! incarnation's refcount, and a stale id fails closed at every entry point.
//! Registering over a row in ANY other state (`active`, `reclaiming`,
//! `reclaim_failed`, `quarantined`) is still a hard [`MemoryError::Duplicate`]:
//! two ids for one live directory would split its refcount, and re-registering
//! over `reclaim_failed`/`quarantined` would erase the very signal that says
//! "those bytes may still be on disk".
//!
//! ## Crash semantics
//!
//! A process that dies after the `reclaiming` commit but before the finishing
//! commit leaves the row in `reclaiming`. That is re-enterable: a later
//! [`reclaim_resource`] re-runs the deleter (which must therefore be idempotent
//! — deleting an already-deleted path is `0` freed bytes) and stamps the
//! result. `reclaimed_bytes` is *assigned*, never accumulated, so a retry can't
//! double-count; and a resource already in `reclaimed` short-circuits to
//! [`ResourceReclaimOutcome::AlreadyReclaimed`] without invoking the deleter at
//! all.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

use crate::error::MemoryError;

use super::common::normalize_utc_iso_or_now;

/// Open the write transaction every state-changing entry point in this module
/// uses.
///
/// `IMMEDIATE` (not the default `DEFERRED`) on purpose: these transactions all
/// *read then write* the same rows, and a deferred transaction takes its write
/// lock only at the first write — which is exactly the window where a concurrent
/// reclaim can slip a whole `reclaiming` → delete → `reclaimed` cycle in between.
/// Taking the write lock up front makes each of them serialize against the
/// others (invariants 2 and 3).
fn write_tx(conn: &mut Connection) -> Result<rusqlite::Transaction<'_>, MemoryError> {
    Ok(conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

/// Closed vocabulary of physical resource kinds. Unknown values are an error
/// (fail-closed), same discipline as `ExecEnvState::parse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// A managed git worktree directory.
    Worktree,
    /// A cargo target dir — typically the *shared* `CARGO_TARGET_DIR`, hence
    /// the many-to-many binding model.
    BuildTarget,
    /// A scratch/temp directory owned by a lease.
    ScratchDir,
    /// A per-project SQLite DB provisioned for a lease.
    ProjectDb,
}

impl ResourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceKind::Worktree => "worktree",
            ResourceKind::BuildTarget => "build_target",
            ResourceKind::ScratchDir => "scratch_dir",
            ResourceKind::ProjectDb => "project_db",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "worktree" => Ok(ResourceKind::Worktree),
            "build_target" => Ok(ResourceKind::BuildTarget),
            "scratch_dir" => Ok(ResourceKind::ScratchDir),
            "project_db" => Ok(ResourceKind::ProjectDb),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown exec_env_resource kind '{other}' (expected one of \
                 worktree/build_target/scratch_dir/project_db)"
            ))),
        }
    }
}

/// Resource lifecycle state.
///
/// ```text
///   active ──reclaim──▶ reclaiming ──delete ok──▶ reclaimed
///     ▲  │                 │  ▲                      │
///     │  │       delete err│  │ re-enter             │ re-register the same
///     │  │                 ▼  │ (crash or retry)     │ (path, kind)
///     │  │              reclaim_failed               │
///     │  └──────────────────────────────────────┐    │
///     └───────────────────────────────────────── ────┘
///        (insert_resource revives a reclaimed row)
///
///   active | reclaiming | reclaim_failed ──quarantine──▶ quarantined
///   quarantined ── no automatic transition: a human/broker owns it
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceState {
    /// Live on disk, owned by zero or more leases.
    Active,
    /// A reclaim is in flight: the DB says "we are deleting these bytes".
    /// A crash here is expected and re-enterable.
    Reclaiming,
    /// The bytes are gone and `reclaimed_bytes` records how many.
    Reclaimed,
    /// The deleter failed. Retryable — a later reclaim re-enters.
    ReclaimFailed,
    /// Held back from automatic reclaim (suspect state, needs a human/broker
    /// decision). [`reclaim_resource`] refuses it rather than freeing bytes
    /// someone deliberately fenced off; [`quarantine_resource`] is how it is
    /// entered.
    Quarantined,
}

impl ResourceState {
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceState::Active => "active",
            ResourceState::Reclaiming => "reclaiming",
            ResourceState::Reclaimed => "reclaimed",
            ResourceState::ReclaimFailed => "reclaim_failed",
            ResourceState::Quarantined => "quarantined",
        }
    }

    /// Parse a persisted state. An unknown/legacy value is an error the caller
    /// must surface: a corrupted state must never masquerade as `active`
    /// (usable) or `reclaimed` (bytes freed).
    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "active" => Ok(ResourceState::Active),
            "reclaiming" => Ok(ResourceState::Reclaiming),
            "reclaimed" => Ok(ResourceState::Reclaimed),
            "reclaim_failed" => Ok(ResourceState::ReclaimFailed),
            "quarantined" => Ok(ResourceState::Quarantined),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown exec_env_resource state '{other}' (expected one of \
                 active/reclaiming/reclaimed/reclaim_failed/quarantined)"
            ))),
        }
    }
}

/// A row in `exec_env_resources`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEnvResource {
    pub resource_id: String,
    pub kind: ResourceKind,
    pub path: String,
    /// Most recent measurement, if anyone measured it.
    pub bytes: Option<i64>,
    pub measured_at: Option<String>,
    pub state: ResourceState,
    pub reclaim_reason: Option<String>,
    pub reclaimed_at: Option<String>,
    /// Bytes actually freed from the filesystem by the reclaim.
    pub reclaimed_bytes: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// Fields required to register a resource. `resource_id` is caller-supplied
/// (uuid v4) and unique; `created_at` defaults to now when empty. `bytes`, if
/// given, stamps `measured_at` at the same moment.
#[derive(Debug, Clone)]
pub struct NewExecEnvResource {
    pub resource_id: String,
    pub kind: ResourceKind,
    pub path: String,
    pub bytes: Option<i64>,
    pub created_at: String,
}

/// Result of [`insert_resource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterOutcome {
    /// A brand-new `(path, kind)` was registered.
    Registered { resource_id: String },
    /// The `(path, kind)` existed as a `reclaimed` row and was revived as
    /// `active` under the caller's new `resource_id` — the same physical path,
    /// provisioned again. The previous incarnation's id and freed bytes are
    /// handed back so an accounting caller does not lose them.
    Revived {
        resource_id: String,
        previous_resource_id: String,
        previous_reclaimed_bytes: Option<i64>,
    },
}

/// Result of [`bind_resource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOutcome {
    /// A binding is now live (fresh row, or a previously released row for the
    /// same `(env_id, resource_id)` re-activated).
    Bound { binding_id: String },
    /// The binding was already live; nothing changed (idempotent) — refcount
    /// is NOT incremented by a re-bind.
    AlreadyBound { binding_id: String },
}

/// Result of [`release_binding`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseBindingOutcome {
    /// The binding was live and is now released on this call.
    Released { binding_id: String },
    /// Already released; no state changed (idempotent).
    AlreadyReleased { binding_id: String },
    /// No binding for that `(env_id, resource_id)` pair.
    NotFound,
}

/// Result of [`quarantine_resource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineOutcome {
    /// The resource is now fenced off from automatic reclaim.
    Quarantined { resource_id: String },
    /// Already quarantined; the original reason is kept (idempotent).
    AlreadyQuarantined { resource_id: String },
    /// Refused: the bytes are already gone, there is nothing left to fence off.
    /// Typed rather than silently "succeeding" — a broker that thinks it
    /// quarantined a resource it did not is exactly the #1029 class of lie.
    AlreadyReclaimed { resource_id: String },
    /// No such resource.
    NotFound,
}

/// Result of a reclaim attempt through the single reclaim path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceReclaimOutcome {
    /// The deleter ran and the bytes are gone.
    Reclaimed {
        resource_id: String,
        reclaimed_bytes: i64,
    },
    /// The resource was already `reclaimed`; the deleter was NOT invoked and
    /// `reclaimed_bytes` was not touched (idempotent).
    AlreadyReclaimed { resource_id: String },
    /// Refused: the resource still has live bindings. Typed on purpose — a
    /// silent skip here is how a caller ends up believing it freed disk it did
    /// not free (#1029).
    BlockedByBinding {
        resource_id: String,
        active_bindings: i64,
    },
    /// Refused: the resource is quarantined; a human/broker owns it.
    Quarantined { resource_id: String },
    /// The deleter ran, but by the time we went to stamp the result the row was
    /// no longer `reclaiming` — a concurrent reclaim finished it, a quarantine
    /// fenced it, or the path was re-registered under a new id. Our number was
    /// NOT written (the row's own record stands), and we say so instead of
    /// returning a `Reclaimed { bytes }` that no ledger row backs.
    LostRace {
        resource_id: String,
        /// What the row says now; `None` if the id no longer resolves (the path
        /// was re-registered under a fresh `resource_id`).
        observed_state: Option<ResourceState>,
        /// What OUR deleter reported freeing — unrecorded, for logging only.
        freed_bytes: i64,
    },
    /// No such resource.
    NotFound,
}

const SELECT_COLUMNS: &str = "resource_id, kind, path, bytes, measured_at, state, \
     reclaim_reason, reclaimed_at, reclaimed_bytes, created_at, updated_at";

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

fn row_to_resource(row: &rusqlite::Row<'_>) -> Result<ExecEnvResource, rusqlite::Error> {
    let kind_raw: String = row.get(1)?;
    let kind = ResourceKind::parse(&kind_raw).map_err(|e| conv_err(1, e))?;
    let state_raw: String = row.get(5)?;
    let state = ResourceState::parse(&state_raw).map_err(|e| conv_err(5, e))?;
    Ok(ExecEnvResource {
        resource_id: row.get(0)?,
        kind,
        path: row.get(2)?,
        bytes: row.get(3)?,
        measured_at: row.get(4)?,
        state,
        reclaim_reason: row.get(6)?,
        reclaimed_at: row.get(7)?,
        reclaimed_bytes: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

/// Read a resource's state inside an open transaction. `None` = no such row.
fn state_in_tx(
    tx: &rusqlite::Transaction<'_>,
    resource_id: &str,
) -> Result<Option<ResourceState>, MemoryError> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT state FROM exec_env_resources WHERE resource_id = ?1",
            params![resource_id],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| ResourceState::parse(&raw)).transpose()
}

/// Register a physical resource, or revive a `reclaimed` one at the same
/// `(path, kind)`.
///
/// `(path, kind)` is UNIQUE — one directory, one row — because two rows for one
/// directory would split its refcount and let a live worktree be deleted. But
/// the same path *does* come back: the reclaimer churns worktree dirs, and a
/// path that was reclaimed yesterday is provisioned again today. So:
///
/// - no row at `(path, kind)` → plain insert, [`RegisterOutcome::Registered`];
/// - a `reclaimed` row → revived in place under the caller's new `resource_id`,
///   with `state = 'active'` and every `reclaim_*` field cleared,
///   [`RegisterOutcome::Revived`] (which carries the retired id and the bytes it
///   freed, so the accounting is not silently dropped);
/// - a row in any other state (`active`, `reclaiming`, `reclaim_failed`,
///   `quarantined`) → [`MemoryError::Duplicate`]. Registering over
///   `reclaim_failed`/`quarantined` would erase the signal that says those bytes
///   may still be on disk.
///
/// Runs in an `IMMEDIATE` transaction: the "does this path exist / in what
/// state" read and the insert-or-revive write must not straddle a concurrent
/// reclaim.
pub fn insert_resource(
    conn: &mut Connection,
    res: &NewExecEnvResource,
) -> Result<RegisterOutcome, MemoryError> {
    let now = normalize_utc_iso_or_now(&res.created_at);
    let measured_at = res.bytes.map(|_| now.clone());

    let tx = write_tx(conn)?;
    let existing: Option<(String, String, Option<i64>)> = tx
        .query_row(
            "SELECT resource_id, state, reclaimed_bytes FROM exec_env_resources \
             WHERE path = ?1 AND kind = ?2",
            params![res.path, res.kind.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let outcome = match existing {
        None => {
            tx.execute(
                "INSERT INTO exec_env_resources
                 (resource_id, kind, path, bytes, measured_at, state, reclaim_reason,
                  reclaimed_at, reclaimed_bytes, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', NULL, NULL, NULL, ?6, ?6)",
                params![
                    res.resource_id,
                    res.kind.as_str(),
                    res.path,
                    res.bytes,
                    measured_at,
                    now,
                ],
            )?;
            RegisterOutcome::Registered {
                resource_id: res.resource_id.clone(),
            }
        }
        Some((previous_resource_id, state_raw, previous_reclaimed_bytes)) => {
            let state = ResourceState::parse(&state_raw)?;
            if state != ResourceState::Reclaimed {
                return Err(MemoryError::Duplicate(format!(
                    "exec_env_resource at ('{}', {}) already exists as '{}' \
                     (resource_id '{previous_resource_id}'); only a 'reclaimed' path can be \
                     re-registered",
                    res.path,
                    res.kind.as_str(),
                    state.as_str(),
                )));
            }
            tx.execute(
                "UPDATE exec_env_resources \
                 SET resource_id = ?1, bytes = ?2, measured_at = ?3, state = 'active', \
                     reclaim_reason = NULL, reclaimed_at = NULL, reclaimed_bytes = NULL, \
                     created_at = ?4, updated_at = ?4 \
                 WHERE resource_id = ?5 AND state = 'reclaimed'",
                params![
                    res.resource_id,
                    res.bytes,
                    measured_at,
                    now,
                    previous_resource_id,
                ],
            )?;
            RegisterOutcome::Revived {
                resource_id: res.resource_id.clone(),
                previous_resource_id,
                previous_reclaimed_bytes,
            }
        }
    };
    tx.commit()?;
    Ok(outcome)
}

/// Fetch a resource by id.
pub fn get_resource(
    conn: &Connection,
    resource_id: &str,
) -> Result<Option<ExecEnvResource>, MemoryError> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM exec_env_resources WHERE resource_id = ?1");
    Ok(conn
        .query_row(&sql, params![resource_id], row_to_resource)
        .optional()?)
}

/// Resolve a resource by its natural key — the pair call sites actually hold
/// (a path on disk and what it is).
pub fn find_resource_by_path(
    conn: &Connection,
    path: &str,
    kind: ResourceKind,
) -> Result<Option<ExecEnvResource>, MemoryError> {
    let sql =
        format!("SELECT {SELECT_COLUMNS} FROM exec_env_resources WHERE path = ?1 AND kind = ?2");
    Ok(conn
        .query_row(&sql, params![path, kind.as_str()], row_to_resource)
        .optional()?)
}

/// List resources, optionally filtered by state and/or kind. Newest first.
pub fn list_resources(
    conn: &Connection,
    state: Option<ResourceState>,
    kind: Option<ResourceKind>,
) -> Result<Vec<ExecEnvResource>, MemoryError> {
    let mut sql = format!("SELECT {SELECT_COLUMNS} FROM exec_env_resources WHERE 1 = 1");
    let mut args: Vec<String> = Vec::new();
    if let Some(state) = state {
        args.push(state.as_str().to_string());
        sql.push_str(&format!(" AND state = ?{}", args.len()));
    }
    if let Some(kind) = kind {
        args.push(kind.as_str().to_string());
        sql.push_str(&format!(" AND kind = ?{}", args.len()));
    }
    sql.push_str(" ORDER BY created_at DESC, resource_id DESC");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), row_to_resource)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Every resource path with at least one LIVE binding — a lease that has
/// attached itself to that resource and never released it
/// (`released_at IS NULL`). This is the read half of the write surface S2c
/// shipped: `insert_resource` registers a claim, `bind_resource` is a holder
/// DECLARING ITSELF against it, and this function is how a caller consults
/// that declaration instead of re-deriving "is anything on this path spoken
/// for" by guessing at the process table (#1062 BUG 1 — the orphan reaper's
/// structural fix: a `ps` scan sees only argv and misses a build whose
/// target dir arrived through an inherited `CARGO_TARGET_DIR` environment
/// variable; a binding row does not depend on how the holder's command line
/// was spelled).
///
/// Deliberately narrower than "every non-`reclaimed` row": an `active`
/// resource with ZERO live bindings is not a live holder, it is a tracked
/// physical resource nobody is currently attached to — precisely the
/// population the reaper's OWN re-enterable-orphan path
/// (`cheap_verdict`'s `ReclaimReason::Orphan`) exists to sweep up when it is
/// also stale and unheld. Protecting every non-`reclaimed` row here
/// unconditionally would silence that path entirely and defeat half of what
/// the orphan reaper is for. A binding is the ledger's actual "someone is
/// holding this right now" signal (the same one `active_binding_count` /
/// `BlockedByBinding` already key off of); this reuses it rather than
/// inventing a second one.
///
/// No `kind` filter: a reader protecting disk from deletion has no reason to
/// trust only one resource kind over another, and over-protection is the
/// safe direction here (see `exec_env_reaper::live_build_target_dirs`).
pub fn list_bound_resource_paths(conn: &Connection) -> Result<Vec<String>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT r.path FROM exec_env_resources r \
         JOIN exec_env_resource_bindings b ON b.resource_id = r.resource_id \
         WHERE b.released_at IS NULL \
         ORDER BY r.path",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Record a fresh measurement. memcore does not walk the filesystem — the
/// caller measures, this only stores the number and when it was taken.
///
/// Gated on state: only `active` and `reclaiming` resources can be measured.
/// Stamping fresh bytes onto a `reclaimed` row would make space that is already
/// free look like it is still occupied (and a `quarantined`/`reclaim_failed` row
/// carries a deliberate signal that a measurement must not overwrite). Runs in
/// an `IMMEDIATE` transaction so the gate and the write cannot straddle a
/// concurrent reclaim.
pub fn record_resource_measurement(
    conn: &mut Connection,
    resource_id: &str,
    bytes: i64,
    measured_at: &str,
) -> Result<(), MemoryError> {
    let now = normalize_utc_iso_or_now(measured_at);
    let tx = write_tx(conn)?;

    let Some(state) = state_in_tx(&tx, resource_id)? else {
        return Err(MemoryError::NotFound(format!(
            "exec_env_resource '{resource_id}'"
        )));
    };
    match state {
        ResourceState::Active | ResourceState::Reclaiming => {}
        other => {
            return Err(MemoryError::InvalidArg(format!(
                "exec_env_resource '{resource_id}' is '{}'; only 'active' or 'reclaiming' \
                 resources are measurable (a measurement on a freed resource would make \
                 released bytes look like they are still on disk)",
                other.as_str()
            )));
        }
    }

    tx.execute(
        "UPDATE exec_env_resources SET bytes = ?2, measured_at = ?3, updated_at = ?3 \
         WHERE resource_id = ?1",
        params![resource_id, bytes, now],
    )?;
    tx.commit()?;
    Ok(())
}

/// How many bindings are still live for a resource — THE refcount. A resource
/// with `active_binding_count > 0` is in use and must not be reclaimed.
pub fn active_binding_count(conn: &Connection, resource_id: &str) -> Result<i64, MemoryError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM exec_env_resource_bindings \
         WHERE resource_id = ?1 AND released_at IS NULL",
        params![resource_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Fence a resource off from automatic reclaim (invariant 3: this,
/// [`release_quarantine`], [`reclaim_resource`] and [`insert_resource`] are the
/// only writers of `state`).
///
/// This is the public door into `quarantined` — the state the broker/postflight
/// puts a resource in when it is suspect (a delete that half-succeeded, a path
/// that no longer looks like what the ledger says it is, a dirty worktree a human
/// must look at). Without it, `quarantined` would only be reachable by raw SQL,
/// which is precisely the "somebody else writes `state`" hole invariant 3 exists
/// to close.
///
/// `reason` is stored in `reclaim_reason` (the row's one free-text field — here
/// it reads as "why these bytes are being held back"). Allowed from `active`,
/// `reclaiming` and `reclaim_failed`. A resource that is already `reclaimed` is
/// refused with a typed [`QuarantineOutcome::AlreadyReclaimed`] — its bytes are
/// gone, there is nothing to fence. Re-quarantining is idempotent and keeps the
/// first reason.
///
/// Leaving quarantine was deliberately NOT in S2a — a human/broker decision
/// needed its own audited door. S2c adds that door: [`release_quarantine`].
pub fn quarantine_resource(
    conn: &mut Connection,
    resource_id: &str,
    reason: &str,
) -> Result<QuarantineOutcome, MemoryError> {
    let tx = write_tx(conn)?;
    let Some(state) = state_in_tx(&tx, resource_id)? else {
        tx.commit()?;
        return Ok(QuarantineOutcome::NotFound);
    };

    let outcome = match state {
        ResourceState::Quarantined => QuarantineOutcome::AlreadyQuarantined {
            resource_id: resource_id.to_string(),
        },
        ResourceState::Reclaimed => QuarantineOutcome::AlreadyReclaimed {
            resource_id: resource_id.to_string(),
        },
        ResourceState::Active | ResourceState::Reclaiming | ResourceState::ReclaimFailed => {
            let now = normalize_utc_iso_or_now("");
            let changed = tx.execute(
                "UPDATE exec_env_resources \
                 SET state = 'quarantined', reclaim_reason = ?2, updated_at = ?3 \
                 WHERE resource_id = ?1 \
                   AND state IN ('active', 'reclaiming', 'reclaim_failed')",
                params![resource_id, reason, now],
            )?;
            if changed == 0 {
                // Can't happen while we hold the write lock, but never report a
                // fence we did not actually put up.
                return Err(MemoryError::Internal(format!(
                    "exec_env_resource '{resource_id}': quarantine matched 0 rows"
                )));
            }
            QuarantineOutcome::Quarantined {
                resource_id: resource_id.to_string(),
            }
        }
    };
    tx.commit()?;
    Ok(outcome)
}

/// Result of [`release_quarantine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseQuarantineOutcome {
    /// The caller's verify/clear closure ran and the resource is `active` again.
    Released {
        resource_id: String,
        cleared_bytes: i64,
    },
    /// The resource was not quarantined; the closure did NOT run and no state
    /// changed. A caller must not be able to "release" its way out of a state
    /// it never entered.
    NotQuarantined {
        resource_id: String,
        state: ResourceState,
    },
    /// No such resource.
    NotFound,
}

/// The only exit from `quarantined` (#894 S2c item 4: "an interrupted target
/// must be verified or cleared before a retry may touch it") — and, with
/// [`quarantine_resource`], [`reclaim_resource`] and [`insert_resource`]'s
/// revive, one of this module's four writers of `exec_env_resources.state`.
///
/// The build broker calls [`quarantine_resource`] when a `cargo` invocation was
/// interrupted (a signal / kill / daemon crash mid-build): the target dir it was
/// writing into is now in an unknown state — half-written fingerprints,
/// truncated rlibs — and reusing it is exactly how a "phantom compile error"
/// (symbol greppable in the source, reported as `not found` by rustc) gets
/// manufactured. Quarantined resources are refused by [`reclaim_resource`] and
/// are unbindable by [`bind_resource`] (only `active` resources bind), so a
/// quarantined target cannot be silently picked up by the next build; this
/// function is the only way out, and it forces the caller to verify or clear
/// the bytes first.
///
/// `verify_or_clear` is the caller's filesystem work — wipe the target dir (and
/// return the bytes freed), or verify it in place (and return 0). Taking it as
/// a closure is the same trick [`reclaim_resource`] uses: the ordering cannot be
/// gotten wrong at a call site, because there is no way to reach `active`
/// without having run it. It runs OUTSIDE any transaction (mirroring
/// `reclaim_resource`'s phase split) — holding the write lock across an
/// arbitrary filesystem op would stall every other writer in this module for as
/// long as the wipe takes. If it errors, the resource STAYS quarantined and the
/// error propagates — a failed clear must never hand the next build a poisoned
/// target. The final write is a guarded `UPDATE ... WHERE state = 'quarantined'`
/// so a concurrent re-quarantine (or a second `release_quarantine` racing this
/// one) cannot both commit.
///
/// When the closure reports it freed bytes, the stored measurement is reset to
/// `0` (the dir is empty now); a verify-in-place (0 freed) leaves the last
/// measurement alone rather than fabricating one.
pub fn release_quarantine<F>(
    conn: &mut Connection,
    resource_id: &str,
    verify_or_clear: F,
) -> Result<ReleaseQuarantineOutcome, MemoryError>
where
    F: FnOnce(&ExecEnvResource) -> Result<i64, MemoryError>,
{
    let sql = format!("SELECT {SELECT_COLUMNS} FROM exec_env_resources WHERE resource_id = ?1");
    let resource: Option<ExecEnvResource> = conn
        .query_row(&sql, params![resource_id], row_to_resource)
        .optional()?;

    let Some(resource) = resource else {
        return Ok(ReleaseQuarantineOutcome::NotFound);
    };
    if resource.state != ResourceState::Quarantined {
        return Ok(ReleaseQuarantineOutcome::NotQuarantined {
            resource_id: resource.resource_id,
            state: resource.state,
        });
    }

    // Filesystem work first, outside any transaction: an error here must leave
    // the row quarantined, which is exactly what "do nothing" gives us.
    let cleared = verify_or_clear(&resource)?;

    let now = normalize_utc_iso_or_now("");
    let changed = if cleared > 0 {
        conn.execute(
            "UPDATE exec_env_resources \
             SET state = 'active', reclaim_reason = NULL, bytes = 0, measured_at = ?2, \
                 updated_at = ?2 \
             WHERE resource_id = ?1 AND state = 'quarantined'",
            params![resource_id, now],
        )?
    } else {
        conn.execute(
            "UPDATE exec_env_resources \
             SET state = 'active', reclaim_reason = NULL, updated_at = ?2 \
             WHERE resource_id = ?1 AND state = 'quarantined'",
            params![resource_id, now],
        )?
    };
    if changed == 0 {
        // Somebody else moved the row between our read and this write (a
        // concurrent release, or a re-quarantine). Never claim a release we did
        // not actually commit.
        return Err(MemoryError::Internal(format!(
            "exec_env_resource '{resource_id}': release_quarantine matched 0 rows \
             (a concurrent writer moved it out of 'quarantined')"
        )));
    }
    Ok(ReleaseQuarantineOutcome::Released {
        resource_id: resource.resource_id,
        cleared_bytes: cleared,
    })
}

/// Bind a lease to a resource (refcount +1, unless this pair is already bound).
///
/// Fail-closed on both ends: the lease must exist in `exec_envs` and the
/// resource must be `active`. Binding a lease to a resource whose bytes are
/// being (or have been) freed would hand that lease a path to nothing.
///
/// The whole check-then-bind runs in one `IMMEDIATE` transaction, and the
/// resource's state is read *inside* it (mirroring `reclaim_exec_env`'s
/// in-transaction resolve). That is invariant 2's other half: a bind that read
/// `active` outside a transaction could have its resource reclaimed out from
/// under it — the reclaimer counts zero bindings, commits `reclaiming`, deletes
/// the bytes — and then insert a live binding pointing at a deleted path. Here,
/// the reclaimer's phase-1 transaction and this one are mutually exclusive: bind
/// either wins (and the reclaim comes back `BlockedByBinding`) or loses (and this
/// returns an error, because the row is no longer `active`).
pub fn bind_resource(
    conn: &mut Connection,
    env_id: &str,
    resource_id: &str,
) -> Result<BindOutcome, MemoryError> {
    let tx = write_tx(conn)?;

    let env_exists: bool = tx
        .query_row(
            "SELECT 1 FROM exec_envs WHERE env_id = ?1",
            params![env_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !env_exists {
        return Err(MemoryError::NotFound(format!("exec_env '{env_id}'")));
    }

    let Some(state) = state_in_tx(&tx, resource_id)? else {
        return Err(MemoryError::NotFound(format!(
            "exec_env_resource '{resource_id}'"
        )));
    };
    if state != ResourceState::Active {
        return Err(MemoryError::InvalidArg(format!(
            "exec_env_resource '{resource_id}' is '{}', only 'active' resources are bindable",
            state.as_str()
        )));
    }

    let existing: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT binding_id, released_at FROM exec_env_resource_bindings \
             WHERE env_id = ?1 AND resource_id = ?2",
            params![env_id, resource_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let now = normalize_utc_iso_or_now("");
    let outcome = match existing {
        // Already live: a re-bind is a no-op, so a retrying caller cannot
        // inflate the refcount.
        Some((binding_id, None)) => BindOutcome::AlreadyBound { binding_id },
        // Released earlier: re-activate the same row (UNIQUE(env_id,
        // resource_id) keeps one row per pair by design).
        Some((binding_id, Some(_))) => {
            tx.execute(
                "UPDATE exec_env_resource_bindings SET released_at = NULL, created_at = ?2 \
                 WHERE binding_id = ?1",
                params![binding_id, now],
            )?;
            BindOutcome::Bound { binding_id }
        }
        None => {
            let binding_id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO exec_env_resource_bindings
                 (binding_id, env_id, resource_id, created_at, released_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![binding_id, env_id, resource_id, now],
            )?;
            BindOutcome::Bound { binding_id }
        }
    };
    tx.commit()?;
    Ok(outcome)
}

/// Release a lease's hold on a resource (refcount -1). Idempotent.
pub fn release_binding(
    conn: &Connection,
    env_id: &str,
    resource_id: &str,
) -> Result<ReleaseBindingOutcome, MemoryError> {
    let existing: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT binding_id, released_at FROM exec_env_resource_bindings \
             WHERE env_id = ?1 AND resource_id = ?2",
            params![env_id, resource_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    match existing {
        None => Ok(ReleaseBindingOutcome::NotFound),
        Some((binding_id, Some(_))) => Ok(ReleaseBindingOutcome::AlreadyReleased { binding_id }),
        Some((binding_id, None)) => {
            let now = normalize_utc_iso_or_now("");
            let changed = conn.execute(
                "UPDATE exec_env_resource_bindings SET released_at = ?2 \
                 WHERE binding_id = ?1 AND released_at IS NULL",
                params![binding_id, now],
            )?;
            if changed == 0 {
                // A concurrent release won; say so rather than claiming this
                // call is what dropped the refcount.
                return Ok(ReleaseBindingOutcome::AlreadyReleased { binding_id });
            }
            Ok(ReleaseBindingOutcome::Released { binding_id })
        }
    }
}

/// THE single reclaim path for a resource (#894 S2a) — and, with
/// [`quarantine_resource`], [`release_quarantine`] and [`insert_resource`]'s
/// revive, one of this module's four writers of `exec_env_resources.state`.
///
/// `delete_bytes` is the caller's filesystem deleter: it receives the resource
/// row (as read *before* the `reclaiming` stamp — `path`/`kind` are what a
/// deleter needs) and returns how many bytes it actually freed. memcore never
/// touches the filesystem; taking the deleter as a closure is what makes the
/// frozen ordering unforgeable at the call site:
///
/// 1. in an `IMMEDIATE` transaction: refuse if a binding is live
///    ([`ResourceReclaimOutcome::BlockedByBinding`]) or the resource is
///    quarantined — no state change, no deletion — else commit
///    `state = 'reclaiming'`. Holding the write lock across that read-and-claim is
///    what makes it mutually exclusive with [`bind_resource`];
/// 2. a crash from here on leaves `reclaiming`, which a later call re-enters;
/// 3. run `delete_bytes` (must be idempotent: an already-deleted path frees 0);
/// 4. commit `reclaimed_bytes` + `state = 'reclaimed'`, *guarded* on the row
///    still being `reclaiming`. If it is not, somebody else moved it and we
///    return [`ResourceReclaimOutcome::LostRace`] rather than claiming bytes the
///    ledger does not record.
///
/// A deleter error stamps `reclaim_failed` (retryable) and propagates. An
/// already-`reclaimed` resource short-circuits to
/// [`ResourceReclaimOutcome::AlreadyReclaimed`] *without* invoking the deleter,
/// so `reclaimed_bytes` is never double-counted.
pub fn reclaim_resource<F>(
    conn: &mut Connection,
    resource_id: &str,
    reason: Option<&str>,
    delete_bytes: F,
) -> Result<ResourceReclaimOutcome, MemoryError>
where
    F: FnOnce(&ExecEnvResource) -> Result<i64, MemoryError>,
{
    // Phase 1 — decide and claim, in an IMMEDIATE transaction, so neither a
    // concurrent reclaim of the same resource nor a concurrent bind can slip
    // between the decision and the claim.
    let tx = write_tx(conn)?;
    let sql = format!("SELECT {SELECT_COLUMNS} FROM exec_env_resources WHERE resource_id = ?1");
    let resource: Option<ExecEnvResource> = tx
        .query_row(&sql, params![resource_id], row_to_resource)
        .optional()?;

    let Some(resource) = resource else {
        tx.commit()?;
        return Ok(ResourceReclaimOutcome::NotFound);
    };

    match resource.state {
        ResourceState::Reclaimed => {
            tx.commit()?;
            return Ok(ResourceReclaimOutcome::AlreadyReclaimed {
                resource_id: resource.resource_id,
            });
        }
        ResourceState::Quarantined => {
            tx.commit()?;
            return Ok(ResourceReclaimOutcome::Quarantined {
                resource_id: resource.resource_id,
            });
        }
        // active / reclaiming / reclaim_failed all proceed: the latter two are
        // the re-entry paths (crash mid-delete, or a retry after a failure).
        ResourceState::Active | ResourceState::Reclaiming | ResourceState::ReclaimFailed => {}
    }

    let active_bindings: i64 = tx.query_row(
        "SELECT COUNT(*) FROM exec_env_resource_bindings \
         WHERE resource_id = ?1 AND released_at IS NULL",
        params![resource_id],
        |row| row.get(0),
    )?;
    if active_bindings > 0 {
        tx.commit()?;
        return Ok(ResourceReclaimOutcome::BlockedByBinding {
            resource_id: resource.resource_id,
            active_bindings,
        });
    }

    let now = normalize_utc_iso_or_now("");
    tx.execute(
        "UPDATE exec_env_resources \
         SET state = 'reclaiming', reclaim_reason = COALESCE(?2, reclaim_reason), \
             updated_at = ?3 \
         WHERE resource_id = ?1",
        params![resource_id, reason, now],
    )?;
    tx.commit()?;

    // Phase 2 — the filesystem work, OUTSIDE any transaction and outside
    // memcore. If the process dies here the row stays `reclaiming` and the next
    // call re-enters.
    let freed = match delete_bytes(&resource) {
        Ok(freed) => freed,
        Err(err) => {
            let now = normalize_utc_iso_or_now("");
            let stamped = conn.execute(
                "UPDATE exec_env_resources SET state = 'reclaim_failed', updated_at = ?2 \
                 WHERE resource_id = ?1 AND state = 'reclaiming'",
                params![resource_id, now],
            );
            // The deleter's error is the one the caller needs: it says what
            // happened to the actual bytes. If the bookkeeping write ALSO failed,
            // report both — swallowing the filesystem error behind a SQLite error
            // (`?`) would hide the reason the disk is still full.
            if let Err(db_err) = stamped {
                return Err(MemoryError::Internal(format!(
                    "exec_env_resource '{resource_id}': delete failed ({err}) AND the \
                     'reclaim_failed' stamp failed ({db_err}) — the row may still read \
                     'reclaiming'"
                )));
            }
            return Err(err);
        }
    };

    // Phase 3 — the bytes are gone; record how many. Assignment, not
    // accumulation: a re-entered reclaim overwrites with what IT freed rather
    // than adding to a previous attempt's number. The `state = 'reclaiming'`
    // guard is what keeps a *concurrent* re-entrant reclaim (which would free 0,
    // the winner having already deleted the path) from clobbering the winner's
    // `reclaimed_bytes` with a zero — and when the guard bites (0 rows), we say
    // we lost the race instead of reporting a `Reclaimed` the ledger never took.
    let now = normalize_utc_iso_or_now("");
    let changed = conn.execute(
        "UPDATE exec_env_resources \
         SET state = 'reclaimed', reclaimed_at = ?2, reclaimed_bytes = ?3, updated_at = ?2 \
         WHERE resource_id = ?1 AND state = 'reclaiming'",
        params![resource_id, now, freed],
    )?;
    if changed == 0 {
        let observed_state = get_resource(conn, resource_id)?.map(|r| r.state);
        return Ok(ResourceReclaimOutcome::LostRace {
            resource_id: resource.resource_id,
            observed_state,
            freed_bytes: freed,
        });
    }

    Ok(ResourceReclaimOutcome::Reclaimed {
        resource_id: resource.resource_id,
        reclaimed_bytes: freed,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Barrier;
    use std::thread;

    use super::*;
    use crate::db::exec_env::{insert_exec_env, EnvClass, NewExecEnvLease};

    fn open_conn() -> Connection {
        // Same raw-connection fixture as `exec_env`'s tests: schema init
        // creates FTS tables needing the registered simple tokenizer.
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    /// A REAL file DB. Concurrency tests cannot use `:memory:` — each connection
    /// would get its own private database and the race under test would not
    /// exist. `init_schema` also applies the WAL + `busy_timeout=5000` pragmas,
    /// which is what makes the loser of a write-lock race wait instead of
    /// erroring.
    fn open_file_conn(db_path: &std::path::Path) -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open(db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn seed_env(conn: &Connection, env_id: &str) {
        insert_exec_env(
            conn,
            &NewExecEnvLease {
                env_id: env_id.to_string(),
                kind: "worktree".to_string(),
                path: format!("/wt/{env_id}"),
                repo_root: "/repo".to_string(),
                branch: "tachi/894/w".to_string(),
                base_sha: "abc123".to_string(),
                dispatch_id: None,
                env_class: EnvClass::default(),
                created_at: String::new(),
            },
        )
        .unwrap();
    }

    fn new_resource(id: &str, kind: ResourceKind, path: &str) -> NewExecEnvResource {
        NewExecEnvResource {
            resource_id: id.to_string(),
            kind,
            path: path.to_string(),
            bytes: None,
            created_at: String::new(),
        }
    }

    /// A deleter that reports `freed` bytes and counts how many times it ran —
    /// "was the filesystem actually touched" is the whole point of this ledger.
    fn counting_deleter(
        calls: &Cell<u32>,
        freed: i64,
    ) -> impl FnOnce(&ExecEnvResource) -> Result<i64, MemoryError> + '_ {
        move |_res| {
            calls.set(calls.get() + 1);
            Ok(freed)
        }
    }

    #[test]
    fn insert_and_get_roundtrip_defaults_to_active() {
        let mut conn = open_conn();
        let outcome = insert_resource(
            &mut conn,
            &NewExecEnvResource {
                resource_id: "res-1".to_string(),
                kind: ResourceKind::BuildTarget,
                path: "/cache/sigil-shared-target".to_string(),
                bytes: Some(4096),
                created_at: String::new(),
            },
        )
        .unwrap();
        assert_eq!(
            outcome,
            RegisterOutcome::Registered {
                resource_id: "res-1".to_string()
            }
        );
        let got = get_resource(&conn, "res-1").unwrap().expect("row present");
        assert_eq!(got.state, ResourceState::Active);
        assert_eq!(got.kind, ResourceKind::BuildTarget);
        assert_eq!(got.bytes, Some(4096));
        assert!(got.measured_at.is_some(), "bytes stamp measured_at");
        assert!(got.reclaimed_bytes.is_none());
        assert!(!got.created_at.is_empty());
    }

    // Discriminating test ⑤ — one directory, one row. Two rows for the same
    // path would split the refcount and let a live worktree be deleted.
    #[test]
    fn duplicate_path_and_kind_is_rejected() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-a", ResourceKind::Worktree, "/wt/x"),
        )
        .unwrap();
        let err = insert_resource(
            &mut conn,
            &new_resource("res-b", ResourceKind::Worktree, "/wt/x"),
        );
        assert!(err.is_err(), "duplicate (path, kind) must be rejected");

        // A different kind at the same path is a different physical resource
        // (e.g. a project DB inside a worktree) and is allowed.
        insert_resource(
            &mut conn,
            &new_resource("res-c", ResourceKind::ProjectDb, "/wt/x"),
        )
        .unwrap();
        assert!(get_resource(&conn, "res-c").unwrap().is_some());

        // …and the natural key resolves each one independently.
        let by_path = find_resource_by_path(&conn, "/wt/x", ResourceKind::Worktree)
            .unwrap()
            .expect("worktree row");
        assert_eq!(by_path.resource_id, "res-a");
        assert!(
            find_resource_by_path(&conn, "/wt/x", ResourceKind::ScratchDir)
                .unwrap()
                .is_none()
        );
    }

    /// #1062 BUG 1's read surface: a resource with a LIVE binding is bound, one
    /// whose binding was released is not, and an `active`-but-never-bound
    /// resource (tracked, but nobody currently holds it) is not either —
    /// discriminating against a naive `state != 'reclaimed'` filter, which would
    /// also catch the never-bound row and silence the reaper's re-enterable-
    /// orphan path for every tracked-but-abandoned resource on the books.
    #[test]
    fn bound_resource_paths_reflects_only_live_bindings() {
        let mut conn = open_conn();
        seed_env(&conn, "env-bound");

        insert_resource(
            &mut conn,
            &new_resource("res-bound", ResourceKind::BuildTarget, "/t/bound"),
        )
        .unwrap();
        bind_resource(&mut conn, "env-bound", "res-bound").unwrap();

        insert_resource(
            &mut conn,
            &new_resource("res-released", ResourceKind::BuildTarget, "/t/released"),
        )
        .unwrap();
        bind_resource(&mut conn, "env-bound", "res-released").unwrap();
        release_binding(&conn, "env-bound", "res-released").unwrap();

        // Tracked and `active`, but nobody ever bound it — the population
        // `cheap_verdict`'s re-enterable `Orphan` path exists to sweep up.
        insert_resource(
            &mut conn,
            &new_resource("res-untouched", ResourceKind::BuildTarget, "/t/untouched"),
        )
        .unwrap();

        let live = list_bound_resource_paths(&conn).unwrap();
        assert_eq!(
            live,
            vec!["/t/bound".to_string()],
            "only a resource with a LIVE binding counts; a released binding and a \
             never-bound-but-active resource must both be absent"
        );
    }

    // Discriminating test ⑥ (review round 2, item 2) — a RECLAIMED path must be
    // re-registerable. The reclaimer churns the same worktree dirs forever; a
    // bare INSERT against UNIQUE(path, kind) meant a path could be provisioned
    // exactly once in the lifetime of the DB.
    #[test]
    fn a_reclaimed_path_can_be_registered_again() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-old", ResourceKind::Worktree, "/wt/churn"),
        )
        .unwrap();
        let calls = Cell::new(0);
        reclaim_resource(
            &mut conn,
            "res-old",
            Some("terminal_state"),
            counting_deleter(&calls, 8_192),
        )
        .unwrap();
        assert_eq!(
            get_resource(&conn, "res-old").unwrap().unwrap().state,
            ResourceState::Reclaimed
        );

        // The same physical path is provisioned again, with a fresh uuid.
        let outcome = insert_resource(
            &mut conn,
            &new_resource("res-new", ResourceKind::Worktree, "/wt/churn"),
        )
        .unwrap();
        assert_eq!(
            outcome,
            RegisterOutcome::Revived {
                resource_id: "res-new".to_string(),
                previous_resource_id: "res-old".to_string(),
                previous_reclaimed_bytes: Some(8_192),
            },
            "re-registration must report what it replaced, not silently overwrite it"
        );

        let revived = get_resource(&conn, "res-new")
            .unwrap()
            .expect("the revived row resolves under the new id");
        assert_eq!(revived.state, ResourceState::Active, "usable again");
        assert_eq!(revived.path, "/wt/churn");
        assert!(revived.reclaimed_at.is_none(), "reclaim_* cleared");
        assert!(revived.reclaimed_bytes.is_none(), "reclaim_* cleared");
        assert!(revived.reclaim_reason.is_none(), "reclaim_* cleared");

        // Still ONE row for the directory, and the retired id fails closed.
        assert_eq!(
            list_resources(&conn, None, Some(ResourceKind::Worktree))
                .unwrap()
                .len(),
            1
        );
        assert!(get_resource(&conn, "res-old").unwrap().is_none());

        // And it is bindable again — the point of the whole exercise.
        seed_env(&conn, "env-2");
        assert!(matches!(
            bind_resource(&mut conn, "env-2", "res-new").unwrap(),
            BindOutcome::Bound { .. }
        ));
        assert_eq!(active_binding_count(&conn, "res-new").unwrap(), 1);
    }

    // …but only a `reclaimed` path revives. Re-registering over a row whose bytes
    // may still be on disk (reclaim_failed) or that a human fenced off
    // (quarantined) would erase exactly the signal that says "look at me".
    #[test]
    fn re_registration_is_refused_for_non_reclaimed_rows() {
        let mut conn = open_conn();

        // reclaim_failed
        insert_resource(
            &mut conn,
            &new_resource("res-f", ResourceKind::ScratchDir, "/s/f"),
        )
        .unwrap();
        let _ = reclaim_resource(&mut conn, "res-f", Some("sweep"), |_res| {
            Err(MemoryError::Io(std::io::Error::other("rm failed")))
        });
        assert_eq!(
            get_resource(&conn, "res-f").unwrap().unwrap().state,
            ResourceState::ReclaimFailed
        );
        assert!(
            insert_resource(
                &mut conn,
                &new_resource("res-f2", ResourceKind::ScratchDir, "/s/f")
            )
            .is_err(),
            "a reclaim_failed path may still hold bytes — re-registering would erase that"
        );

        // quarantined
        insert_resource(
            &mut conn,
            &new_resource("res-q", ResourceKind::ScratchDir, "/s/q"),
        )
        .unwrap();
        quarantine_resource(&mut conn, "res-q", "suspect").unwrap();
        assert!(
            insert_resource(
                &mut conn,
                &new_resource("res-q2", ResourceKind::ScratchDir, "/s/q")
            )
            .is_err(),
            "a quarantined path is fenced off; registration must not lift the fence"
        );
        assert_eq!(
            get_resource(&conn, "res-q").unwrap().unwrap().state,
            ResourceState::Quarantined
        );
    }

    // Discriminating test ① — invariant 2: live binding ⇒ not reclaimable, and
    // the refusal is TYPED (a silent skip is exactly the #1029 bug).
    #[test]
    fn reclaim_is_blocked_while_a_binding_is_live() {
        let mut conn = open_conn();
        seed_env(&conn, "env-1");
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();
        bind_resource(&mut conn, "env-1", "res-1").unwrap();

        let calls = Cell::new(0);
        let outcome = reclaim_resource(
            &mut conn,
            "res-1",
            Some("sweep"),
            counting_deleter(&calls, 999),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ResourceReclaimOutcome::BlockedByBinding {
                resource_id: "res-1".to_string(),
                active_bindings: 1,
            }
        );
        assert_eq!(
            calls.get(),
            0,
            "deleter must NOT run while a binding is live"
        );

        let got = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(got.state, ResourceState::Active, "state untouched");
        assert!(got.reclaimed_bytes.is_none());
    }

    // Discriminating test ② — releasing the last binding unblocks reclaim, and
    // `reclaimed` carries the bytes the deleter actually freed.
    #[test]
    fn reclaim_succeeds_after_last_binding_released() {
        let mut conn = open_conn();
        seed_env(&conn, "env-1");
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();
        bind_resource(&mut conn, "env-1", "res-1").unwrap();

        let released = release_binding(&conn, "env-1", "res-1").unwrap();
        assert!(matches!(released, ReleaseBindingOutcome::Released { .. }));
        assert_eq!(active_binding_count(&conn, "res-1").unwrap(), 0);

        let calls = Cell::new(0);
        let outcome = reclaim_resource(
            &mut conn,
            "res-1",
            Some("terminal_state"),
            counting_deleter(&calls, 12_345),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ResourceReclaimOutcome::Reclaimed {
                resource_id: "res-1".to_string(),
                reclaimed_bytes: 12_345,
            }
        );
        assert_eq!(calls.get(), 1);

        let got = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(got.state, ResourceState::Reclaimed);
        assert_eq!(got.reclaimed_bytes, Some(12_345));
        assert_eq!(got.reclaim_reason.as_deref(), Some("terminal_state"));
        assert!(got.reclaimed_at.is_some());

        // Idempotent: a second reclaim neither re-runs the deleter nor
        // re-stamps the bytes.
        let again = reclaim_resource(
            &mut conn,
            "res-1",
            Some("again"),
            counting_deleter(&calls, 777),
        )
        .unwrap();
        assert_eq!(
            again,
            ResourceReclaimOutcome::AlreadyReclaimed {
                resource_id: "res-1".to_string(),
            }
        );
        assert_eq!(
            calls.get(),
            1,
            "deleter must not run for an already-reclaimed resource"
        );
        let after = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(after.reclaimed_bytes, Some(12_345), "bytes not re-counted");
        assert_eq!(after.reclaim_reason.as_deref(), Some("terminal_state"));
    }

    // Discriminating test ③ — invariant 4: a SHARED resource (two leases, one
    // cargo target) survives until the last binding goes.
    #[test]
    fn shared_resource_survives_until_the_last_binding_is_released() {
        let mut conn = open_conn();
        seed_env(&conn, "env-1");
        seed_env(&conn, "env-2");
        insert_resource(
            &mut conn,
            &new_resource(
                "res-target",
                ResourceKind::BuildTarget,
                "/cache/sigil-shared-target",
            ),
        )
        .unwrap();
        bind_resource(&mut conn, "env-1", "res-target").unwrap();
        bind_resource(&mut conn, "env-2", "res-target").unwrap();
        assert_eq!(active_binding_count(&conn, "res-target").unwrap(), 2);

        // A re-bind of a live pair must not inflate the refcount.
        assert!(matches!(
            bind_resource(&mut conn, "env-2", "res-target").unwrap(),
            BindOutcome::AlreadyBound { .. }
        ));
        assert_eq!(active_binding_count(&conn, "res-target").unwrap(), 2);

        release_binding(&conn, "env-1", "res-target").unwrap();
        assert_eq!(active_binding_count(&conn, "res-target").unwrap(), 1);

        let calls = Cell::new(0);
        // refcount 1 (env-2 still building) — the shared target must NOT be
        // deleted out from under it.
        let blocked =
            reclaim_resource(&mut conn, "res-target", None, counting_deleter(&calls, 1)).unwrap();
        assert_eq!(
            blocked,
            ResourceReclaimOutcome::BlockedByBinding {
                resource_id: "res-target".to_string(),
                active_bindings: 1,
            }
        );
        assert_eq!(calls.get(), 0);

        // Releasing an already-released binding is a no-op — it cannot drive
        // the refcount below the truth.
        assert!(matches!(
            release_binding(&conn, "env-1", "res-target").unwrap(),
            ReleaseBindingOutcome::AlreadyReleased { .. }
        ));
        assert_eq!(active_binding_count(&conn, "res-target").unwrap(), 1);

        release_binding(&conn, "env-2", "res-target").unwrap();
        assert_eq!(active_binding_count(&conn, "res-target").unwrap(), 0);

        let outcome = reclaim_resource(
            &mut conn,
            "res-target",
            Some("last_binding_released"),
            counting_deleter(&calls, 8_000_000),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ResourceReclaimOutcome::Reclaimed {
                resource_id: "res-target".to_string(),
                reclaimed_bytes: 8_000_000,
            }
        );
        assert_eq!(calls.get(), 1);
    }

    // Discriminating test ④ (rewritten, review round 2 item 4) — the retry after
    // a crash must ASSIGN the bytes it freed, not add them to what a previous
    // attempt recorded.
    //
    // The first attempt must therefore SUCCEED (100 bytes recorded); only then is
    // there a number to double-count. (The previous version of this test crashed
    // the *first* attempt, so `reclaimed_bytes` was NULL going into the retry and
    // an accumulating implementation would have passed it.)
    #[test]
    fn a_reentered_reclaim_assigns_bytes_and_cannot_double_count() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::ScratchDir, "/scratch/1"),
        )
        .unwrap();

        let calls = Cell::new(0);
        let first = reclaim_resource(
            &mut conn,
            "res-1",
            Some("sweep"),
            counting_deleter(&calls, 100),
        )
        .unwrap();
        assert_eq!(
            first,
            ResourceReclaimOutcome::Reclaimed {
                resource_id: "res-1".to_string(),
                reclaimed_bytes: 100,
            }
        );
        assert_eq!(
            get_resource(&conn, "res-1")
                .unwrap()
                .unwrap()
                .reclaimed_bytes,
            Some(100),
            "first attempt really did record 100 — this is the number a retry could double"
        );

        // Now the crash: the process died between the finishing UPDATE and its
        // commit being observed, so a restart finds the row back in `reclaiming`
        // WITH the 100 already stamped. This is the only shape in which
        // double-counting is even possible.
        conn.execute(
            "UPDATE exec_env_resources SET state = 'reclaiming' WHERE resource_id = ?1",
            params!["res-1"],
        )
        .unwrap();

        // The retry's deleter frees 100 again (a re-run of the same rm over a dir
        // the DB never confirmed gone). An accumulating implementation records
        // 200; an assigning one records 100.
        let retry =
            reclaim_resource(&mut conn, "res-1", None, counting_deleter(&calls, 100)).unwrap();
        assert_eq!(
            retry,
            ResourceReclaimOutcome::Reclaimed {
                resource_id: "res-1".to_string(),
                reclaimed_bytes: 100,
            }
        );
        assert_eq!(
            calls.get(),
            2,
            "the retry re-enters and re-runs the deleter"
        );

        let done = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(done.state, ResourceState::Reclaimed);
        assert_ne!(
            done.reclaimed_bytes,
            Some(200),
            "bytes must be ASSIGNED, not accumulated — 200 would be a lie about freed disk"
        );
        assert_eq!(done.reclaimed_bytes, Some(100), "assigned, not accumulated");
        // The first reclaim's reason survives the re-entry (None doesn't wipe it).
        assert_eq!(done.reclaim_reason.as_deref(), Some("sweep"));

        // And a third call is a pure no-op.
        let again =
            reclaim_resource(&mut conn, "res-1", None, counting_deleter(&calls, 55)).unwrap();
        assert_eq!(
            again,
            ResourceReclaimOutcome::AlreadyReclaimed {
                resource_id: "res-1".to_string()
            }
        );
        assert_eq!(calls.get(), 2);
        assert_eq!(
            get_resource(&conn, "res-1")
                .unwrap()
                .unwrap()
                .reclaimed_bytes,
            Some(100)
        );
    }

    // A deleter that fails must never leave the row claiming its bytes are gone —
    // and the failure is retryable.
    #[test]
    fn a_failed_delete_claims_no_bytes_and_is_retryable() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::ScratchDir, "/scratch/1"),
        )
        .unwrap();

        let err = reclaim_resource(&mut conn, "res-1", Some("sweep"), |_res| {
            Err(MemoryError::Io(std::io::Error::other("rm -rf died")))
        })
        .unwrap_err();
        assert!(err.to_string().contains("rm -rf died"));

        let mid = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(mid.state, ResourceState::ReclaimFailed);
        assert_ne!(
            mid.state,
            ResourceState::Reclaimed,
            "a failed delete must never report reclaimed"
        );
        assert!(
            mid.reclaimed_bytes.is_none(),
            "no bytes claimed for a failed delete"
        );

        // reclaim_failed is a re-entry point, not a dead end.
        let calls = Cell::new(0);
        let retry =
            reclaim_resource(&mut conn, "res-1", None, counting_deleter(&calls, 64)).unwrap();
        assert_eq!(
            retry,
            ResourceReclaimOutcome::Reclaimed {
                resource_id: "res-1".to_string(),
                reclaimed_bytes: 64,
            }
        );
        assert_eq!(calls.get(), 1);
    }

    // Review round 2, item 7 — when the deleter fails AND the bookkeeping write
    // fails too, the filesystem error must not be swallowed by the SQLite one:
    // the FS error is the one that explains why the disk is still full.
    #[test]
    fn a_deleter_error_survives_a_failing_reclaim_failed_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("resources.db");
        let mut conn = open_file_conn(&db_path);
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();

        let err = reclaim_resource(&mut conn, "res-1", Some("sweep"), |_res| {
            // Break the bookkeeping out from under the error path (a stand-in for
            // "the DB went away mid-reclaim"), then fail the delete.
            let other = Connection::open(&db_path).unwrap();
            other.execute("DROP TABLE exec_env_resources", []).unwrap();
            Err(MemoryError::Io(std::io::Error::other("rm -rf died")))
        })
        .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("rm -rf died"),
            "the filesystem error must survive the failed DB stamp: {msg}"
        );
        assert!(
            msg.contains("exec_env_resources"),
            "…and the DB failure must be reported alongside it: {msg}"
        );
    }

    // Invariant 1, ordering: `reclaiming` is COMMITTED before the deleter runs.
    // If the whole reclaim lived in one transaction, a second connection would
    // still see `active` here and this assertion would fail — which is what
    // makes a crash mid-delete recoverable at all.
    #[test]
    fn reclaiming_is_committed_before_the_deleter_runs() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("resources.db");
        let mut conn = open_file_conn(&db_path);
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();

        let observed: Cell<Option<String>> = Cell::new(None);
        reclaim_resource(&mut conn, "res-1", Some("sweep"), |_res| {
            // A separate connection = what a crashed-then-restarted daemon (or
            // a concurrent sweep) would see on disk at this instant.
            let other = Connection::open(&db_path).unwrap();
            let state: String = other
                .query_row(
                    "SELECT state FROM exec_env_resources WHERE resource_id = 'res-1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            observed.set(Some(state));
            Ok(64)
        })
        .unwrap();

        assert_eq!(
            observed.into_inner().as_deref(),
            Some("reclaiming"),
            "the deleter must run with `reclaiming` already durable"
        );
        assert_eq!(
            get_resource(&conn, "res-1").unwrap().unwrap().state,
            ResourceState::Reclaimed
        );
    }

    // Review round 2, item 3 — the finishing UPDATE is guarded on
    // `state = 'reclaiming'`. When that guard bites (somebody else finished the
    // row while our deleter was running), the old code still returned
    // `Reclaimed { bytes }` for a write that touched zero rows. Now it says it
    // lost, and the winner's number is left standing.
    #[test]
    fn losing_the_reclaiming_row_reports_lost_race_not_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("resources.db");
        let mut conn = open_file_conn(&db_path);
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();

        let outcome = reclaim_resource(&mut conn, "res-1", Some("sweep"), |_res| {
            // While we are "deleting", another sweep on another connection
            // finishes the row and records ITS number.
            let other = Connection::open(&db_path).unwrap();
            other
                .execute(
                    "UPDATE exec_env_resources \
                     SET state = 'reclaimed', reclaimed_bytes = 7, \
                         reclaimed_at = '2026-07-13T00:00:00Z' \
                     WHERE resource_id = 'res-1'",
                    [],
                )
                .unwrap();
            Ok(4_096)
        })
        .unwrap();

        assert_eq!(
            outcome,
            ResourceReclaimOutcome::LostRace {
                resource_id: "res-1".to_string(),
                observed_state: Some(ResourceState::Reclaimed),
                freed_bytes: 4_096,
            },
            "a zero-row UPDATE must not be reported as a successful reclaim"
        );
        assert_eq!(
            get_resource(&conn, "res-1")
                .unwrap()
                .unwrap()
                .reclaimed_bytes,
            Some(7),
            "the winner's number stands; the loser does not clobber the ledger"
        );
    }

    // Review round 2, item 1 — THE frozen invariant: a bind and a reclaim of the
    // same resource can never both succeed. Two real connections on a real file
    // DB, released together by a barrier, N rounds.
    //
    // Before the fix, `bind_resource` read the resource state outside any
    // transaction and then INSERTed: a reclaim could commit `reclaiming`, run the
    // deleter and commit `reclaimed` inside that window, leaving a live binding
    // that points at deleted bytes. Both calls would report success. Now bind takes
    // an IMMEDIATE transaction and re-reads the state inside it, so exactly one of
    // the two wins.
    #[test]
    fn concurrent_bind_and_reclaim_cannot_both_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("resources.db");
        let mut binder = open_file_conn(&db_path);
        let mut reclaimer = open_file_conn(&db_path);

        seed_env(&binder, "env-1");

        const ROUNDS: usize = 64;
        for round in 0..ROUNDS {
            let resource_id = format!("res-{round}");
            insert_resource(
                &mut binder,
                &new_resource(
                    &resource_id,
                    ResourceKind::Worktree,
                    &format!("/wt/{round}"),
                ),
            )
            .unwrap();
        }

        for round in 0..ROUNDS {
            let resource_id = format!("res-{round}");
            let barrier = Barrier::new(2);
            let deleter_ran = AtomicBool::new(false);

            let (bind_outcome, reclaim_outcome) = thread::scope(|scope| {
                let binder = &mut binder;
                let reclaimer = &mut reclaimer;
                let barrier = &barrier;
                let deleter_ran = &deleter_ran;
                let rid = resource_id.as_str();

                let bind_thread = scope.spawn(move || {
                    barrier.wait();
                    bind_resource(binder, "env-1", rid)
                });
                let reclaim_thread = scope.spawn(move || {
                    barrier.wait();
                    reclaim_resource(reclaimer, rid, Some("race"), |_res| {
                        deleter_ran.store(true, Ordering::SeqCst);
                        Ok(4_096)
                    })
                });
                (bind_thread.join().unwrap(), reclaim_thread.join().unwrap())
            });

            let bind_won = matches!(bind_outcome, Ok(BindOutcome::Bound { .. }));
            let reclaim_won = matches!(
                reclaim_outcome,
                Ok(ResourceReclaimOutcome::Reclaimed { .. })
            );
            assert!(
                bind_won ^ reclaim_won,
                "round {round}: exactly one of bind/reclaim may win — \
                 bind={bind_outcome:?} reclaim={reclaim_outcome:?}"
            );

            let resource = get_resource(&binder, &resource_id).unwrap().unwrap();
            let live = active_binding_count(&binder, &resource_id).unwrap();
            let deleted = deleter_ran.load(Ordering::SeqCst);

            if bind_won {
                assert!(
                    !deleted,
                    "round {round}: the deleter ran even though a binding is live"
                );
                assert_eq!(resource.state, ResourceState::Active, "round {round}");
                assert_eq!(live, 1, "round {round}");
                assert!(
                    matches!(
                        reclaim_outcome,
                        Ok(ResourceReclaimOutcome::BlockedByBinding { .. })
                    ),
                    "round {round}: the losing reclaim must say why: {reclaim_outcome:?}"
                );
            } else {
                assert!(
                    deleted,
                    "round {round}: the winning reclaim must have deleted"
                );
                assert_eq!(resource.state, ResourceState::Reclaimed, "round {round}");
                assert_eq!(
                    live, 0,
                    "round {round}: a lease is bound to bytes that are gone"
                );
                assert!(
                    bind_outcome.is_err(),
                    "round {round}: the losing bind must fail closed: {bind_outcome:?}"
                );
            }
        }
    }

    // Review round 2, item 5 — `quarantined` must be reachable through the public
    // API (invariant 3: nothing writes `state` with ad-hoc SQL). The broker and
    // postflight both need this door.
    #[test]
    fn quarantine_is_public_and_the_quarantined_are_refused_not_freed() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-q", ResourceKind::ScratchDir, "/scratch/q"),
        )
        .unwrap();

        let outcome =
            quarantine_resource(&mut conn, "res-q", "dirty worktree, human decides").unwrap();
        assert_eq!(
            outcome,
            QuarantineOutcome::Quarantined {
                resource_id: "res-q".to_string()
            }
        );
        let got = get_resource(&conn, "res-q").unwrap().unwrap();
        assert_eq!(got.state, ResourceState::Quarantined);
        assert_eq!(
            got.reclaim_reason.as_deref(),
            Some("dirty worktree, human decides")
        );

        // Idempotent, and the first reason stands.
        assert_eq!(
            quarantine_resource(&mut conn, "res-q", "second thoughts").unwrap(),
            QuarantineOutcome::AlreadyQuarantined {
                resource_id: "res-q".to_string()
            }
        );
        assert_eq!(
            get_resource(&conn, "res-q")
                .unwrap()
                .unwrap()
                .reclaim_reason
                .as_deref(),
            Some("dirty worktree, human decides")
        );

        // The fence holds: reclaim refuses and the deleter never runs.
        let calls = Cell::new(0);
        let refused = reclaim_resource(
            &mut conn,
            "res-q",
            Some("sweep"),
            counting_deleter(&calls, 1),
        )
        .unwrap();
        assert_eq!(
            refused,
            ResourceReclaimOutcome::Quarantined {
                resource_id: "res-q".to_string()
            }
        );
        assert_eq!(calls.get(), 0, "quarantined bytes must never be deleted");
        assert_eq!(
            get_resource(&conn, "res-q").unwrap().unwrap().state,
            ResourceState::Quarantined
        );

        // Quarantining bytes that are already gone is refused, typed — not a
        // silent success.
        insert_resource(
            &mut conn,
            &new_resource("res-r", ResourceKind::ScratchDir, "/scratch/r"),
        )
        .unwrap();
        reclaim_resource(&mut conn, "res-r", None, counting_deleter(&calls, 10)).unwrap();
        assert_eq!(
            quarantine_resource(&mut conn, "res-r", "too late").unwrap(),
            QuarantineOutcome::AlreadyReclaimed {
                resource_id: "res-r".to_string()
            }
        );
        assert_eq!(
            quarantine_resource(&mut conn, "ghost", "nope").unwrap(),
            QuarantineOutcome::NotFound
        );
    }

    // A reclaim_failed resource can be fenced off (that is the point: a delete
    // that keeps failing is exactly what a human should look at).
    #[test]
    fn quarantine_accepts_a_reclaim_failed_resource() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();
        let _ = reclaim_resource(&mut conn, "res-1", Some("sweep"), |_res| {
            Err(MemoryError::Io(std::io::Error::other("EPERM")))
        });
        assert_eq!(
            get_resource(&conn, "res-1").unwrap().unwrap().state,
            ResourceState::ReclaimFailed
        );

        assert_eq!(
            quarantine_resource(&mut conn, "res-1", "rm keeps failing").unwrap(),
            QuarantineOutcome::Quarantined {
                resource_id: "res-1".to_string()
            }
        );
        let calls = Cell::new(0);
        assert_eq!(
            reclaim_resource(&mut conn, "res-1", None, counting_deleter(&calls, 1)).unwrap(),
            ResourceReclaimOutcome::Quarantined {
                resource_id: "res-1".to_string()
            }
        );
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn release_quarantine_requires_the_clear_to_succeed() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-q", ResourceKind::BuildTarget, "/target/shared"),
        )
        .unwrap();
        record_resource_measurement(&mut conn, "res-q", 4_000_000_000, "").unwrap();
        quarantine_resource(&mut conn, "res-q", "interrupted").unwrap();

        // A failing clear leaves the resource quarantined — the poisoned target
        // must never become reusable because the wipe half-worked.
        let err = release_quarantine(&mut conn, "res-q", |_res| {
            Err(MemoryError::InvalidArg("rm -rf failed".to_string()))
        });
        assert!(err.is_err());
        assert_eq!(
            get_resource(&conn, "res-q").unwrap().unwrap().state,
            ResourceState::Quarantined,
            "a failed clear must not release the quarantine"
        );

        // A successful clear releases it and resets the measurement to 0.
        let out = release_quarantine(&mut conn, "res-q", |res| {
            assert_eq!(res.path, "/target/shared");
            Ok(4_000_000_000)
        })
        .unwrap();
        assert_eq!(
            out,
            ReleaseQuarantineOutcome::Released {
                resource_id: "res-q".to_string(),
                cleared_bytes: 4_000_000_000,
            }
        );
        let got = get_resource(&conn, "res-q").unwrap().unwrap();
        assert_eq!(got.state, ResourceState::Active);
        assert_eq!(got.bytes, Some(0), "a cleared target measures 0 bytes");
        assert!(got.reclaim_reason.is_none());
    }

    #[test]
    fn release_quarantine_does_not_run_the_closure_on_a_healthy_resource() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-ok", ResourceKind::BuildTarget, "/target/ok"),
        )
        .unwrap();
        let ran = Cell::new(false);
        let out = release_quarantine(&mut conn, "res-ok", |_res| {
            ran.set(true);
            Ok(1)
        })
        .unwrap();
        assert_eq!(
            out,
            ReleaseQuarantineOutcome::NotQuarantined {
                resource_id: "res-ok".to_string(),
                state: ResourceState::Active,
            }
        );
        assert!(
            !ran.get(),
            "no filesystem work for a resource that was never quarantined"
        );
    }

    #[test]
    fn release_quarantine_on_unknown_resource_is_not_found() {
        let mut conn = open_conn();
        let ran = Cell::new(false);
        let out = release_quarantine(&mut conn, "ghost", |_res| {
            ran.set(true);
            Ok(0)
        })
        .unwrap();
        assert_eq!(out, ReleaseQuarantineOutcome::NotFound);
        assert!(!ran.get());
    }

    #[test]
    fn reclaim_missing_resource_reports_not_found() {
        let mut conn = open_conn();
        let calls = Cell::new(0);
        let outcome =
            reclaim_resource(&mut conn, "nope", None, counting_deleter(&calls, 1)).unwrap();
        assert_eq!(outcome, ResourceReclaimOutcome::NotFound);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn bind_is_fail_closed_on_unknown_env_unknown_resource_and_dead_resource() {
        let mut conn = open_conn();
        seed_env(&conn, "env-1");
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();

        assert!(
            bind_resource(&mut conn, "ghost-env", "res-1").is_err(),
            "binding an unknown lease must fail closed"
        );
        assert!(
            bind_resource(&mut conn, "env-1", "ghost-res").is_err(),
            "binding an unknown resource must fail closed"
        );

        let calls = Cell::new(0);
        reclaim_resource(&mut conn, "res-1", None, counting_deleter(&calls, 10)).unwrap();
        assert!(
            bind_resource(&mut conn, "env-1", "res-1").is_err(),
            "a lease must not be bound to bytes that are already gone"
        );

        // …and a quarantined resource is not bindable either.
        insert_resource(
            &mut conn,
            &new_resource("res-q", ResourceKind::ScratchDir, "/s/q"),
        )
        .unwrap();
        quarantine_resource(&mut conn, "res-q", "suspect").unwrap();
        assert!(
            bind_resource(&mut conn, "env-1", "res-q").is_err(),
            "a fenced-off resource must not be handed to a lease"
        );
    }

    #[test]
    fn release_binding_on_unknown_pair_is_not_found() {
        let mut conn = open_conn();
        seed_env(&conn, "env-1");
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();
        assert_eq!(
            release_binding(&conn, "env-1", "res-1").unwrap(),
            ReleaseBindingOutcome::NotFound
        );
    }

    #[test]
    fn rebinding_a_released_pair_reactivates_the_same_row() {
        let mut conn = open_conn();
        seed_env(&conn, "env-1");
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::BuildTarget, "/t"),
        )
        .unwrap();
        let first = bind_resource(&mut conn, "env-1", "res-1").unwrap();
        let BindOutcome::Bound { binding_id } = first else {
            panic!("expected a fresh binding");
        };
        release_binding(&conn, "env-1", "res-1").unwrap();
        assert_eq!(active_binding_count(&conn, "res-1").unwrap(), 0);

        let again = bind_resource(&mut conn, "env-1", "res-1").unwrap();
        assert_eq!(again, BindOutcome::Bound { binding_id });
        assert_eq!(active_binding_count(&conn, "res-1").unwrap(), 1);
    }

    #[test]
    fn list_filters_by_state_and_kind() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-w", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();
        insert_resource(
            &mut conn,
            &new_resource("res-t", ResourceKind::BuildTarget, "/t"),
        )
        .unwrap();
        insert_resource(
            &mut conn,
            &new_resource("res-s", ResourceKind::ScratchDir, "/s"),
        )
        .unwrap();
        let calls = Cell::new(0);
        reclaim_resource(&mut conn, "res-s", None, counting_deleter(&calls, 1)).unwrap();

        let active = list_resources(&conn, Some(ResourceState::Active), None).unwrap();
        assert_eq!(active.len(), 2);
        let reclaimed = list_resources(&conn, Some(ResourceState::Reclaimed), None).unwrap();
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].resource_id, "res-s");

        let targets = list_resources(&conn, None, Some(ResourceKind::BuildTarget)).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].resource_id, "res-t");

        let active_targets = list_resources(
            &conn,
            Some(ResourceState::Active),
            Some(ResourceKind::BuildTarget),
        )
        .unwrap();
        assert_eq!(active_targets.len(), 1);
        assert_eq!(list_resources(&conn, None, None).unwrap().len(), 3);
    }

    #[test]
    fn measurement_is_stored_not_computed() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::BuildTarget, "/t"),
        )
        .unwrap();
        assert!(get_resource(&conn, "res-1")
            .unwrap()
            .unwrap()
            .bytes
            .is_none());

        record_resource_measurement(&mut conn, "res-1", 42_000, "").unwrap();
        let got = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(got.bytes, Some(42_000));
        assert!(got.measured_at.is_some());

        assert!(
            record_resource_measurement(&mut conn, "ghost", 1, "").is_err(),
            "measuring an unknown resource must not silently create one"
        );
    }

    // Review round 2, item 6 — a measurement is gated on state. Stamping fresh
    // bytes onto a row whose bytes are already freed would make released disk look
    // occupied forever (the mirror image of the #1029 lie).
    #[test]
    fn measurement_is_refused_once_the_bytes_are_gone_or_fenced() {
        let mut conn = open_conn();
        insert_resource(
            &mut conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();
        record_resource_measurement(&mut conn, "res-1", 900, "").unwrap();

        let calls = Cell::new(0);
        reclaim_resource(&mut conn, "res-1", None, counting_deleter(&calls, 900)).unwrap();
        assert!(
            record_resource_measurement(&mut conn, "res-1", 5_000_000, "").is_err(),
            "a reclaimed resource must not accept a new measurement"
        );
        let got = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(
            got.bytes,
            Some(900),
            "the pre-reclaim measurement is untouched"
        );

        // Fenced-off resources are equally off-limits.
        insert_resource(
            &mut conn,
            &new_resource("res-q", ResourceKind::ScratchDir, "/s/q"),
        )
        .unwrap();
        quarantine_resource(&mut conn, "res-q", "suspect").unwrap();
        assert!(record_resource_measurement(&mut conn, "res-q", 1, "").is_err());
        assert!(get_resource(&conn, "res-q")
            .unwrap()
            .unwrap()
            .bytes
            .is_none());

        // …but a resource that is being reclaimed right now IS measurable: that is
        // exactly when a deleter measures what it is about to remove.
        insert_resource(
            &mut conn,
            &new_resource("res-x", ResourceKind::ScratchDir, "/s/x"),
        )
        .unwrap();
        conn.execute(
            "UPDATE exec_env_resources SET state = 'reclaiming' WHERE resource_id = 'res-x'",
            [],
        )
        .unwrap();
        record_resource_measurement(&mut conn, "res-x", 777, "").unwrap();
        assert_eq!(
            get_resource(&conn, "res-x").unwrap().unwrap().bytes,
            Some(777)
        );
    }

    #[test]
    fn state_and_kind_parse_reject_unknown() {
        assert!(ResourceState::parse("gone").is_err());
        assert_eq!(
            ResourceState::parse("reclaiming").unwrap(),
            ResourceState::Reclaiming
        );
        assert!(ResourceKind::parse("cache").is_err());
        assert_eq!(
            ResourceKind::parse("build_target").unwrap(),
            ResourceKind::BuildTarget
        );
    }
}

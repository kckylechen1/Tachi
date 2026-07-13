//! Execution-environment resource ledger (#894 S2a) — the BYTES behind a lease.
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
//!    silent skip that would let the caller believe it freed something.
//! 3. **Single writer.** Every state transition of a resource row goes through
//!    [`reclaim_resource`] (mirroring `reclaim_exec_env`); nothing else writes
//!    `state`.
//! 4. **refcount = live bindings.** A shared resource is only reclaimable after
//!    the *last* binding is released.
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

use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::error::MemoryError;

use super::common::normalize_utc_iso_or_now;

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
///                          │  ▲
///                delete err│  │ re-enter (crash or retry)
///                          ▼  │
///                       reclaim_failed
///
///   quarantined ── (no automatic transition: a human/broker owns it)
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
    /// someone deliberately fenced off.
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

/// Register a physical resource. `(path, kind)` is UNIQUE: registering the same
/// path twice is an error, never a second row — two rows for one directory
/// would split its refcount and let a live worktree be deleted.
pub fn insert_resource(conn: &Connection, res: &NewExecEnvResource) -> Result<(), MemoryError> {
    let now = normalize_utc_iso_or_now(&res.created_at);
    let measured_at = res.bytes.map(|_| now.clone());
    conn.execute(
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
    Ok(())
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

/// Record a fresh measurement. memcore does not walk the filesystem — the
/// caller measures, this only stores the number and when it was taken.
pub fn record_resource_measurement(
    conn: &Connection,
    resource_id: &str,
    bytes: i64,
    measured_at: &str,
) -> Result<(), MemoryError> {
    let now = normalize_utc_iso_or_now(measured_at);
    let changed = conn.execute(
        "UPDATE exec_env_resources SET bytes = ?2, measured_at = ?3, updated_at = ?3 \
         WHERE resource_id = ?1",
        params![resource_id, bytes, now],
    )?;
    if changed == 0 {
        return Err(MemoryError::NotFound(format!(
            "exec_env_resource '{resource_id}'"
        )));
    }
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

/// Bind a lease to a resource (refcount +1, unless this pair is already bound).
///
/// Fail-closed on both ends: the lease must exist in `exec_envs` and the
/// resource must be `active`. Binding a lease to a resource whose bytes are
/// being (or have been) freed would hand that lease a path to nothing.
pub fn bind_resource(
    conn: &Connection,
    env_id: &str,
    resource_id: &str,
) -> Result<BindOutcome, MemoryError> {
    let env_exists: bool = conn
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

    let resource = get_resource(conn, resource_id)?
        .ok_or_else(|| MemoryError::NotFound(format!("exec_env_resource '{resource_id}'")))?;
    if resource.state != ResourceState::Active {
        return Err(MemoryError::InvalidArg(format!(
            "exec_env_resource '{resource_id}' is '{}', only 'active' resources are bindable",
            resource.state.as_str()
        )));
    }

    let existing: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT binding_id, released_at FROM exec_env_resource_bindings \
             WHERE env_id = ?1 AND resource_id = ?2",
            params![env_id, resource_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let now = normalize_utc_iso_or_now("");
    match existing {
        // Already live: a re-bind is a no-op, so a retrying caller cannot
        // inflate the refcount.
        Some((binding_id, None)) => Ok(BindOutcome::AlreadyBound { binding_id }),
        // Released earlier: re-activate the same row (UNIQUE(env_id,
        // resource_id) keeps one row per pair by design).
        Some((binding_id, Some(_))) => {
            conn.execute(
                "UPDATE exec_env_resource_bindings SET released_at = NULL, created_at = ?2 \
                 WHERE binding_id = ?1",
                params![binding_id, now],
            )?;
            Ok(BindOutcome::Bound { binding_id })
        }
        None => {
            let binding_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO exec_env_resource_bindings
                 (binding_id, env_id, resource_id, created_at, released_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![binding_id, env_id, resource_id, now],
            )?;
            Ok(BindOutcome::Bound { binding_id })
        }
    }
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
            conn.execute(
                "UPDATE exec_env_resource_bindings SET released_at = ?2 \
                 WHERE binding_id = ?1 AND released_at IS NULL",
                params![binding_id, now],
            )?;
            Ok(ReleaseBindingOutcome::Released { binding_id })
        }
    }
}

/// THE single reclaim path for a resource (#894 S2a) — and the only writer of
/// `exec_env_resources.state`.
///
/// `delete_bytes` is the caller's filesystem deleter: it receives the resource
/// row (as read *before* the `reclaiming` stamp — `path`/`kind` are what a
/// deleter needs) and returns how many bytes it actually freed. memcore never
/// touches the filesystem; taking the deleter as a closure is what makes the
/// frozen ordering unforgeable at the call site:
///
/// 1. refuse if a binding is live ([`ResourceReclaimOutcome::BlockedByBinding`])
///    or the resource is quarantined — no state change, no deletion;
/// 2. commit `state = 'reclaiming'` — a crash from here on leaves that state,
///    which a later call re-enters;
/// 3. run `delete_bytes` (must be idempotent: an already-deleted path frees 0);
/// 4. commit `reclaimed_bytes` + `state = 'reclaimed'`.
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
    // Phase 1 — decide and claim, transactionally, so a concurrent reclaim of
    // the same resource can't run the deleter twice in parallel.
    let tx = conn.transaction()?;
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
            conn.execute(
                "UPDATE exec_env_resources SET state = 'reclaim_failed', updated_at = ?2 \
                 WHERE resource_id = ?1 AND state = 'reclaiming'",
                params![resource_id, now],
            )?;
            return Err(err);
        }
    };

    // Phase 3 — the bytes are gone; record how many. Assignment, not
    // accumulation: a re-entered reclaim overwrites with what IT freed rather
    // than adding to a previous attempt's number. The `state = 'reclaiming'`
    // guard is what keeps a *concurrent* re-entrant reclaim (which would free 0,
    // the winner having already deleted the path) from clobbering the winner's
    // `reclaimed_bytes` with a zero.
    let now = normalize_utc_iso_or_now("");
    conn.execute(
        "UPDATE exec_env_resources \
         SET state = 'reclaimed', reclaimed_at = ?2, reclaimed_bytes = ?3, updated_at = ?2 \
         WHERE resource_id = ?1 AND state = 'reclaiming'",
        params![resource_id, now, freed],
    )?;
    Ok(ResourceReclaimOutcome::Reclaimed {
        resource_id: resource.resource_id,
        reclaimed_bytes: freed,
    })
}

/// Result of [`quarantine_resource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineOutcome {
    /// The resource is now quarantined.
    Quarantined { resource_id: String },
    /// It was already quarantined; nothing changed (idempotent).
    AlreadyQuarantined { resource_id: String },
    /// Refused: the bytes are already gone (`reclaimed`), so there is nothing
    /// to fence off. Typed rather than silently "succeeding" — a caller that
    /// believes it quarantined a live target when it actually pointed at a
    /// reclaimed row would go on to reuse a path that no longer exists.
    AlreadyReclaimed { resource_id: String },
    /// No such resource.
    NotFound,
}

/// Fence a resource off from automatic reclaim AND from reuse (#894 S2c item
/// 4): the *only* writer of `state = 'quarantined'`.
///
/// The build broker calls this when a `cargo` invocation was interrupted (a
/// signal / kill / daemon crash mid-build): the target dir it was writing into
/// is now in an unknown state — half-written fingerprints, truncated rlibs —
/// and reusing it is exactly how a "phantom compile error" (symbol greppable in
/// the source, reported as `not found` by rustc) gets manufactured. Quarantined
/// resources are refused by [`reclaim_resource`] and are unbindable by
/// [`bind_resource`] (only `active` resources bind), so a quarantined target
/// cannot be silently picked up by the next build; the only way out is
/// [`release_quarantine`], which forces the caller to verify or clear the bytes
/// first.
///
/// `reclaim_resource` remains the only path to `reclaiming`/`reclaimed`; this
/// function and [`release_quarantine`] own the quarantine edge and nothing
/// else. Splitting the edges (rather than exposing a generic state setter) is
/// what keeps "single writer per transition" true.
pub fn quarantine_resource(
    conn: &mut Connection,
    resource_id: &str,
    reason: &str,
) -> Result<QuarantineOutcome, MemoryError> {
    let tx = conn.transaction()?;
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT resource_id, state FROM exec_env_resources WHERE resource_id = ?1",
            params![resource_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let outcome = match existing {
        None => QuarantineOutcome::NotFound,
        Some((resource_id, state_raw)) => match ResourceState::parse(&state_raw)? {
            ResourceState::Quarantined => QuarantineOutcome::AlreadyQuarantined { resource_id },
            ResourceState::Reclaimed => QuarantineOutcome::AlreadyReclaimed { resource_id },
            // active / reclaiming / reclaim_failed all fence off: an
            // interrupted reclaim leaves bytes in an unknown state too.
            ResourceState::Active | ResourceState::Reclaiming | ResourceState::ReclaimFailed => {
                let now = normalize_utc_iso_or_now("");
                tx.execute(
                    "UPDATE exec_env_resources \
                     SET state = 'quarantined', reclaim_reason = ?2, updated_at = ?3 \
                     WHERE resource_id = ?1",
                    params![resource_id, reason, now],
                )?;
                QuarantineOutcome::Quarantined { resource_id }
            }
        },
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

/// The only exit from `quarantined`, and the only writer of the
/// quarantined -> active edge (#894 S2c item 4: "an interrupted target must be
/// verified or cleared before a retry may touch it").
///
/// `verify_or_clear` is the caller's filesystem work — wipe the target dir (and
/// return the bytes freed), or verify it in place (and return 0). Taking it as
/// a closure is the same trick [`reclaim_resource`] uses: the ordering cannot
/// be gotten wrong at a call site, because there is no way to reach `active`
/// without having run it. If it errors, the resource STAYS quarantined and the
/// error propagates — a failed clear must never hand the next build a poisoned
/// target.
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
    if cleared > 0 {
        conn.execute(
            "UPDATE exec_env_resources \
             SET state = 'active', reclaim_reason = NULL, bytes = 0, measured_at = ?2, \
                 updated_at = ?2 \
             WHERE resource_id = ?1 AND state = 'quarantined'",
            params![resource_id, now],
        )?;
    } else {
        conn.execute(
            "UPDATE exec_env_resources \
             SET state = 'active', reclaim_reason = NULL, updated_at = ?2 \
             WHERE resource_id = ?1 AND state = 'quarantined'",
            params![resource_id, now],
        )?;
    }
    Ok(ReleaseQuarantineOutcome::Released {
        resource_id: resource.resource_id,
        cleared_bytes: cleared,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::db::exec_env::{insert_exec_env, NewExecEnvLease};

    fn open_conn() -> Connection {
        // Same raw-connection fixture as `exec_env`'s tests: schema init
        // creates FTS tables needing the registered simple tokenizer.
        libsimple::enable_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
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
                env_class: crate::db::exec_env::EnvClass::EditOnly,
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
        let conn = open_conn();
        insert_resource(
            &conn,
            &NewExecEnvResource {
                resource_id: "res-1".to_string(),
                kind: ResourceKind::BuildTarget,
                path: "/cache/sigil-shared-target".to_string(),
                bytes: Some(4096),
                created_at: String::new(),
            },
        )
        .unwrap();
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
        let conn = open_conn();
        insert_resource(
            &conn,
            &new_resource("res-a", ResourceKind::Worktree, "/wt/x"),
        )
        .unwrap();
        let err = insert_resource(
            &conn,
            &new_resource("res-b", ResourceKind::Worktree, "/wt/x"),
        );
        assert!(err.is_err(), "duplicate (path, kind) must be rejected");

        // A different kind at the same path is a different physical resource
        // (e.g. a project DB inside a worktree) and is allowed.
        insert_resource(
            &conn,
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

    // Discriminating test ① — invariant 2: live binding ⇒ not reclaimable, and
    // the refusal is TYPED (a silent skip is exactly the #1029 bug).
    #[test]
    fn reclaim_is_blocked_while_a_binding_is_live() {
        let mut conn = open_conn();
        seed_env(&conn, "env-1");
        insert_resource(
            &conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();
        bind_resource(&conn, "env-1", "res-1").unwrap();

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
            &conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();
        bind_resource(&conn, "env-1", "res-1").unwrap();

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
            &conn,
            &new_resource(
                "res-target",
                ResourceKind::BuildTarget,
                "/cache/sigil-shared-target",
            ),
        )
        .unwrap();
        bind_resource(&conn, "env-1", "res-target").unwrap();
        bind_resource(&conn, "env-2", "res-target").unwrap();
        assert_eq!(active_binding_count(&conn, "res-target").unwrap(), 2);

        // A re-bind of a live pair must not inflate the refcount.
        assert!(matches!(
            bind_resource(&conn, "env-2", "res-target").unwrap(),
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

    // Discriminating test ④ — crash mid-`reclaiming` is re-enterable, and the
    // retry does not double-count bytes.
    #[test]
    fn crash_mid_reclaiming_is_reentrant_and_does_not_double_count() {
        let mut conn = open_conn();
        insert_resource(
            &conn,
            &new_resource("res-1", ResourceKind::ScratchDir, "/scratch/1"),
        )
        .unwrap();

        // The deleter dies (process killed / rm failed) after `reclaiming` was
        // committed. Reaching `reclaim_failed` (not `reclaimed`) is the point:
        // bytes were never confirmed freed, so the row must not claim they were.
        let err = reclaim_resource(&mut conn, "res-1", Some("sweep"), |_res| {
            Err(MemoryError::Io(std::io::Error::other("rm -rf died")))
        })
        .unwrap_err();
        assert!(err.to_string().contains("rm -rf died"));
        let mid = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_ne!(
            mid.state,
            ResourceState::Reclaimed,
            "a failed delete must never report reclaimed"
        );
        assert!(
            mid.reclaimed_bytes.is_none(),
            "no bytes claimed for a failed delete"
        );

        // A hard crash (process death) between the two commits leaves exactly
        // `reclaiming`; simulate that persisted state directly, then prove a
        // later call re-enters it rather than getting stuck.
        conn.execute(
            "UPDATE exec_env_resources SET state = 'reclaiming' WHERE resource_id = ?1",
            params!["res-1"],
        )
        .unwrap();

        let calls = Cell::new(0);
        // The re-entered deleter finds a partially deleted dir and frees what's
        // left (100), not the original total.
        let outcome =
            reclaim_resource(&mut conn, "res-1", None, counting_deleter(&calls, 100)).unwrap();
        assert_eq!(
            outcome,
            ResourceReclaimOutcome::Reclaimed {
                resource_id: "res-1".to_string(),
                reclaimed_bytes: 100,
            }
        );
        assert_eq!(calls.get(), 1);
        let done = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(done.state, ResourceState::Reclaimed);
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
        assert_eq!(calls.get(), 1);
        assert_eq!(
            get_resource(&conn, "res-1")
                .unwrap()
                .unwrap()
                .reclaimed_bytes,
            Some(100)
        );
    }

    // Invariant 1, ordering: `reclaiming` is COMMITTED before the deleter runs.
    // If the whole reclaim lived in one transaction, a second connection would
    // still see `active` here and this assertion would fail — which is what
    // makes a crash mid-delete recoverable at all.
    #[test]
    fn reclaiming_is_committed_before_the_deleter_runs() {
        libsimple::enable_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("resources.db");
        let mut conn = Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        insert_resource(
            &conn,
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

    #[test]
    fn quarantined_resource_is_refused_not_freed() {
        let mut conn = open_conn();
        insert_resource(
            &conn,
            &new_resource("res-q", ResourceKind::ScratchDir, "/scratch/q"),
        )
        .unwrap();
        conn.execute(
            "UPDATE exec_env_resources SET state = 'quarantined' WHERE resource_id = ?1",
            params!["res-q"],
        )
        .unwrap();

        let calls = Cell::new(0);
        let outcome = reclaim_resource(
            &mut conn,
            "res-q",
            Some("sweep"),
            counting_deleter(&calls, 1),
        )
        .unwrap();
        assert_eq!(
            outcome,
            ResourceReclaimOutcome::Quarantined {
                resource_id: "res-q".to_string()
            }
        );
        assert_eq!(calls.get(), 0, "quarantined bytes must never be deleted");
        assert_eq!(
            get_resource(&conn, "res-q").unwrap().unwrap().state,
            ResourceState::Quarantined
        );
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
            &conn,
            &new_resource("res-1", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();

        assert!(
            bind_resource(&conn, "ghost-env", "res-1").is_err(),
            "binding an unknown lease must fail closed"
        );
        assert!(
            bind_resource(&conn, "env-1", "ghost-res").is_err(),
            "binding an unknown resource must fail closed"
        );

        let calls = Cell::new(0);
        reclaim_resource(&mut conn, "res-1", None, counting_deleter(&calls, 10)).unwrap();
        assert!(
            bind_resource(&conn, "env-1", "res-1").is_err(),
            "a lease must not be bound to bytes that are already gone"
        );
    }

    #[test]
    fn release_binding_on_unknown_pair_is_not_found() {
        let conn = open_conn();
        seed_env(&conn, "env-1");
        insert_resource(
            &conn,
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
        let conn = open_conn();
        seed_env(&conn, "env-1");
        insert_resource(
            &conn,
            &new_resource("res-1", ResourceKind::BuildTarget, "/t"),
        )
        .unwrap();
        let first = bind_resource(&conn, "env-1", "res-1").unwrap();
        let BindOutcome::Bound { binding_id } = first else {
            panic!("expected a fresh binding");
        };
        release_binding(&conn, "env-1", "res-1").unwrap();
        assert_eq!(active_binding_count(&conn, "res-1").unwrap(), 0);

        let again = bind_resource(&conn, "env-1", "res-1").unwrap();
        assert_eq!(again, BindOutcome::Bound { binding_id });
        assert_eq!(active_binding_count(&conn, "res-1").unwrap(), 1);
    }

    #[test]
    fn list_filters_by_state_and_kind() {
        let mut conn = open_conn();
        insert_resource(
            &conn,
            &new_resource("res-w", ResourceKind::Worktree, "/wt/1"),
        )
        .unwrap();
        insert_resource(
            &conn,
            &new_resource("res-t", ResourceKind::BuildTarget, "/t"),
        )
        .unwrap();
        insert_resource(
            &conn,
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
        let conn = open_conn();
        insert_resource(
            &conn,
            &new_resource("res-1", ResourceKind::BuildTarget, "/t"),
        )
        .unwrap();
        assert!(get_resource(&conn, "res-1")
            .unwrap()
            .unwrap()
            .bytes
            .is_none());

        record_resource_measurement(&conn, "res-1", 42_000, "").unwrap();
        let got = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(got.bytes, Some(42_000));
        assert!(got.measured_at.is_some());

        assert!(
            record_resource_measurement(&conn, "ghost", 1, "").is_err(),
            "measuring an unknown resource must not silently create one"
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

    // ─── quarantine edge (#894 S2c item 4) ──────────────────────────────────

    #[test]
    fn quarantined_target_is_unbindable_and_unreclaimable() {
        let mut conn = open_conn();
        seed_env(&conn, "env-q");
        insert_resource(
            &conn,
            &new_resource("res-q", ResourceKind::BuildTarget, "/target/shared"),
        )
        .unwrap();

        assert_eq!(
            quarantine_resource(&mut conn, "res-q", "interrupted cargo").unwrap(),
            QuarantineOutcome::Quarantined {
                resource_id: "res-q".to_string()
            }
        );
        let got = get_resource(&conn, "res-q").unwrap().unwrap();
        assert_eq!(got.state, ResourceState::Quarantined);
        assert_eq!(got.reclaim_reason.as_deref(), Some("interrupted cargo"));

        // A quarantined target cannot be handed to a new lease...
        assert!(
            bind_resource(&conn, "env-q", "res-q").is_err(),
            "a quarantined target must not be bindable — that is how the next \
             build would silently reuse a poisoned target dir"
        );
        // ...and automatic reclaim refuses it too (a human/broker owns it).
        let calls = Cell::new(0);
        assert_eq!(
            reclaim_resource(&mut conn, "res-q", None, counting_deleter(&calls, 9)).unwrap(),
            ResourceReclaimOutcome::Quarantined {
                resource_id: "res-q".to_string()
            }
        );
        assert_eq!(calls.get(), 0, "deleter must not run on a quarantined row");
    }

    #[test]
    fn quarantine_is_idempotent_and_refuses_a_reclaimed_row() {
        let mut conn = open_conn();
        insert_resource(
            &conn,
            &new_resource("res-1", ResourceKind::BuildTarget, "/target/a"),
        )
        .unwrap();
        quarantine_resource(&mut conn, "res-1", "first").unwrap();
        assert_eq!(
            quarantine_resource(&mut conn, "res-1", "second").unwrap(),
            QuarantineOutcome::AlreadyQuarantined {
                resource_id: "res-1".to_string()
            }
        );
        // The idempotent no-op must not overwrite the original reason.
        let got = get_resource(&conn, "res-1").unwrap().unwrap();
        assert_eq!(got.reclaim_reason.as_deref(), Some("first"));

        // Already-reclaimed bytes cannot be "quarantined" — there is nothing there.
        insert_resource(
            &conn,
            &new_resource("res-2", ResourceKind::ScratchDir, "/scratch/b"),
        )
        .unwrap();
        let calls = Cell::new(0);
        reclaim_resource(&mut conn, "res-2", None, counting_deleter(&calls, 4)).unwrap();
        assert_eq!(
            quarantine_resource(&mut conn, "res-2", "late").unwrap(),
            QuarantineOutcome::AlreadyReclaimed {
                resource_id: "res-2".to_string()
            }
        );

        assert_eq!(
            quarantine_resource(&mut conn, "ghost", "x").unwrap(),
            QuarantineOutcome::NotFound
        );
    }

    #[test]
    fn release_quarantine_requires_the_clear_to_succeed() {
        let mut conn = open_conn();
        insert_resource(
            &conn,
            &new_resource("res-q", ResourceKind::BuildTarget, "/target/shared"),
        )
        .unwrap();
        record_resource_measurement(&conn, "res-q", 4_000_000_000, "").unwrap();
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
            &conn,
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
}

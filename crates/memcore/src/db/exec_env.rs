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
//!   active ──reclaim──▶ reclaimed
//!     ▲                     │
//!     └── (no transition) ◀─┘   reclaiming an already-reclaimed lease is an
//!                               idempotent no-op, never an error.
//! ```
//!
//! There is exactly one function that flips a lease to `reclaimed`
//! ([`reclaim_exec_env`]); `safe_merge` / cancel / terminal-state all route
//! through it, and the sweep is a backstop, not a second writer.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;

use super::common::normalize_utc_iso_or_now;

/// Lease lifecycle state. S1 persists exactly two states; the fuller
/// provisioned→leased→terminal ladder from the design doc is deferred to a
/// later slice and would extend this enum additively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecEnvState {
    /// Provisioned and in use — the worktree exists and the lease owns it.
    Active,
    /// Reclaimed — the worktree/branch/target have been (or are being) torn
    /// down; the row is retained for audit and idempotent reclaim.
    Reclaimed,
}

impl ExecEnvState {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecEnvState::Active => "active",
            ExecEnvState::Reclaimed => "reclaimed",
        }
    }

    /// Parse a persisted state string. An unknown/legacy value is an error the
    /// caller must surface (fail-closed): a corrupted state must never silently
    /// masquerade as `active` (usable) or `reclaimed` (torn down).
    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "active" => Ok(ExecEnvState::Active),
            "reclaimed" => Ok(ExecEnvState::Reclaimed),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown exec_env state '{other}' (expected 'active' or 'reclaimed')"
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
    /// the serialized executor seat and binds (refcounted) to the seat's
    /// resident target. It still gets no target dir of its own.
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

    /// Whether provisioning allocates a `build_target` resource for this class.
    /// `EditOnly` is the only class that gets none — that is the whole point of
    /// it.
    pub fn allocates_build_target(self) -> bool {
        !matches!(self, EnvClass::EditOnly)
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

const SELECT_COLUMNS: &str = "env_id, kind, path, repo_root, branch, base_sha, dispatch_id, \
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
    let class_raw: String = row.get(7)?;
    let env_class = EnvClass::parse(&class_raw).map_err(|e| conv_err(7, e))?;
    let state_raw: String = row.get(8)?;
    let state = ExecEnvState::parse(&state_raw).map_err(|e| conv_err(8, e))?;
    Ok(ExecEnvLease {
        env_id: row.get(0)?,
        kind: row.get(1)?,
        path: row.get(2)?,
        repo_root: row.get(3)?,
        branch: row.get(4)?,
        base_sha: row.get(5)?,
        dispatch_id: row.get(6)?,
        env_class,
        state,
        reclaim_reason: row.get(9)?,
        schema_version: row.get(10)?,
        created_at: row.get(11)?,
        reclaimed_at: row.get(12)?,
    })
}

/// Insert a new lease. Fails if `env_id` already exists — a lease id collision
/// is a bug, never a silent overwrite that could orphan the prior worktree.
pub fn insert_exec_env(conn: &Connection, lease: &NewExecEnvLease) -> Result<(), MemoryError> {
    let created_at = if lease.created_at.trim().is_empty() {
        normalize_utc_iso_or_now("")
    } else {
        normalize_utc_iso_or_now(&lease.created_at)
    };
    conn.execute(
        "INSERT INTO exec_envs
         (env_id, kind, path, repo_root, branch, base_sha, dispatch_id,
          env_class, state, reclaim_reason, schema_version, created_at, reclaimed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', NULL, 1, ?9, NULL)",
        params![
            lease.env_id,
            lease.kind,
            lease.path,
            lease.repo_root,
            lease.branch,
            lease.base_sha,
            lease.dispatch_id,
            lease.env_class.as_str(),
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

/// THE single reclaim path (#894 S1): transactionally flip an `active` lease to
/// `reclaimed`, stamping `reclaimed_at` and an optional reason. Idempotent — a
/// lease that is already `reclaimed` returns [`ReclaimOutcome::AlreadyReclaimed`]
/// without a second write, and a missing lease returns
/// [`ReclaimOutcome::NotFound`]. safe_merge / cancel / terminal-state must all
/// call through here; the sweep is a backstop that never owns this transition.
pub fn reclaim_exec_env(
    conn: &mut Connection,
    selector: &ExecEnvSelector,
    reason: Option<&str>,
) -> Result<ReclaimOutcome, MemoryError> {
    let tx = conn.transaction()?;
    // Resolve the target row inside the transaction so the read-then-write is
    // atomic against a concurrent reclaim of the same lease.
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

    let outcome = match existing {
        None => ReclaimOutcome::NotFound,
        Some((env_id, state_raw)) => match ExecEnvState::parse(&state_raw)? {
            ExecEnvState::Reclaimed => ReclaimOutcome::AlreadyReclaimed { env_id },
            ExecEnvState::Active => {
                let now = normalize_utc_iso_or_now("");
                tx.execute(
                    "UPDATE exec_envs SET state = 'reclaimed', reclaimed_at = ?2, \
                     reclaim_reason = ?3 WHERE env_id = ?1 AND state = 'active'",
                    params![env_id, now, reason],
                )?;
                ReclaimOutcome::Reclaimed { env_id }
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
        // Keep this raw-connection fixture equivalent to MemoryStore's open
        // path: schema initialization creates FTS tables that require the
        // registered simple tokenizer.
        libsimple::enable_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
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
        let outcome =
            reclaim_exec_env(&mut conn, &ExecEnvSelector::EnvId("nope".to_string()), None).unwrap();
        assert_eq!(outcome, ReclaimOutcome::NotFound);
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
        assert!(got.env_class.allocates_build_target());
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

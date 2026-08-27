//! Persisted WorkClaim holder evidence for destructive worktree cleanup.
//!
//! OS process evidence remains the independent liveness gate in `holder`.
//! Planning reads the durable ExecEnv/WorkClaim ledger. Immediately before a
//! destructive command, execution atomically claims the lease as `removing`
//! so dispatch admission cannot win a check-then-delete race.

use std::path::{Path, PathBuf};

/// The persisted counterpart to the OS holder probe.  Reasons are retained
/// for failures so a refusal cannot be rendered as a successful no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbHolderEvidence {
    Clear,
    NotApplicable,
    Held,
    Contradictory,
    Unavailable(String),
    Unverifiable(String),
}

impl DbHolderEvidence {
    pub fn refusal_reason(&self) -> Option<String> {
        match self {
            Self::Clear | Self::NotApplicable => None,
            Self::Held => Some("persisted WorkClaim holder evidence is Held".to_string()),
            Self::Contradictory => {
                Some("persisted WorkClaim holder evidence is Contradictory".to_string())
            }
            Self::Unavailable(reason) => Some(format!(
                "persisted WorkClaim holder evidence is Unavailable: {reason}"
            )),
            Self::Unverifiable(reason) => Some(format!(
                "persisted WorkClaim holder evidence is Unverifiable: {reason}"
            )),
        }
    }
}

pub type DbHolderProbeFn = dyn Fn(&Path) -> DbHolderEvidence;

/// Persisted ownership of a managed worktree removal. A legacy worktree has
/// no env id and the completion methods are no-ops.
pub struct DbRemovalClaim {
    db_path: PathBuf,
    env_id: Option<String>,
}

#[cfg(test)]
pub(crate) fn legacy_removal_claim_for_test() -> DbRemovalClaim {
    DbRemovalClaim {
        db_path: PathBuf::new(),
        env_id: None,
    }
}

impl DbRemovalClaim {
    pub fn complete(self, reclaimed_bytes: i64) -> Result<(), String> {
        let Some(env_id) = self.env_id else {
            return Ok(());
        };
        let mut store = open_maintenance_store(&self.db_path)?;
        memcore::complete_exec_env_removal(
            store.connection_mut(),
            &env_id,
            Some("worktree removed"),
            reclaimed_bytes,
        )
        .map_err(|error| format!("complete ExecEnv removal claim: {error}"))
    }

    pub fn abort(self) -> Result<(), String> {
        let Some(env_id) = self.env_id else {
            return Ok(());
        };
        let mut store = open_maintenance_store(&self.db_path)?;
        memcore::abort_exec_env_removal(store.connection_mut(), &env_id)
            .map_err(|error| format!("abort ExecEnv removal claim: {error}"))
    }
}

/// Measure bytes before deletion so resource-ledger completion records the
/// physical amount removed instead of inventing a successful zero.
pub fn measure_worktree_bytes(path: &Path) -> Result<i64, String> {
    fn visit(path: &Path) -> Result<u64, String> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("measure {}: {error}", path.display()))?;
        if !metadata.is_dir() {
            return Ok(metadata.len());
        }
        let mut total = 0_u64;
        for entry in std::fs::read_dir(path)
            .map_err(|error| format!("measure directory {}: {error}", path.display()))?
        {
            let entry = entry
                .map_err(|error| format!("measure directory entry {}: {error}", path.display()))?;
            total = total
                .checked_add(visit(&entry.path())?)
                .ok_or_else(|| format!("worktree byte count overflow at {}", path.display()))?;
        }
        Ok(total)
    }

    let bytes = visit(path)?;
    i64::try_from(bytes).map_err(|_| format!("worktree byte count exceeds i64: {bytes}"))
}

/// Read holder evidence from the configured Tachi global DB.  The cleaner
/// shares the runtime's `TACHI_HOME` layout and canonical database filename,
/// never creates or migrates a DB while deciding whether deletion is safe.
pub fn probe_worktree_holder(worktree: &Path) -> DbHolderEvidence {
    let home = resolve_tachi_home();
    probe_worktree_holder_from_home(&home, worktree)
}

/// Atomically acquire destructive ownership immediately before deletion.
pub fn claim_worktree_removal(worktree: &Path) -> Result<DbRemovalClaim, String> {
    let home = resolve_tachi_home();
    claim_worktree_removal_from_home(&home, worktree)
}

fn claim_worktree_removal_from_home(
    home: &Path,
    worktree: &Path,
) -> Result<DbRemovalClaim, String> {
    let canonical = std::fs::canonicalize(worktree)
        .map_err(|error| format!("canonicalize worktree before removal claim: {error}"))?;
    let path = canonical
        .to_str()
        .ok_or_else(|| "worktree path is not valid UTF-8".to_string())?;
    let db_path = configured_global_db(home)?;
    let mut store = open_maintenance_store(&db_path)?;
    let env_id = memcore::claim_exec_env_removal(store.connection_mut(), path)
        .map_err(|error| format!("claim ExecEnv removal: {error}"))?;
    Ok(DbRemovalClaim { db_path, env_id })
}

fn open_maintenance_store(db_path: &Path) -> Result<memcore::MemoryStore, String> {
    let path = db_path
        .to_str()
        .ok_or_else(|| "configured global DB path is not valid UTF-8".to_string())?;
    memcore::MemoryStore::open_existing_read_write(path)
        .map_err(|error| format!("open global DB for removal claim: {error}"))
}

fn resolve_tachi_home() -> PathBuf {
    if let Some(home) = std::env::var_os("TACHI_HOME") {
        return PathBuf::from(home);
    }
    if let Some(home) = std::env::var_os("SIGIL_HOME") {
        return PathBuf::from(home);
    }
    if let Some(home) = std::env::var_os("TACHI_APP_HOME") {
        return PathBuf::from(home);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".tachi"))
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn probe_worktree_holder_from_home(home: &Path, worktree: &Path) -> DbHolderEvidence {
    let db = match configured_global_db(home) {
        Ok(path) => path,
        Err(error) => return DbHolderEvidence::Unavailable(error),
    };
    probe_worktree_holder_at_db(&db, worktree)
}

#[cfg(not(test))]
fn probe_worktree_holder_from_home(home: &Path, worktree: &Path) -> DbHolderEvidence {
    let db = match configured_global_db(home) {
        Ok(path) => path,
        Err(error) => return DbHolderEvidence::Unavailable(error),
    };
    probe_worktree_holder_at_db(&db, worktree)
}

fn configured_global_db(home: &Path) -> Result<PathBuf, String> {
    // The cleaner cannot run the write-side filename migration. Reuse
    // memcore's read-only split-brain and compat-link resolver so it may read
    // a legacy-only DB without silently choosing one of two real databases.
    memcore::resolve_memory_db_read_path(&home.join("global").join(memcore::MEMORY_DB_FILENAME))
        .map_err(|error| format!("resolve configured global DB filename: {error}"))
}

fn probe_worktree_holder_at_db(db_path: &Path, worktree: &Path) -> DbHolderEvidence {
    if db_path.as_os_str().is_empty() {
        return DbHolderEvidence::Unavailable("Tachi home is not configured".to_string());
    }
    if !db_path.is_file() {
        return DbHolderEvidence::Unavailable(format!(
            "configured global DB is unavailable at {}",
            db_path.display()
        ));
    }
    let db = match db_path.to_str() {
        Some(path) => path,
        None => {
            return DbHolderEvidence::Unverifiable(
                "configured global DB path is not valid UTF-8".to_string(),
            )
        }
    };
    let store = match memcore::MemoryStore::open_read_only(db) {
        Ok(store) => store,
        Err(err) => return DbHolderEvidence::Unavailable(format!("open global DB: {err}")),
    };
    // The persisted ExecEnv path is canonical. Resolve every existing
    // candidate before the exact lookup so sweep's walked spelling (for
    // example /tmp versus /private/tmp) cannot turn a held managed tree into
    // an apparent legacy/unbound environment. An unresolved path cannot
    // establish that it is legacy, so fail closed instead of returning
    // NotApplicable.
    let canonical_worktree = match std::fs::canonicalize(worktree) {
        Ok(path) => path,
        Err(err) => {
            return DbHolderEvidence::Unverifiable(format!(
                "cannot resolve worktree path before ExecEnv lookup: {err}"
            ))
        }
    };
    let path = match canonical_worktree.to_str() {
        Some(path) => path,
        None => {
            return DbHolderEvidence::Unverifiable("worktree path is not valid UTF-8".to_string())
        }
    };
    let lease = match memcore::find_live_exec_env_by_path(store.connection(), path) {
        Ok(Some(lease)) => lease,
        Ok(None) => return DbHolderEvidence::NotApplicable,
        Err(err) => return DbHolderEvidence::Unavailable(format!("find ExecEnv: {err}")),
    };
    if lease.state != memcore::ExecEnvState::Active {
        return DbHolderEvidence::Held;
    }
    match memcore::exec_env_resource_removal_refusal(store.connection(), &lease.env_id, path) {
        Ok(None) => {}
        Ok(Some(detail)) => return DbHolderEvidence::Unverifiable(detail),
        Err(err) => {
            return DbHolderEvidence::Unavailable(format!("read ExecEnv resource evidence: {err}"))
        }
    }
    match memcore::holder_evidence(store.connection(), &lease.env_id) {
        Ok(memcore::HolderEvidence::Clear) => DbHolderEvidence::Clear,
        Ok(memcore::HolderEvidence::NotApplicable) => DbHolderEvidence::NotApplicable,
        Ok(memcore::HolderEvidence::Held) => DbHolderEvidence::Held,
        Ok(memcore::HolderEvidence::Contradictory) => DbHolderEvidence::Contradictory,
        Ok(memcore::HolderEvidence::Unavailable) => {
            DbHolderEvidence::Unavailable("ledger row has an unknown claim state".to_string())
        }
        Ok(memcore::HolderEvidence::Unverifiable) => DbHolderEvidence::Unverifiable(
            "ExecEnv and WorkClaim links cannot establish a safe holder".to_string(),
        ),
        Err(err) => DbHolderEvidence::Unavailable(format!("read holder evidence: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::{EnvClass, NewExecEnvLease};

    #[test]
    fn configured_global_db_supports_legacy_and_rejects_split_brain() {
        let home = unique_temp_dir("work-claim-db-filename");
        let global = home.join("global");
        std::fs::create_dir_all(&global).unwrap();
        let legacy = global.join(memcore::LEGACY_MEMORY_DB_FILENAME);
        std::fs::write(&legacy, b"legacy").unwrap();
        assert_eq!(configured_global_db(&home).unwrap(), legacy);

        let canonical = global.join(memcore::MEMORY_DB_FILENAME);
        std::fs::write(&canonical, b"canonical").unwrap();
        let error = configured_global_db(&home)
            .expect_err("two real memory DB files must fail closed instead of choosing one");
        assert!(error.contains("both a canonical"), "{error}");
        std::fs::remove_dir_all(home).unwrap();
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn store(home: &Path) -> memcore::MemoryStore {
        let db = home.join("global").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        memcore::MemoryStore::open(db.to_str().unwrap()).unwrap()
    }

    fn insert_env(store: &mut memcore::MemoryStore, path: &Path) {
        let canonical_path = std::fs::canonicalize(path).unwrap();
        memcore::insert_exec_env(
            store.connection(),
            &NewExecEnvLease {
                env_id: "env-1".to_string(),
                kind: "worktree".to_string(),
                path: canonical_path.display().to_string(),
                repo_root: "/repo".to_string(),
                branch: "lane/test".to_string(),
                base_sha: "base".to_string(),
                dispatch_id: None,
                env_class: EnvClass::EditOnly,
                created_at: "2026-07-18T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        memcore::insert_resource(
            store.connection_mut(),
            &memcore::NewExecEnvResource {
                resource_id: "res-1".to_string(),
                kind: memcore::ResourceKind::Worktree,
                path: canonical_path.display().to_string(),
                bytes: None,
                created_at: "2026-07-18T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        memcore::bind_resource(store.connection_mut(), "env-1", "res-1").unwrap();
    }

    fn bind_claim(store: &memcore::MemoryStore, state: &str) {
        store
            .connection()
            .execute(
                "INSERT INTO session_claims \
             (claim_id, branch, state, created_at, heartbeat_at, agent_identity_id, exec_env_id) \
             VALUES ('claim-1', '', ?1, '', '', 'agent-1', 'env-1')",
                [state],
            )
            .unwrap();
        store.connection().execute(
            "UPDATE exec_envs SET agent_identity_id='agent-1', claim_id='claim-1' WHERE env_id='env-1'",
            [],
        ).unwrap();
    }

    #[test]
    fn persisted_holder_evidence_distinguishes_all_cleanup_branches() {
        let root = unique_temp_dir("work-claim-holder-evidence");
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();

        // Legacy / no ExecEnv is explicitly NotApplicable, not a guessed clear.
        let not_applicable_home = root.join("not-applicable");
        let _store = store(&not_applicable_home);
        assert_eq!(
            probe_worktree_holder_from_home(&not_applicable_home, &worktree),
            DbHolderEvidence::NotApplicable
        );

        let clear_home = root.join("clear");
        let mut clear_store = store(&clear_home);
        insert_env(&mut clear_store, &worktree);
        bind_claim(&clear_store, "released");
        assert_eq!(
            probe_worktree_holder_from_home(&clear_home, &worktree),
            DbHolderEvidence::Clear
        );

        let held_home = root.join("held");
        let mut held_store = store(&held_home);
        insert_env(&mut held_store, &worktree);
        bind_claim(&held_store, "active");
        assert_eq!(
            probe_worktree_holder_from_home(&held_home, &worktree),
            DbHolderEvidence::Held
        );

        let dispatching_home = root.join("dispatching");
        let mut dispatching_store = store(&dispatching_home);
        insert_env(&mut dispatching_store, &worktree);
        dispatching_store
            .connection()
            .execute(
                "UPDATE exec_envs SET state='dispatching' WHERE env_id='env-1'",
                [],
            )
            .unwrap();
        assert_eq!(
            probe_worktree_holder_from_home(&dispatching_home, &worktree),
            DbHolderEvidence::Held,
            "a dispatching lease must block destructive cleanup even before holder evidence exists"
        );
        dispatching_store
            .connection()
            .execute(
                "UPDATE exec_envs SET state='removing' WHERE env_id='env-1'",
                [],
            )
            .unwrap();
        assert_eq!(
            probe_worktree_holder_from_home(&dispatching_home, &worktree),
            DbHolderEvidence::Held,
            "a removal claim must remain visible to every destructive cleaner"
        );

        let quarantined_home = root.join("quarantined");
        let mut quarantined_store = store(&quarantined_home);
        insert_env(&mut quarantined_store, &worktree);
        memcore::quarantine_resource(
            quarantined_store.connection_mut(),
            "res-1",
            "postflight rejected",
        )
        .unwrap();
        assert!(matches!(
            probe_worktree_holder_from_home(&quarantined_home, &worktree),
            DbHolderEvidence::Unverifiable(detail) if detail.contains("is quarantined")
        ));

        let unverifiable_home = root.join("unverifiable");
        let mut unverifiable_store = store(&unverifiable_home);
        insert_env(&mut unverifiable_store, &worktree);
        unverifiable_store.connection().execute(
            "UPDATE exec_envs SET agent_identity_id='agent-1', claim_id='missing' WHERE env_id='env-1'",
            [],
        ).unwrap();
        assert!(
            matches!(
                probe_worktree_holder_from_home(&unverifiable_home, &worktree),
                DbHolderEvidence::Unverifiable(_)
            ),
            "a missing bound claim is not safe to infer as clear"
        );

        let contradictory_home = root.join("contradictory");
        let mut contradictory_store = store(&contradictory_home);
        insert_env(&mut contradictory_store, &worktree);
        bind_claim(&contradictory_store, "released");
        contradictory_store
            .connection()
            .execute(
                "UPDATE session_claims SET exec_env_id='other-env' WHERE claim_id='claim-1'",
                [],
            )
            .unwrap();
        assert_eq!(
            probe_worktree_holder_from_home(&contradictory_home, &worktree),
            DbHolderEvidence::Contradictory
        );

        let unavailable_home = root.join("unavailable");
        assert!(matches!(
            probe_worktree_holder_from_home(&unavailable_home, &worktree),
            DbHolderEvidence::Unavailable(_)
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unresolved_path_is_unverifiable_not_legacy() {
        let root = unique_temp_dir("work-claim-unresolved-path");
        let home = root.join("legacy");
        let _store = store(&home);

        assert!(matches!(
            probe_worktree_holder_from_home(&home, &root.join("missing-worktree")),
            DbHolderEvidence::Unverifiable(_)
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn removal_claim_fences_dispatch_and_has_explicit_terminal_transitions() {
        let root = unique_temp_dir("work-claim-removal-admission");
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let home = root.join("home");
        let mut seeded = store(&home);
        insert_env(&mut seeded, &worktree);
        drop(seeded);

        let claim = claim_worktree_removal_from_home(&home, &worktree).unwrap();
        let db = configured_global_db(&home).unwrap();
        let mut observed = open_maintenance_store(&db).unwrap();
        assert_eq!(
            memcore::get_exec_env(observed.connection(), "env-1")
                .unwrap()
                .unwrap()
                .state,
            memcore::ExecEnvState::Removing
        );
        assert_eq!(
            observed
                .connection_mut()
                .execute(
                    "UPDATE exec_envs SET state='dispatching' WHERE env_id='env-1' AND state='active'",
                    [],
                )
                .unwrap(),
            0
        );
        drop(observed);
        claim.abort().unwrap();

        let observed = open_maintenance_store(&db).unwrap();
        assert_eq!(
            memcore::get_exec_env(observed.connection(), "env-1")
                .unwrap()
                .unwrap()
                .state,
            memcore::ExecEnvState::Active
        );
        assert_eq!(
            memcore::get_resource(observed.connection(), "res-1")
                .unwrap()
                .unwrap()
                .state,
            memcore::ResourceState::Active
        );
        assert_eq!(
            memcore::active_binding_count(observed.connection(), "res-1").unwrap(),
            1
        );
        drop(observed);

        let claim = claim_worktree_removal_from_home(&home, &worktree).unwrap();
        claim.complete(123).unwrap();
        let observed = open_maintenance_store(&db).unwrap();
        assert_eq!(
            memcore::get_exec_env(observed.connection(), "env-1")
                .unwrap()
                .unwrap()
                .state,
            memcore::ExecEnvState::Reclaimed
        );
        let resource = memcore::get_resource(observed.connection(), "res-1")
            .unwrap()
            .unwrap();
        assert_eq!(resource.state, memcore::ResourceState::Reclaimed);
        assert_eq!(resource.reclaimed_bytes, Some(123));
        assert_eq!(
            memcore::active_binding_count(observed.connection(), "res-1").unwrap(),
            0
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn held_env_is_found_through_a_symlinked_path_spelling() {
        let root = unique_temp_dir("work-claim-holder-canonical-path");
        let worktree = root.join("worktree");
        let alias = root.join("worktree-alias");
        std::fs::create_dir_all(&worktree).unwrap();
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();

        let home = root.join("held");
        let mut store = store(&home);
        insert_env(&mut store, &worktree);
        bind_claim(&store, "active");

        assert_eq!(
            probe_worktree_holder_from_home(&home, &alias),
            DbHolderEvidence::Held,
            "a spelling variation must resolve to the held managed ExecEnv before deletion"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}

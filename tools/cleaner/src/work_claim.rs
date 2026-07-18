//! Persisted WorkClaim holder evidence for destructive worktree cleanup.
//!
//! OS process evidence remains the independent liveness gate in `holder`.
//! This module only reads the durable ExecEnv/WorkClaim ledger; it never
//! transitions identity, claim, or ExecEnv lifecycle state.

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

/// Read holder evidence from the configured Tachi global DB.  The cleaner
/// shares the runtime's `TACHI_HOME` layout (`<home>/global/memory.db`) and
/// never creates or migrates a DB while deciding whether deletion is safe.
pub fn probe_worktree_holder(worktree: &Path) -> DbHolderEvidence {
    let home = resolve_tachi_home();
    probe_worktree_holder_from_home(&home, worktree)
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
    probe_worktree_holder_at_db(&home.join("global").join("memory.db"), worktree)
}

#[cfg(not(test))]
fn probe_worktree_holder_from_home(home: &Path, worktree: &Path) -> DbHolderEvidence {
    probe_worktree_holder_at_db(&home.join("global").join("memory.db"), worktree)
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
    let lease = match memcore::find_active_exec_env_by_path(store.connection(), path) {
        Ok(Some(lease)) => lease,
        Ok(None) => return DbHolderEvidence::NotApplicable,
        Err(err) => return DbHolderEvidence::Unavailable(format!("find ExecEnv: {err}")),
    };
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
        let db = home.join("global").join("memory.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        memcore::MemoryStore::open(db.to_str().unwrap()).unwrap()
    }

    fn insert_env(store: &memcore::MemoryStore, path: &Path) {
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
        let clear_store = store(&clear_home);
        insert_env(&clear_store, &worktree);
        bind_claim(&clear_store, "released");
        assert_eq!(
            probe_worktree_holder_from_home(&clear_home, &worktree),
            DbHolderEvidence::Clear
        );

        let held_home = root.join("held");
        let held_store = store(&held_home);
        insert_env(&held_store, &worktree);
        bind_claim(&held_store, "active");
        assert_eq!(
            probe_worktree_holder_from_home(&held_home, &worktree),
            DbHolderEvidence::Held
        );

        let unverifiable_home = root.join("unverifiable");
        let unverifiable_store = store(&unverifiable_home);
        insert_env(&unverifiable_store, &worktree);
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
        let contradictory_store = store(&contradictory_home);
        insert_env(&contradictory_store, &worktree);
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

    #[cfg(unix)]
    #[test]
    fn held_env_is_found_through_a_symlinked_path_spelling() {
        let root = unique_temp_dir("work-claim-holder-canonical-path");
        let worktree = root.join("worktree");
        let alias = root.join("worktree-alias");
        std::fs::create_dir_all(&worktree).unwrap();
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();

        let home = root.join("held");
        let store = store(&home);
        insert_env(&store, &worktree);
        bind_claim(&store, "active");

        assert_eq!(
            probe_worktree_holder_from_home(&home, &alias),
            DbHolderEvidence::Held,
            "a spelling variation must resolve to the held managed ExecEnv before deletion"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}

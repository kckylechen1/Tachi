//! Canonical dispatch-outcome persistence on [`MemoryStore`].

use crate::{db, DispatchOutcomeRow, MemoryError, MemoryStore, NewDispatchOutcome};

impl MemoryStore {
    /// Atomically reconcile one canonical dispatch outcome under the store's
    /// bounded SQLite lock-retry policy. The retry closure is intentionally
    /// database-only: callers must not replay delivery commands or event
    /// appends while recovering a transient local lock.
    pub fn upsert_dispatch_outcome(
        &self,
        new: &NewDispatchOutcome,
    ) -> Result<DispatchOutcomeRow, MemoryError> {
        db::retry_memory_locked("dispatch_outcomes_upsert", &self.db_label, || {
            db::upsert_outcome_reconciling_terminal_placeholder(&self.conn, new)
        })
        .map_err(|error| {
            MemoryError::InvalidArg(format!(
                "dispatch outcome persistence failed after retry_memory_locked(op=dispatch_outcomes_upsert, db_label={}): {error}",
                self.db_label
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn new_outcome(outcome_id: &str) -> NewDispatchOutcome {
        NewDispatchOutcome {
            outcome_id: outcome_id.to_string(),
            dispatch_id: "dispatch-concurrent".to_string(),
            vendor: "codex".to_string(),
            task_type: Some("completion".to_string()),
            execution_outcome: "completed".to_string(),
            reported_outcome: Some("success".to_string()),
            evidence_refs: serde_json::json!(["delivery already durable"]),
            identity_attribution_basis: "unknown".to_string(),
            ..Default::default()
        }
    }

    fn row_count(store: &MemoryStore) -> i64 {
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM dispatch_outcomes \
                 WHERE dispatch_id = 'dispatch-concurrent' AND task_type = 'completion'",
                [],
                |row| row.get(0),
            )
            .expect("count canonical completion rows")
    }

    #[test]
    fn replay_keeps_the_first_outcome_id_and_one_row() {
        let dir = tempfile::tempdir().expect("temp db directory");
        let db_path = dir.path().join("memory.db");
        let store =
            MemoryStore::open_with_label(db_path.to_str().expect("utf-8 temp db path"), "global")
                .expect("open labelled store");

        let first = store
            .upsert_dispatch_outcome(&new_outcome("outcome-first"))
            .expect("first canonical outcome");
        let second = store
            .upsert_dispatch_outcome(&new_outcome("outcome-replay"))
            .expect("replayed canonical outcome");

        assert_eq!(first.outcome_id, second.outcome_id);
        assert_eq!(row_count(&store), 1);
    }

    #[test]
    fn concurrent_completions_same_key_return_one_row() {
        let dir = tempfile::tempdir().expect("temp db directory");
        let db_path = dir.path().join("memory.db");
        let db_path = db_path.to_string_lossy().to_string();
        MemoryStore::open_with_label(&db_path, "global").expect("initialize labelled store");

        let barrier = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for outcome_id in ["outcome-a", "outcome-b"] {
            let barrier = Arc::clone(&barrier);
            let db_path = db_path.clone();
            workers.push(std::thread::spawn(move || {
                let store = MemoryStore::open_with_label(&db_path, "global")
                    .expect("open concurrent labelled store");
                barrier.wait();
                store
                    .upsert_dispatch_outcome(&new_outcome(outcome_id))
                    .expect("concurrent canonical outcome")
                    .outcome_id
            }));
        }
        let outcome_ids: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().expect("concurrent writer joins"))
            .collect();
        let store =
            MemoryStore::open_with_label(&db_path, "global").expect("reopen labelled store");

        assert_eq!(outcome_ids[0], outcome_ids[1]);
        assert_eq!(row_count(&store), 1);
    }
}

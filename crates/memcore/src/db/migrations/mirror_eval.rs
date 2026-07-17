//! v20 (#1066) — `mirror_eval_runs` / `mirror_eval_observations` /
//! `mirror_eval_adjudications` tables: first-class mirror eval intake for
//! harness-native subagents (register/observe/adjudicate/get), mirroring
//! the v18 `dispatch_adjudications` migration for a DB that predates this
//! table set.

use rusqlite::Connection;

use crate::error::MemoryError;

pub(super) fn migrate_v20_mirror_eval(conn: &Connection) -> Result<usize, MemoryError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mirror_eval_runs (
            eval_run_id         TEXT PRIMARY KEY,
            register_key        TEXT NOT NULL UNIQUE,
            frozen_contract_ref TEXT NOT NULL CHECK (length(trim(frozen_contract_ref)) > 0),
            execution_origin    TEXT NOT NULL CHECK (length(trim(execution_origin)) > 0),
            lifecycle_owner     TEXT NOT NULL CHECK (length(trim(lifecycle_owner)) > 0),
            harness             TEXT,
            native_child_id     TEXT,
            requested_profile   TEXT,
            requested_model     TEXT,
            requested_agent     TEXT,
            created_at          TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_mirror_eval_runs_native_child
            ON mirror_eval_runs(native_child_id, created_at);
        CREATE TABLE IF NOT EXISTS mirror_eval_observations (
            observation_id   TEXT PRIMARY KEY,
            eval_run_id      TEXT NOT NULL UNIQUE,
            terminal_outcome TEXT NOT NULL CHECK (length(trim(terminal_outcome)) > 0),
            duration_ms      INTEGER,
            cost_tokens      INTEGER,
            cost_usd         REAL,
            result_ref       TEXT,
            artifacts        TEXT NOT NULL DEFAULT '[]',
            effective_model    TEXT,
            effective_backend  TEXT,
            effective_harness  TEXT,
            created_at       TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS mirror_eval_adjudications (
            adjudication_id       TEXT PRIMARY KEY,
            eval_run_id           TEXT NOT NULL,
            event_key             TEXT NOT NULL UNIQUE,
            actor                 TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            verifier_model        TEXT,
            usefulness            TEXT NOT NULL CHECK (length(trim(usefulness)) > 0),
            failure_mode          TEXT,
            first_review_findings TEXT NOT NULL DEFAULT '[]',
            plan_delta            TEXT,
            next_prompt_delta     TEXT,
            evidence_usable       INTEGER NOT NULL CHECK (evidence_usable IN (0, 1)),
            used_in_final_claim   INTEGER NOT NULL DEFAULT 0 CHECK (used_in_final_claim IN (0, 1)),
            human_override        INTEGER NOT NULL DEFAULT 0 CHECK (human_override IN (0, 1)),
            evidence_ref          TEXT NOT NULL CHECK (length(trim(evidence_ref)) > 0),
            created_at            TEXT NOT NULL DEFAULT '',
            insertion_seq         INTEGER NOT NULL,
            UNIQUE (eval_run_id, insertion_seq)
        );
        CREATE INDEX IF NOT EXISTS idx_mirror_eval_adjudications_run
            ON mirror_eval_adjudications(eval_run_id, created_at);",
    )?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v20_creates_mirror_eval_tables_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(migrate_v20_mirror_eval(&conn).unwrap(), 1);
        assert_eq!(migrate_v20_mirror_eval(&conn).unwrap(), 1);
        for table in [
            "mirror_eval_runs",
            "mirror_eval_observations",
            "mirror_eval_adjudications",
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "table {table} must exist after migration");
        }
    }
}

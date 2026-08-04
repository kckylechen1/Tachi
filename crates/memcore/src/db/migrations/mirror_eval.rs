//! v20 (#1066) — `mirror_eval_runs` / `mirror_eval_observations` /
//! `mirror_eval_adjudications` tables: first-class mirror eval intake for
//! harness-native subagents (register/observe/adjudicate/get), mirroring
//! the v18 `dispatch_adjudications` migration for a DB that predates this
//! table set.
//!
//! ## AC-8 / codex round-2 finding #3b: structural-justification exception
//!
//! AC-8 requires "pre-fix RED and post-fix GREEN evidence" per new behavior.
//! This module's table set, and everything downstream that depends on it
//! (`memcore::db::mirror_eval`, `complete_ops::mirror_eval_projection`, the
//! `tests/dispatch_tests/completion_eval/mirror_eval.rs` tool-surface
//! coverage), is net-new capability with NO pre-existing entry point on
//! `origin/main`: there is no prior `migrate_v20_mirror_eval` (or any
//! function it could regress) to call with new inputs and observe a
//! behavioral failure. "RED" for this module's tests is therefore `cargo
//! test`/`cargo check` failing to find the symbol at all on `origin/main` —
//! compile-red, not behavioral-red — which is the explicit structural-
//! justification alternative the #1066 fix-round's contract allows in place
//! of pre-fix-behavioral-RED/post-fix-GREEN, precisely because no prior
//! behavior existed to regress. The 2 tests that DO modify pre-existing
//! behavior (the `tool_profile.rs` / `filtering.rs` profile-visibility flip)
//! remain genuinely behavioral RED/GREEN and are not covered by this
//! exception.

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

pub(super) fn migrate_v20_mirror_eval(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    // #1585 D3: product-scoped migration. A PortableKernel store never
    // created the table(s) this touches, so the work is vacuously done.
    // Returning Ok here (rather than skipping the call) is deliberate:
    // `apply_versioned_migration` still marks the sentinel, so a portable
    // database is a COMPLETE stamped-28 database by every existing gate's
    // definition (`validate_current_schema_integrity`,
    // `MIGRATION_SENTINEL_KEYS`) — the sentinel set is profile-invariant.
    if !profile.includes_product() {
        return Ok(0);
    }
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
        assert_eq!(
            migrate_v20_mirror_eval(&conn, StoreProfile::TachiFull).unwrap(),
            1
        );
        assert_eq!(
            migrate_v20_mirror_eval(&conn, StoreProfile::TachiFull).unwrap(),
            1
        );
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

//! v39 (#1888 follow-up): additive identity/task metadata columns on the
//! v20 mirror eval tables, so `tachi_agent_eval(action='candidate_projection')`
//! can confirm dimensions it previously had to report as
//! unrecorded:
//!
//! - `mirror_eval_runs.requested_task_type` — task kind frozen as contract
//!   metadata at registration (requested basis, `register_requested`; the
//!   mirror spine has no carrier-observed task fact and never pretends one).
//! - `mirror_eval_runs.requested_role` — register-time role (requested
//!   basis). Role CONFIRMATION never reads this column; it exists so the
//!   requested intent is durable and displayable, mirroring
//!   `requested_model`'s relationship to `effective_model`.
//! - `mirror_eval_observations.effective_role` — carrier-observed role; the
//!   ONLY role source a candidate projection may confirm against.
//! - `mirror_eval_observations.effective_model_revision` — explicit
//!   carrier-observed model revision, separate from the model identity
//!   string. A legacy `@version` suffix on `effective_model` remains a
//!   fallback ONLY where this column is NULL; NEW writes refuse conflicting
//!   representations (see `record_mirror_eval_observation`).
//!
//! All four columns are nullable and content-free for legacy rows: a NULL
//! stays explicitly unknown/historical — it is never backfilled, defaulted,
//! or satisfied from a requested-basis column (the v27 content-free-column
//! precedent). The migration is ALTER-only: no data change, no new objects,
//! receipt count = columns added.

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

pub(super) fn migrate_v39_mirror_eval_identity(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    // #1585 D3 product-scoping, same guard as v20: a PortableKernel store
    // never created the mirror tables these columns belong to, so the work
    // is vacuously done and the sentinel still stamps (the sentinel set is
    // profile-invariant).
    if !profile.includes_product() {
        return Ok(0);
    }
    crate::db::schema::install_mirror_eval_identity_schema(conn)?;
    crate::db::schema::validate_mirror_eval_identity_schema(conn)?;
    Ok(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::mirror_eval;

    fn open_product_conn() -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
        conn.query_row(
            &format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"),
            [column],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// The migration adds exactly the four nullable columns, is idempotent,
    /// and the validator refuses a database missing any of them.
    #[test]
    fn v39_adds_the_four_identity_columns_idempotently() {
        let conn = open_product_conn();
        assert_eq!(
            migrate_v39_mirror_eval_identity(&conn, StoreProfile::TachiFull).unwrap(),
            4
        );
        for (table, column) in [
            ("mirror_eval_runs", "requested_task_type"),
            ("mirror_eval_runs", "requested_role"),
            ("mirror_eval_observations", "effective_role"),
            ("mirror_eval_observations", "effective_model_revision"),
        ] {
            assert!(
                column_exists(&conn, table, column),
                "{table}.{column} must exist after v39"
            );
        }
        // Idempotent replay: same receipt, no error, still valid.
        assert_eq!(
            migrate_v39_mirror_eval_identity(&conn, StoreProfile::TachiFull).unwrap(),
            4
        );
        crate::db::schema::validate_mirror_eval_identity_schema(&conn).unwrap();

        // Fail-closed: a database still carrying only the v20 shape (none of
        // the v39 columns) is refused by the validator, not silently repaired.
        let legacy = Connection::open_in_memory().unwrap();
        mirror_eval::migrate_v20_mirror_eval(&legacy, StoreProfile::TachiFull).unwrap();
        assert!(crate::db::schema::validate_mirror_eval_identity_schema(&legacy).is_err());
    }

    /// A portable store skips the work (tables do not exist there) and still
    /// reports a stable zero receipt.
    #[test]
    fn v39_is_vacuous_on_portable_stores() {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        assert_eq!(
            migrate_v39_mirror_eval_identity(&conn, StoreProfile::PortableKernel).unwrap(),
            0
        );
    }

    /// Existing rows remain NULL after the additive v39 installer.
    #[test]
    fn v39_leaves_legacy_rows_null_never_backfilled() {
        let conn = open_product_conn();
        // A legacy-shaped run: registered with the v39 fields omitted, so its row has
        // no requested_task_type/requested_role; observed without the new
        // observation columns.
        let legacy_run = crate::db::mirror_eval::register_mirror_eval_run(
            &conn,
            &crate::db::mirror_eval::NewMirrorEvalRun {
                frozen_contract_ref: "kckylechen1/tachi#1066".to_string(),
                execution_origin: "host_native_subagent".to_string(),
                lifecycle_owner: "host".to_string(),
                native_child_id: Some("v34-legacy".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::mirror_eval::record_mirror_eval_observation(
            &conn,
            &crate::db::mirror_eval::NewMirrorEvalObservation {
                eval_run_id: legacy_run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                effective_model: Some("openai/gpt-5@2026-03-10".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::mirror_eval::append_mirror_eval_adjudication(
            &conn,
            &crate::db::mirror_eval::NewMirrorEvalAdjudication {
                adjudication_id: "v34-adj".to_string(),
                eval_run_id: legacy_run.eval_run_id.clone(),
                event_key: "v34-adj-key".to_string(),
                actor: "leader".to_string(),
                usefulness: "useful".to_string(),
                evidence_usable: true,
                evidence_ref: "run-v34".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        migrate_v39_mirror_eval_identity(&conn, StoreProfile::TachiFull).unwrap();

        let (task_type, role): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT requested_task_type, requested_role FROM mirror_eval_runs \
                 WHERE eval_run_id = ?1",
                [&legacy_run.eval_run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(task_type, None);
        assert_eq!(role, None);
        let (effective_role, revision): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT effective_role, effective_model_revision FROM mirror_eval_observations \
                 WHERE eval_run_id = ?1",
                [&legacy_run.eval_run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(effective_role, None);
        assert_eq!(
            revision, None,
            "the legacy @suffix stays on the model string; the column is never backfilled"
        );
    }
}

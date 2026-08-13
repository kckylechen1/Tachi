//! Narrow operator-only preview/apply seams for irreversible maintenance.

use rusqlite::Connection;

use crate::{db, GcConfig, MemoryError, MemoryStore};

impl MemoryStore {
    /// Select the complete canonical GC candidate registry at one frozen time.
    /// This method performs SELECTs only; `include_kanban` is true for the
    /// canonical CLI and false for the historical table-only caller.
    pub fn plan_operator_gc(
        &self,
        cfg: &GcConfig,
        as_of: &str,
        kanban_max_age_days: u64,
        include_kanban: bool,
    ) -> Result<Vec<db::MaintenanceClassFact>, MemoryError> {
        db::gc_candidate_facts(
            &self.conn,
            cfg,
            self.profile,
            as_of,
            kanban_max_age_days,
            include_kanban,
        )
    }

    /// Re-freeze, mutate, and expose the precommit boundary inside one
    /// `BEGIN IMMEDIATE` transaction. The callback is where the server makes
    /// its prepared receipt durable; an error rolls the whole mutation back.
    pub fn apply_operator_gc_with_precommit_receipt<F>(
        &mut self,
        cfg: &GcConfig,
        as_of: &str,
        kanban_max_age_days: u64,
        include_kanban: bool,
        expected: &[db::MaintenanceClassFact],
        before_commit: F,
    ) -> Result<db::GcMaintenanceOutcome, MemoryError>
    where
        F: FnOnce(
            &Connection,
            &[db::MaintenanceClassFact],
            &[db::MaintenanceClassFact],
        ) -> Result<(), MemoryError>,
    {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::apply_gc_candidate_facts(
            &mut self.conn,
            cfg,
            self.profile,
            self.vec_available,
            as_of,
            kanban_max_age_days,
            include_kanban,
            expected,
            before_commit,
        )
    }

    /// Select exact-row and canonical associated-state facts for one ID.
    pub fn plan_operator_delete(
        &self,
        id: &str,
    ) -> Result<Vec<db::MaintenanceClassFact>, MemoryError> {
        db::delete_candidate_facts(&self.conn, id, self.vec_available, self.profile)
    }

    /// Re-freeze and canonically delete one exact ID inside one transaction.
    pub fn apply_operator_delete_with_precommit_receipt<F>(
        &mut self,
        id: &str,
        expected: &[db::MaintenanceClassFact],
        before_commit: F,
    ) -> Result<db::DeleteMaintenanceOutcome, MemoryError>
    where
        F: FnOnce(
            &Connection,
            &[db::MaintenanceClassFact],
            &[db::MaintenanceClassFact],
        ) -> Result<(), MemoryError>,
    {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::apply_delete_candidate_facts(
            &mut self.conn,
            id,
            self.vec_available,
            self.profile,
            expected,
            before_commit,
        )
    }
}

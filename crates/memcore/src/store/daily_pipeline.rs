//! Daily pipeline SQL helpers on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryStore};

pub use crate::db::{
    CategorySourceGroup, DailyHealthDbSnapshot, DuplicateSummaryRow, EvalEvidenceRow,
};

impl MemoryStore {
    pub fn list_eval_evidence(
        &self,
        days: i64,
        limit: usize,
        exclude_auto_synthesized: bool,
    ) -> Result<Vec<EvalEvidenceRow>, MemoryError> {
        db::list_eval_evidence(&self.conn, days, limit, exclude_auto_synthesized)
    }

    pub fn collect_daily_health_snapshot(&self) -> Result<DailyHealthDbSnapshot, MemoryError> {
        db::collect_daily_health_snapshot(&self.conn)
    }

    pub fn count_consolidated_active_memories(&self) -> Result<i64, MemoryError> {
        db::count_consolidated_active_memories(&self.conn)
    }

    pub fn list_memory_ids_needing_embedding(
        &self,
        limit: usize,
    ) -> Result<Vec<String>, MemoryError> {
        db::list_memory_ids_needing_embedding(&self.conn, limit)
    }

    pub fn list_promotion_candidate_ids(&self, limit: usize) -> Result<Vec<String>, MemoryError> {
        db::list_promotion_candidate_ids(&self.conn, limit)
    }

    // tachi#1446 lever 6: `count_distinct_access_days` used to be re-exported
    // here as a second `MemoryStore` method with the same body as
    // `MemoryStore::distinct_access_days` in `store/maintenance.rs`. It had no
    // callers, and two store-level names for one query is how the promotion
    // ratchet came to be fixable in a place the live pipeline does not read.
    // `MemoryStore::distinct_access_days` is the single store-level entry
    // point; it takes an `AccessEventKind`.
}

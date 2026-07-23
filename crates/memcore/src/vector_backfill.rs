//! Portable vector-backfill membership and read models.
//!
//! Backfill has several callers (manual CLI, daemon sweep, and status-time
//! sweep state). Keeping its membership predicate here prevents a caller from
//! silently counting a different population than it selects for embedding.

use crate::{MemoryError, MemoryStore, RECALL_CACHE_SQL_WHERE, RECALL_CACHE_SQL_WHERE_M};

/// Whether ephemeral recall-rerank cache entries are eligible for a vector
/// backfill. Cache rows are deliberately opt-in: they are non-durable work
/// products and must not inflate normal coverage or backfill worklists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VectorBackfillScope {
    pub include_cache: bool,
}

/// A stable vector-backfill candidate payload, independent of provider code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorBackfillEntry {
    pub id: String,
    pub text: String,
    pub summary: String,
    pub revision: i64,
}

/// Counts over the exact eligible population used by vector-backfill selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VectorBackfillCounts {
    pub total: usize,
    pub with_vector: usize,
    pub pending: usize,
}

/// SQL classification for anchor plumbing rows. Keep this aligned with
/// [`crate::is_anchor_entry`]: anchors are graph endpoints, not embeddable
/// memory content, so neither their vector presence nor absence is backfill
/// work.
const ANCHOR_SQL_WHERE: &str = "id GLOB 'anchor:*' OR path = '/anchors' OR path GLOB '/anchors/*'";
const ANCHOR_SQL_WHERE_M: &str =
    "m.id GLOB 'anchor:*' OR m.path = '/anchors' OR m.path GLOB '/anchors/*'";

/// Return the canonical SQL membership predicate for vector backfill.
///
/// `qualified` selects the `m.`-qualified form required by queries that alias
/// `memories` as `m`. Status and selection callers must use this instead of
/// reconstructing the cache/anchor exclusions.
pub fn vector_backfill_eligible_where(scope: VectorBackfillScope, qualified: bool) -> String {
    let anchor_where = if qualified {
        ANCHOR_SQL_WHERE_M
    } else {
        ANCHOR_SQL_WHERE
    };
    let cache_where = if qualified {
        RECALL_CACHE_SQL_WHERE_M
    } else {
        RECALL_CACHE_SQL_WHERE
    };
    if scope.include_cache {
        format!("NOT ({anchor_where})")
    } else {
        format!("NOT ({anchor_where}) AND NOT ({cache_where})")
    }
}

impl MemoryStore {
    /// Count the exact membership set used by [`Self::vector_backfill_entries`].
    ///
    /// Archived entries intentionally remain eligible: existing health
    /// coverage includes them, and leaving their missing vectors unselected
    /// would make pending counts permanently unresolvable. Entries with a
    /// vector remain in `total`/`with_vector` but not `pending`.
    pub fn vector_backfill_counts(
        &self,
        scope: VectorBackfillScope,
    ) -> Result<VectorBackfillCounts, MemoryError> {
        let where_clause = vector_backfill_eligible_where(scope, true);
        let (total, with_vector): (i64, i64) = self.conn.query_row(
            &format!(
                "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN v.id IS NOT NULL THEN 1 ELSE 0 END), 0)
                 FROM memories m
                 LEFT JOIN memories_vec v ON v.id = m.id
                 WHERE {where_clause}"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let total = total.max(0) as usize;
        let with_vector = with_vector.max(0) as usize;
        Ok(VectorBackfillCounts {
            total,
            with_vector,
            pending: total.saturating_sub(with_vector),
        })
    }

    /// List missing-vector rows from the same membership set counted by
    /// [`Self::vector_backfill_counts`].
    pub fn vector_backfill_entries(
        &self,
        scope: VectorBackfillScope,
        limit: Option<usize>,
    ) -> Result<Vec<VectorBackfillEntry>, MemoryError> {
        let where_clause = vector_backfill_eligible_where(scope, true);
        let order_by = crate::embed_config::embed_selection_order_by("m.");
        let limit = limit.map(|value| value as i64).unwrap_or(-1);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT m.id, m.text, m.summary, m.revision
             FROM memories m
             LEFT JOIN memories_vec v ON v.id = m.id
             WHERE {where_clause}
               AND v.id IS NULL
             {order_by}
             LIMIT ?1"
        ))?;
        let rows = stmt.query_map(rusqlite::params![limit], |row| {
            Ok(VectorBackfillEntry {
                id: row.get(0)?,
                text: row.get(1)?,
                summary: row.get(2)?,
                revision: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::{VectorBackfillCounts, VectorBackfillScope};
    use crate::MemoryStore;
    use rusqlite::params;

    fn insert_memory(
        store: &MemoryStore,
        id: &str,
        path: &str,
        source: &str,
        topic: &str,
        metadata: &str,
        archived: bool,
    ) {
        let now = chrono::Utc::now().to_rfc3339();
        store
            .connection()
            .execute(
                "INSERT INTO memories (
                    id, path, summary, text, importance, timestamp, category, topic,
                    keywords, entities, source, scope, archived,
                    created_at, updated_at, access_count, revision, metadata
                 ) VALUES (?1, ?2, '', 'body', 0.5, ?3, 'fact', ?4,
                           '[]', '[]', ?5, 'project', ?6,
                           ?3, ?3, 0, 1, ?7)",
                params![id, path, now, topic, source, archived, metadata],
            )
            .expect("insert memory");
    }

    #[test]
    fn vector_backfill_selection_and_counts_share_all_membership_classes() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("vector-backfill.db");
        let mut store = MemoryStore::open(db_path.to_str().expect("utf8")).expect("open");

        insert_memory(&store, "ordinary", "/notes", "manual", "note", "{}", false);
        insert_memory(&store, "archived", "/notes", "manual", "note", "{}", true);
        insert_memory(
            &store,
            "vector-present",
            "/notes",
            "manual",
            "note",
            "{}",
            false,
        );
        insert_memory(
            &store,
            "cache-source",
            "/notes",
            "foundry_recall_rerank_cache",
            "note",
            "{}",
            false,
        );
        insert_memory(
            &store,
            "cache-topic",
            "/notes",
            "manual",
            "recall_rerank_cache",
            "{}",
            false,
        );
        insert_memory(
            &store,
            "foundry:recall-cache:id",
            "/notes",
            "manual",
            "note",
            "{}",
            false,
        );
        insert_memory(
            &store,
            "cache-path",
            "/recall-cache/item",
            "manual",
            "note",
            "{}",
            false,
        );
        insert_memory(
            &store,
            "cache-metadata-bool",
            "/notes",
            "manual",
            "note",
            r#"{"recall_rerank_cache":true}"#,
            false,
        );
        insert_memory(
            &store,
            "cache-key",
            "/notes",
            "manual",
            "note",
            r#"{"cache_key":"foundry_recall_rerank_cache"}"#,
            false,
        );
        insert_memory(&store, "anchor:id", "/notes", "manual", "note", "{}", false);
        insert_memory(
            &store,
            "path-anchor",
            "/anchors/project/x",
            "manual",
            "note",
            "{}",
            false,
        );

        let vector = vec![0.0_f32; 1024];
        assert!(store
            .update_enrichment_fields("vector-present", None, Some(&vector), None, None, 1)
            .expect("write vector"));

        let durable_scope = VectorBackfillScope::default();
        let durable_counts = store.vector_backfill_counts(durable_scope).expect("counts");
        assert_eq!(
            durable_counts,
            VectorBackfillCounts {
                total: 3,
                with_vector: 1,
                pending: 2,
            }
        );
        let mut durable_ids: Vec<_> = store
            .vector_backfill_entries(durable_scope, None)
            .expect("entries")
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        durable_ids.sort();
        assert_eq!(durable_ids, ["archived", "ordinary"]);
        assert_eq!(durable_counts.pending, durable_ids.len());

        let cache_scope = VectorBackfillScope {
            include_cache: true,
        };
        let cache_counts = store.vector_backfill_counts(cache_scope).expect("counts");
        assert_eq!(cache_counts.total, 9);
        assert_eq!(cache_counts.with_vector, 1);
        let cache_entries = store
            .vector_backfill_entries(cache_scope, None)
            .expect("entries");
        assert_eq!(cache_counts.pending, cache_entries.len());
    }

    #[test]
    fn anchor_membership_is_case_sensitive_like_the_rust_classifier() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("case-sensitive-anchors.db");
        let store = MemoryStore::open(db_path.to_str().expect("utf8")).expect("open");

        insert_memory(
            &store,
            "anchor:canonical",
            "/notes",
            "manual",
            "note",
            "{}",
            false,
        );
        insert_memory(
            &store,
            "canonical-path-anchor",
            "/anchors/project/x",
            "manual",
            "note",
            "{}",
            false,
        );
        insert_memory(
            &store,
            "Anchor:mixed-case-id",
            "/notes",
            "manual",
            "note",
            "{}",
            false,
        );
        insert_memory(
            &store,
            "mixed-case-path",
            "/Anchors/project/x",
            "manual",
            "note",
            "{}",
            false,
        );

        let scope = VectorBackfillScope::default();
        let counts = store.vector_backfill_counts(scope).expect("counts");
        let mut ids: Vec<_> = store
            .vector_backfill_entries(scope, None)
            .expect("entries")
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        ids.sort();

        assert_eq!(counts.total, 2);
        assert_eq!(counts.pending, 2);
        assert_eq!(ids, ["Anchor:mixed-case-id", "mixed-case-path"]);
    }
}

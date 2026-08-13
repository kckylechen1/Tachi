//! Derived item and retention methods on [`MemoryStore`].

use crate::{db, error::MemoryError, types::GcConfig, MemoryStore};

impl MemoryStore {
    /// Save a derived item (causal extraction, distilled rule, etc.)
    #[allow(clippy::too_many_arguments)]
    pub fn save_derived(
        &self,
        text: &str,
        path: &str,
        summary: &str,
        importance: f64,
        source: &str,
        scope: &str,
        metadata: &serde_json::Value,
    ) -> Result<String, MemoryError> {
        crate::path_router::validate_retired_sticky_write(path, "other")
            .map_err(|error| MemoryError::InvalidArg(error.to_string()))?;
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::save_derived(
            &self.conn, text, path, summary, importance, source, scope, metadata,
        )
    }

    /// Save or update a derived item with a stable caller-provided id.
    #[allow(clippy::too_many_arguments)]
    pub fn save_derived_with_id(
        &self,
        id: &str,
        text: &str,
        path: &str,
        summary: &str,
        importance: f64,
        source: &str,
        scope: &str,
        metadata: &serde_json::Value,
    ) -> Result<(), MemoryError> {
        crate::path_router::validate_retired_sticky_write(path, "other")
            .map_err(|error| MemoryError::InvalidArg(error.to_string()))?;
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::save_derived_with_id(
            &self.conn, id, text, path, summary, importance, source, scope, metadata,
        )
    }

    /// List derived items by source and path prefix.
    pub fn list_derived_by_source(
        &self,
        source: &str,
        path_prefix: &str,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, MemoryError> {
        db::list_derived_by_source(&self.conn, source, path_prefix, limit)
    }

    /// Archive a memory entry (set archived=1, used after merge).
    pub fn archive_memory(&self, id: &str) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::archive_memory(&self.conn, id)
    }

    /// Archive an active memory only when its revision is the one inspected.
    pub fn archive_memory_if_revision(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::archive_memory_if_revision(&self.conn, id, expected_revision)
    }

    /// Restore an archived memory only when its archived revision is unchanged.
    pub fn restore_archived_if_revision(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::restore_archived_if_revision(&self.conn, id, expected_revision)
    }

    /// Mark a memory as superseded by a newer/canonical memory.
    pub fn supersede_memory(&self, id: &str, superseded_by: &str) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::supersede_memory(&self.conn, id, superseded_by)
    }

    /// Mark a memory as superseded only when its inspected revision is still
    /// current. This keeps migration lifecycle edges revision-CAS protected.
    pub fn supersede_memory_if_revision(
        &self,
        id: &str,
        superseded_by: &str,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::supersede_memory_if_revision(&self.conn, id, superseded_by, expected_revision)
    }

    /// Run retention-based garbage collection on growing tables.
    /// Thresholds are driven by `GcConfig` (replaces previously hardcoded literals).
    pub fn gc_tables(&mut self, cfg: &GcConfig) -> Result<serde_json::Value, MemoryError> {
        let db_label = self.db_label.clone();
        let profile = self.profile;
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("gc_tables", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::gc_tables(&mut self.conn, cfg, profile)
        })
    }

    /// Archive low-importance memories not accessed in `stale_days`.
    ///
    /// Writes a `memory.gc_archived` receipt to `tachi_events` naming the rows
    /// and thresholds whenever the sweep archives anything — see
    /// [`db::archive_stale_memories`] (tachi#1463).
    ///
    /// Retried on `SQLITE_BUSY` like [`Self::gc_tables`]: tachi#1463 made the
    /// sweep a single transaction so its receipt cannot describe a partially
    /// applied archival, which means it now holds the write lock across all
    /// four passes instead of releasing it between four autocommit statements.
    /// Retrying is safe because that transaction rolls back whole, and a replay
    /// re-selects from scratch — rows archived by a committed attempt no longer
    /// match `archived = 0`.
    pub fn archive_stale_memories(&self, stale_days: u32) -> Result<u64, MemoryError> {
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        // tachi#1585 D5: the store's own `KernelPolicy::recall`, not
        // `db::archive_stale_memories`'s pure-default convenience wrapper.
        db::retry_memory_locked("archive_stale_memories", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::archive_stale_memories_with_config(&self.conn, stale_days, &self.policy.recall)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use rusqlite::types::Value as SqlValue;

    fn derived_snapshot(store: &MemoryStore) -> Vec<Vec<String>> {
        let mut stmt = store
            .connection()
            .prepare(
                "SELECT id, text, path, summary, importance, source, scope, metadata, created_at \
                 FROM derived_items ORDER BY id",
            )
            .expect("prepare derived snapshot");
        let column_count = stmt.column_count();
        stmt.query_map([], |row| {
            (0..column_count)
                .map(|column| {
                    row.get::<_, SqlValue>(column)
                        .map(|value| format!("{value:?}"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("read derived snapshot")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect derived snapshot")
    }

    fn database_snapshot(store: &MemoryStore) -> Vec<(String, Vec<Vec<String>>)> {
        let conn = store.connection();
        let table_names = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .expect("prepare table snapshot")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("list snapshot tables")
            .collect::<Result<Vec<_>, _>>()
            .expect("read snapshot tables");

        table_names
            .into_iter()
            .map(|table| {
                let quoted = format!("\"{}\"", table.replace('"', "\"\""));
                let mut stmt = conn
                    .prepare(&format!("SELECT * FROM {quoted}"))
                    .expect("prepare table contents snapshot");
                let column_count = stmt.column_count();
                let rows = stmt
                    .query_map([], |row| {
                        (0..column_count)
                            .map(|column| {
                                row.get::<_, SqlValue>(column)
                                    .map(|value| format!("{value:?}"))
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .expect("read table contents snapshot")
                    .collect::<Result<Vec<_>, _>>()
                    .expect("collect table contents snapshot");
                (table, rows)
            })
            .collect()
    }

    fn seed_legacy_sticky(store: &MemoryStore, id: &str, archived: bool) {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize raw legacy sticky fixture");
        store
            .connection()
            .execute(
                "INSERT INTO memories
                    (id,path,summary,text,importance,timestamp,valid_from,category,topic,
                     keywords,entities,source,scope,archived,created_at,updated_at,revision,
                     metadata,retention_policy,tier)
                 VALUES (?1,'/sticky/legacy','legacy summary','legacy body',0.5,
                         '2026-08-12T00:00:00Z','2026-08-12T00:00:00Z','sticky','legacy',
                         '[]','[]','extraction','general',?2,'2026-08-12T00:00:00Z',
                         '2026-08-12T00:00:00Z',1,'{}','ephemeral','raw')",
                params![id, archived],
            )
            .expect("seed raw legacy sticky fixture");
    }

    fn ordinary_entry(id: &str) -> crate::MemoryEntry {
        crate::MemoryEntry {
            id: id.to_string(),
            path: "/ordinary".to_string(),
            summary: String::new(),
            text: "ordinary body".to_string(),
            importance: 0.5,
            timestamp: "2026-08-12T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn assert_existing_sticky_refused<F>(label: &str, archived: bool, operation: F)
    where
        F: FnOnce(&mut MemoryStore) -> Result<(), MemoryError>,
    {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        seed_legacy_sticky(&store, "legacy-sticky", archived);
        let before = database_snapshot(&store);
        let before_changes = store.connection().total_changes();
        let error = operation(&mut store).expect_err(label);
        assert!(
            error.to_string().contains("tachi_a2a"),
            "{label} must name the typed replacement: {error}"
        );
        assert_eq!(
            store.connection().total_changes(),
            before_changes,
            "{label} must execute zero SQLite changes"
        );
        assert_eq!(
            database_snapshot(&store),
            before,
            "{label} must leave every ordinary store table unchanged"
        );
    }

    #[test]
    fn derived_writers_reject_retired_paths_without_writes() {
        for (index, path) in ["/sticky", "//STICKY///legacy/"].into_iter().enumerate() {
            let store = MemoryStore::open_in_memory().expect("open memory store");
            let before = derived_snapshot(&store);
            let before_changes = store.connection().total_changes();
            let error = store
                .save_derived(
                    "derived body",
                    path,
                    "derived summary",
                    0.5,
                    "test",
                    "general",
                    &serde_json::json!({"index": index}),
                )
                .expect_err("save_derived must reject a retired path");
            assert!(error.to_string().contains("tachi_a2a"), "{error}");
            assert_eq!(store.connection().total_changes(), before_changes);
            assert_eq!(derived_snapshot(&store), before);

            let before = derived_snapshot(&store);
            let before_changes = store.connection().total_changes();
            let error = store
                .save_derived_with_id(
                    &format!("retired-derived-{index}"),
                    "derived body",
                    path,
                    "derived summary",
                    0.5,
                    "test",
                    "general",
                    &serde_json::json!({"index": index}),
                )
                .expect_err("save_derived_with_id must reject a retired path");
            assert!(error.to_string().contains("tachi_a2a"), "{error}");
            assert_eq!(store.connection().total_changes(), before_changes);
            assert_eq!(derived_snapshot(&store), before);
        }
    }

    #[test]
    fn derived_writers_accept_a_normal_path() {
        let store = MemoryStore::open_in_memory().expect("open memory store");
        let id = store
            .save_derived(
                "derived body",
                "/derived/normal",
                "derived summary",
                0.5,
                "test",
                "general",
                &serde_json::json!({}),
            )
            .expect("save_derived normal path");
        store
            .save_derived_with_id(
                "normal-derived",
                "derived body",
                "/derived/normal",
                "derived summary",
                0.5,
                "test",
                "general",
                &serde_json::json!({}),
            )
            .expect("save_derived_with_id normal path");
        assert!(!id.is_empty());
        assert_eq!(derived_snapshot(&store).len(), 2);
    }

    #[test]
    fn existing_retired_sticky_rows_are_read_only_across_representative_public_mutators() {
        assert_existing_sticky_refused("content update", false, |store| {
            store
                .update_with_revision(
                    "legacy-sticky",
                    "changed body",
                    "changed summary",
                    "manual",
                    &serde_json::json!({"changed": true}),
                    None,
                    1,
                )
                .map(|_| ())
        });
        assert_existing_sticky_refused("metadata failure stamp", false, |store| {
            store.record_enrichment_failure("legacy-sticky", "summary", "failed")
        });
        assert_existing_sticky_refused("enrichment update", false, |store| {
            store
                .update_enrichment_fields(
                    "legacy-sticky",
                    Some("enriched summary"),
                    None,
                    None,
                    None,
                    1,
                )
                .map(|_| ())
        });
        assert_existing_sticky_refused("derived archive", false, |store| {
            store.archive_memory("legacy-sticky").map(|_| ())
        });
        assert_existing_sticky_refused("revision archive", false, |store| {
            store
                .archive_memory_if_revision("legacy-sticky", 1)
                .map(|_| ())
        });
        assert_existing_sticky_refused("restore", true, |store| {
            store
                .restore_archived_if_revision("legacy-sticky", 1)
                .map(|_| ())
        });
        assert_existing_sticky_refused("supersede", false, |store| {
            store
                .supersede_memory("legacy-sticky", "ordinary-target")
                .map(|_| ())
        });
        assert_existing_sticky_refused("revision supersede", false, |store| {
            store
                .supersede_memory_if_revision("legacy-sticky", "ordinary-target", 1)
                .map(|_| ())
        });
        assert_existing_sticky_refused("delete", false, |store| {
            store.delete("legacy-sticky").map(|_| ())
        });

        let mut ordinary = MemoryStore::open_in_memory().expect("open ordinary store");
        let entry = ordinary_entry("ordinary");
        ordinary.upsert(&entry).expect("seed ordinary row");
        assert!(ordinary
            .update_with_revision(
                "ordinary",
                "changed ordinary body",
                "changed ordinary summary",
                "manual",
                &serde_json::json!({"changed": true}),
                None,
                1,
            )
            .expect("ordinary content update"));
        assert!(ordinary
            .archive_memory("ordinary")
            .expect("ordinary archive"));
        assert!(ordinary.delete("ordinary").expect("ordinary delete"));
    }
}

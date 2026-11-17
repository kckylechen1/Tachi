//! Read-only metadata candidate lookups on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryStore};

pub use crate::db::{CategoryPathPrefixMemoryRow, PathPrefixMemoryRow};

impl MemoryStore {
    /// Body-free, read-only discovery in one consistent SQLite snapshot.
    /// Prefix matches are literal; the result is never deletion authority.
    pub fn inspect_maintenance_candidates(
        &self,
        prefix: &str,
        as_of: &str,
        max_age_days: u64,
    ) -> Result<db::MaintenanceCandidateReport, MemoryError> {
        let tx = self.conn.unchecked_transaction()?;
        let report = db::maintenance_candidates(&tx, self.profile, prefix, as_of, max_age_days)?;
        tx.commit()?;
        Ok(report)
    }

    /// Recheck lifecycle, retention and references and canonically delete only
    /// eligible Kanban rows under the same immediate transaction.
    pub fn gc_expired_kanban_cards(
        &mut self,
        as_of: &str,
        max_age_days: u64,
    ) -> Result<usize, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::sweep_kanban_gc(
            &mut self.conn,
            self.profile,
            self.vec_available,
            as_of,
            max_age_days,
        )
    }

    /// List `memories` rows whose `path` matches a SQL `LIKE` pattern (caller
    /// supplies the full pattern, e.g. `"{prefix}%"`). Used by GC sweeps that
    /// scope by path namespace (e.g. kanban).
    pub fn list_memories_by_path_prefix(
        &self,
        path_like_pattern: &str,
    ) -> Result<Vec<PathPrefixMemoryRow>, MemoryError> {
        db::list_memories_by_path_prefix(&self.conn, path_like_pattern)
    }

    /// Find exact string metadata identities without collecting a whole
    /// subtree's metadata in the caller. Every predicate is required; missing
    /// and non-string values do not match. Paths are literal prefixes, not LIKE
    /// patterns. Includes archived rows so the host's typed get retains its
    /// existing active-row policy and legacy duplicate resolution.
    ///
    /// All admitted-prefix metadata is parsed with serde_json, even on a
    /// nonmatching key. Corruption therefore remains an error rather than a
    /// false miss. Only matching rows cross the API; bodies/vectors are not read.
    /// This still scans/parses the prefix: it is not an indexed lookup.
    pub fn find_metadata_key_candidates(
        &self,
        path_prefix: &str,
        predicates: &[(&str, &str)],
        max_matches: usize,
    ) -> Result<Vec<PathPrefixMemoryRow>, MemoryError> {
        if path_prefix.is_empty()
            || predicates.is_empty()
            || predicates.len() > 16
            || !(1..=4096).contains(&max_matches)
        {
            return Err(MemoryError::InvalidArg(
                "invalid metadata key lookup".into(),
            ));
        }
        let mut statement = self.conn.prepare(
            "SELECT id, path, category, timestamp, metadata FROM memories
             WHERE substr(path, 1, length(?1)) COLLATE BINARY = ?1",
        )?;
        let mut rows = statement.query([path_prefix])?;
        read_key_candidates(&mut rows, path_prefix, predicates, max_matches)
    }

    /// List `memories` rows matching an exact `category` and a `path` `LIKE`
    /// pattern. Used by GC sweeps that scope by category + path namespace
    /// (e.g. handoff memos).
    pub fn list_memories_by_category_and_path_prefix(
        &self,
        category: &str,
        path_like_pattern: &str,
    ) -> Result<Vec<CategoryPathPrefixMemoryRow>, MemoryError> {
        db::list_memories_by_category_and_path_prefix(&self.conn, category, path_like_pattern)
    }
}

fn read_key_candidates(
    rows: &mut rusqlite::Rows<'_>,
    path_prefix: &str,
    predicates: &[(&str, &str)],
    max_matches: usize,
) -> Result<Vec<PathPrefixMemoryRow>, MemoryError> {
    let mut matches = Vec::new();
    while let Some(row) = rows.next()? {
        let path: String = row.get(1)?;
        if !path.starts_with(path_prefix) {
            continue;
        }
        let metadata: String = row.get(4)?;
        let value: serde_json::Value = serde_json::from_str(&metadata)
            .map_err(|_| MemoryError::InvalidArg("invalid key-candidate metadata".into()))?;
        if !predicates
            .iter()
            .all(|(field, expected)| value.get(*field).and_then(|v| v.as_str()) == Some(*expected))
        {
            continue;
        }
        if matches.len() == max_matches {
            return Err(MemoryError::InvalidArg(
                "metadata key matches exceed budget".into(),
            ));
        }
        matches.push(PathPrefixMemoryRow {
            id: row.get(0)?,
            path,
            category: row.get(2)?,
            timestamp: row.get(3)?,
            metadata,
        });
    }
    matches.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| b.timestamp.cmp(&a.timestamp))
    });
    Ok(matches)
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn corrupt_nonmatching_metadata_in_the_literal_scope_remains_an_error() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // SQL-row fixture only: it drives the same decoder used by the public
        // store method. The portable suite covers real kernel stores separately.
        conn.execute_batch(
            r#"CREATE TABLE candidates(id TEXT, path TEXT, category TEXT, timestamp TEXT, metadata TEXT);
             INSERT INTO candidates VALUES ('outside','/other/row','fact','2026','{bad');
             INSERT INTO candidates VALUES ('match','/agents/a/one','fact','2026',
                 '{"key":"wrong","key":"wanted"}');"#,
        )
        .unwrap();
        let read = || {
            let mut stmt = conn.prepare("SELECT * FROM candidates").unwrap();
            let mut rows = stmt.query([]).unwrap();
            read_key_candidates(&mut rows, "/agents/a/", &[("key", "wanted")], 8)
        };
        assert_eq!(read().unwrap()[0].id, "match");
        conn.execute(
            "INSERT INTO candidates VALUES ('corrupt','/agents/a/other','fact','2026','{bad')",
            [],
        )
        .unwrap();
        assert!(read().is_err());
    }
}

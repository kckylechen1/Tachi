//! Opening, construction, and write-path entry points for [`MemoryStore`].

use rusqlite::Connection;

use crate::{db, db::DbOpenContext, error::MemoryError, path_router, MemoryEntry, MemoryStore};

/// Test/operator escape hatch: when set to a truthy value, the path-routing
/// validation in `MemoryStore::upsert` is bypassed entirely. Useful for test
/// fixtures that intentionally write across the canonical layout.
fn path_validation_disabled() -> bool {
    matches!(
        std::env::var("TACHI_DISABLE_PATH_VALIDATION")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

impl MemoryStore {
    /// Open (or create) a memory database at the given path.
    ///
    /// Uses the fail-closed default [`DbOpenContext`] (`OpenExisting + Deny`):
    /// a fresh file builds, a current DB opens, but a *stamped older* DB
    /// refuses to migrate in place (#1119). Callers that are the intended
    /// deploy-time migrator, or that are provisioning a brand-new DB, use
    /// [`Self::open_with_context`] with an explicit context.
    pub fn open(db_path: &str) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(db_path, "unknown", false, &DbOpenContext::default())
    }

    /// Open (or create) a memory database with a known manifest label.
    /// Enables path-routing validation at write time and runs data migrations.
    ///
    /// Uses the fail-closed default [`DbOpenContext`] (`OpenExisting + Deny`);
    /// see [`Self::open`] and [`Self::open_with_label_and_context`].
    pub fn open_with_label(db_path: &str, db_label: &str) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(db_path, db_label, true, &DbOpenContext::default())
    }

    /// Open (or create) with an explicit [`DbOpenContext`] and no manifest
    /// label (path-routing validation disabled, like [`Self::open`]).
    pub fn open_with_context(db_path: &str, ctx: &DbOpenContext) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(db_path, "unknown", false, ctx)
    }

    /// Open (or create) with an explicit manifest label AND an explicit
    /// [`DbOpenContext`] — the entry point the deploy-time migrator and
    /// fresh-provisioning flows use to thread migration authority / open
    /// intent down into the #1119 gate.
    pub fn open_with_label_and_context(
        db_path: &str,
        db_label: &str,
        ctx: &DbOpenContext,
    ) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(db_path, db_label, true, ctx)
    }

    fn open_with_label_inner(
        db_path: &str,
        db_label: &str,
        path_validation: bool,
        ctx: &DbOpenContext,
    ) -> Result<Self, MemoryError> {
        // Register extensions BEFORE opening the connection.
        crate::db::enable_simple_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        // Acquire the in-process startup lock BEFORE the #1132 rename-on-open
        // migration, not after (RESIDUAL-1). The migration's stat+rename+symlink
        // sequence and the `open_read_write` below (which CREATES the canonical
        // file when absent) must not interleave: without this ordering, two
        // threads in THIS process could both stat the canonical name as absent
        // and both `rename` the legacy file onto it, the second clobbering the
        // first. Holding the guard across migrate + open serializes them.
        //
        // SCOPE NOTE: `acquire_startup_lock` is a process-local `Mutex`, so it
        // serializes the same-process race only. The remaining CROSS-process
        // TOCTOU (a SECOND daemon opening the same store dir: stat-absent here,
        // `Connection::open` creates+populates there, our rename would clobber
        // it) is closed inside `migrate_legacy_filename_if_present` itself, which
        // now migrates via an ATOMIC NO-CLOBBER rename (`renamex_np`/`renameat2`,
        // #1226) that fails with EEXIST rather than overwriting a concurrently
        // created canonical file. On kernels/filesystems/platforms without that
        // primitive it FAILS CLOSED (loud error) rather than degrading to a
        // plain rename, which would reopen the very clobber race it closes.
        let _startup_guard = db::acquire_startup_lock();
        // #1132: one-time rename-on-open migration away from the legacy
        // `memory.db` filename, before the connection is opened. Single seam —
        // see `db::filename`'s doc comment for why it lives here and not
        // scattered across every call site that builds a `db_path`.
        db::migrate_legacy_filename_if_present(std::path::Path::new(db_path))?;
        let mut conn = db::open_read_write(db_path)?;
        let reserved_reference_write = db::register_reserved_reference_write_guard(&conn)?;
        // Both labelled and unlabelled opens run schema init + data migrations
        // through init_schema_with_label_mut so the pre-migration backup and
        // post-migration fingerprint marker apply uniformly. Previously the
        // path_validation=false branch called init_schema directly, skipping
        // backups for all CLI/open_cli_store paths (#597 CP1).
        let p = std::path::PathBuf::from(db_path);
        let migration_authorization =
            db::authorize_reserved_reference_write(&reserved_reference_write)?;
        let schema_result = db::init_schema_with_label_mut(&mut conn, db_label, &p, ctx);
        drop(migration_authorization);
        let _ = schema_result?;
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
            reserved_reference_write,
            vec_available,
            db_label: db_label.to_string(),
            path_validation,
        })
    }

    /// Open an existing memory database for pure read-only operations.
    ///
    /// This intentionally skips schema initialization because init paths can
    /// write. Use it for CLI search/stats and other diagnostics that must work
    /// even when the DB file is not writable.
    ///
    /// Still enforces the #984 schema-version hard-fail gate (F2): a DB
    /// stamped newer than this kernel's `EXPECTED_SCHEMA_VERSION` is rejected
    /// immediately after opening, before any read touches it — read-only
    /// callers (CLI diagnostics, the server's read pool) must not be able to
    /// silently read columns/rows a newer kernel wrote and this kernel
    /// doesn't understand. An *older*-stamped DB is fine here: read-only opens
    /// never run migrations, so there's nothing to bring forward, and old
    /// data must stay readable by newer kernels.
    ///
    /// This does not affect `crate::db::doctor_probe`, which never routes
    /// through `MemoryStore` — it opens raw `rusqlite::Connection`s directly
    /// for legacy/foreign/possibly-corrupt files, by design (see that
    /// module's doc comment).
    pub fn open_read_only(db_path: &str) -> Result<Self, MemoryError> {
        crate::db::enable_simple_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn = db::open_read_only(db_path)?;
        let reserved_reference_write = db::register_reserved_reference_write_guard(&conn)?;
        db::migrations::check_schema_version_gate(&conn)?;
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
            reserved_reference_write,
            vec_available,
            db_label: "unknown".to_string(),
            path_validation: false,
        })
    }

    /// Open an existing database for a narrowly-scoped maintenance write.
    /// This never creates a file, initializes schema, or runs migrations; it
    /// does refresh the reserved-reference safety triggers before exposing a
    /// write-capable connection.
    pub fn open_existing_read_write(db_path: &str) -> Result<Self, MemoryError> {
        crate::db::enable_simple_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn =
            Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db::configure_connection(&conn)?;
        let reserved_reference_write = db::register_reserved_reference_write_guard(&conn)?;
        let stored = db::migrations::read_schema_version(&conn)?;
        if stored != db::migrations::EXPECTED_SCHEMA_VERSION {
            return Err(MemoryError::InvalidArg(format!(
                "exact-dedupe apply requires schema {}, found {stored}",
                db::migrations::EXPECTED_SCHEMA_VERSION
            )));
        }
        // A version stamp is not proof of shape. Prepare the complete memories
        // projection exact-dedupe reads and writes before returning a writable
        // handle; this validates only and deliberately performs no
        // init/migration.
        conn.prepare(
            "SELECT id,path,text,revision,retention_policy,tier,query_diversity,recall_count,access_count,metadata,archived,superseded_by,valid_until,updated_at FROM memories WHERE 0",
        )
        .map_err(|error| {
            MemoryError::InvalidArg(format!(
                "exact-dedupe apply requires current memories schema: {error}"
            ))
        })?;
        db::install_reserved_reference_guard(&conn)?;
        // Registration above makes vec0 available to this connection, but a
        // maintenance open must not create its virtual table. Preparing a
        // read-only query proves the already-existing table and module are
        // usable; a missing or unloadable memories_vec simply disables vector
        // evidence for this apply.
        let vec_available = conn.prepare("SELECT id FROM memories_vec LIMIT 0").is_ok();
        Ok(Self {
            conn,
            reserved_reference_write,
            vec_available,
            db_label: "unknown".to_string(),
            path_validation: false,
        })
    }

    /// In-memory database (useful for tests and scripts).
    pub fn open_in_memory() -> Result<Self, MemoryError> {
        crate::db::enable_simple_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        db::configure_connection(&conn)?;
        let reserved_reference_write = db::register_reserved_reference_write_guard(&conn)?;
        let migration_authorization =
            db::authorize_reserved_reference_write(&reserved_reference_write)?;
        let schema_result = db::init_schema(&conn);
        drop(migration_authorization);
        schema_result?;
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
            reserved_reference_write,
            vec_available,
            db_label: "unknown".to_string(),
            path_validation: false,
        })
    }

    fn validate_write_path(&self, entry: &MemoryEntry) -> Result<(), MemoryError> {
        if self.path_validation && !path_validation_disabled() {
            let allow_cross = entry
                .metadata
                .get("allow_cross_project")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Err(e) =
                path_router::validate_path_for_db(&entry.path, &self.db_label, allow_cross)
            {
                eprintln!(
                    "warning: path-routing validation rejected write db_label={} path={} error={}",
                    self.db_label, entry.path, e
                );
                return Err(MemoryError::InvalidArg(e.to_string()));
            }
        }
        Ok(())
    }

    /// Insert or update a memory entry (with optional embedding vector).
    pub fn upsert(&mut self, entry: &MemoryEntry) -> Result<(), MemoryError> {
        self.validate_write_path(entry)?;
        let db_label = self.db_label.clone();
        db::retry_memory_locked("upsert", &db_label, || {
            db::upsert(&mut self.conn, entry, self.vec_available)
        })
    }

    /// Atomically insert a memory and all of its search projections, without
    /// rewriting an existing id.
    pub fn insert_if_absent(
        &mut self,
        entry: &MemoryEntry,
    ) -> Result<db::InsertMemoryResult, MemoryError> {
        self.validate_write_path(entry)?;
        let db_label = self.db_label.clone();
        db::retry_memory_locked("insert_if_absent", &db_label, || {
            db::insert_if_absent(&mut self.conn, entry, self.vec_available)
        })
    }

    /// Atomically save a modern id-less entry. The DB identity chooses a
    /// winner, while legacy rows without an identity remain untouched.
    pub fn upsert_idless(
        &mut self,
        entry: &MemoryEntry,
        identity: &str,
    ) -> Result<db::IdlessUpsertResult, MemoryError> {
        self.validate_write_path(entry)?;
        let db_label = self.db_label.clone();
        db::retry_memory_locked("upsert_idless", &db_label, || {
            db::upsert_idless(&mut self.conn, entry, self.vec_available, identity)
        })
    }
}

#[cfg(test)]
mod exact_dedupe_open_tests {
    use super::*;

    #[test]
    fn existing_read_write_does_not_recreate_missing_memories_vec() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current.db");
        let store = MemoryStore::open(&path.to_string_lossy()).unwrap();
        store
            .connection()
            .execute("DROP TABLE memories_vec", [])
            .unwrap();
        drop(store);

        let maintenance = MemoryStore::open_existing_read_write(&path.to_string_lossy()).unwrap();
        assert!(!maintenance.vec_available);
        let table_count: i64 = maintenance
            .connection()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='memories_vec'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 0);
    }

    #[test]
    fn existing_read_write_refreshes_reserved_reference_guards() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current.db");
        let mut store = MemoryStore::open(&path.to_string_lossy()).unwrap();
        let entry: MemoryEntry = serde_json::from_value(serde_json::json!({
            "id": "maintenance-guard",
            "text": "maintenance guard fixture",
            "timestamp": "2026-07-25T00:00:00Z"
        }))
        .unwrap();
        let reference = db::ValidatedReferenceMutation::evidence(
            "#100".to_string(),
            "2026-07-25T00:00:00Z".to_string(),
            None,
        )
        .unwrap();
        store
            .upsert_with_validated_reference_mutations(
                &entry,
                None,
                &serde_json::Map::new(),
                &[reference],
            )
            .unwrap();
        store
            .connection()
            .execute_batch(
                "DROP TRIGGER memories_reserved_refs_insert_guard;
                 DROP TRIGGER memories_reserved_refs_update_guard;",
            )
            .unwrap();
        drop(store);

        let maintenance = MemoryStore::open_existing_read_write(&path.to_string_lossy()).unwrap();
        let erase = maintenance.connection().execute(
            "UPDATE memories SET metadata = '{}' WHERE id = 'maintenance-guard'",
            [],
        );
        assert!(
            erase.is_err(),
            "maintenance open left reserved refs unguarded"
        );
    }

    #[test]
    fn existing_read_write_rejects_spoofed_current_version_without_initializing_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spoofed.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE decoy(value TEXT); PRAGMA user_version = {}",
            db::migrations::EXPECTED_SCHEMA_VERSION
        ))
        .unwrap();
        drop(conn);

        let error = match MemoryStore::open_existing_read_write(&path.to_string_lossy()) {
            Ok(_) => panic!("spoofed schema was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("current memories schema"));

        let conn = Connection::open(&path).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(tables, vec!["decoy"]);
    }
}

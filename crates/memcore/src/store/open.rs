//! Opening, construction, and write-path entry points for [`MemoryStore`].

use rusqlite::Connection;

use crate::{db, db::DbOpenContext, error::MemoryError, path_router, MemoryEntry, MemoryStore};

/// Dry-run operation whose read schema must be proven before a compatibility
/// handle is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyBackfillOperation {
    Vectors,
    Summaries,
    Metadata,
    Fts,
}

impl ReadOnlyBackfillOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Vectors => "vector dry-run",
            Self::Summaries => "summary dry-run",
            Self::Metadata => "metadata dry-run",
            Self::Fts => "FTS dry-run",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SchemaColumn {
    name: String,
    hidden: i64,
}

fn schema_object_kind(conn: &Connection, name: &str) -> Result<Option<String>, MemoryError> {
    match conn.query_row(
        "SELECT type
         FROM pragma_table_list
         WHERE schema = 'main' AND name = ?1",
        [name],
        |row| row.get(0),
    ) {
        Ok(kind) => Ok(Some(kind)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn schema_columns(conn: &Connection, table: &str) -> Result<Vec<SchemaColumn>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT name, hidden
         FROM pragma_table_xinfo(?1)
         ORDER BY cid",
    )?;
    let columns = statement
        .query_map([table], |row| {
            Ok(SchemaColumn {
                name: row.get(0)?,
                hidden: row.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from)?;
    Ok(columns)
}

fn require_schema_object_kind(
    conn: &Connection,
    operation: ReadOnlyBackfillOperation,
    name: &str,
    expected_kind: &str,
    description: &str,
) -> Result<(), MemoryError> {
    let actual_kind = schema_object_kind(conn, name)?;
    if actual_kind.as_deref() == Some(expected_kind) {
        return Ok(());
    }
    let actual = actual_kind.as_deref().unwrap_or("missing");
    Err(MemoryError::InvalidArg(format!(
        "{} requires {description} '{name}'; found {actual}",
        operation.label()
    )))
}

fn require_shadow_objects(
    conn: &Connection,
    operation: ReadOnlyBackfillOperation,
    shape: &str,
    objects: &[(&str, &str)],
) -> Result<(), MemoryError> {
    for (name, expected_kind) in objects {
        let actual_kind = schema_object_kind(conn, name)?;
        if actual_kind.as_deref() != Some(*expected_kind) {
            let actual = actual_kind.as_deref().unwrap_or("missing");
            return Err(MemoryError::InvalidArg(format!(
                "{} requires {shape}: object '{name}' must be {expected_kind}, found {actual}",
                operation.label()
            )));
        }
    }
    Ok(())
}

fn require_vec0_backing_objects(
    conn: &Connection,
    operation: ReadOnlyBackfillOperation,
) -> Result<(), MemoryError> {
    for (name, expected_columns) in [
        ("memories_vec_chunks", 4_i64),
        ("memories_vec_info", 2),
        ("memories_vec_rowids", 4),
        ("memories_vec_vector_chunks00", 2),
    ] {
        let object = conn.query_row(
            "SELECT type, ncol
             FROM pragma_table_list
             WHERE schema = 'main' AND name = ?1",
            [name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        );
        let (kind, columns) = match object {
            Ok(object) => object,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(MemoryError::InvalidArg(format!(
                    "{} requires vec0 memories_vec shape: backing object '{name}' is missing",
                    operation.label()
                )))
            }
            Err(error) => return Err(error.into()),
        };
        if !matches!(kind.as_str(), "shadow" | "table") || columns != expected_columns {
            return Err(MemoryError::InvalidArg(format!(
                "{} requires vec0 memories_vec shape: backing object '{name}' has type {kind} and {columns} columns",
                operation.label()
            )));
        }
    }
    Ok(())
}

fn require_visible_columns(
    conn: &Connection,
    operation: ReadOnlyBackfillOperation,
    table: &str,
    required: &[&str],
) -> Result<(), MemoryError> {
    let columns = schema_columns(conn, table)?;
    for required_name in required {
        if !columns
            .iter()
            .any(|column| column.hidden == 0 && column.name == *required_name)
        {
            return Err(MemoryError::InvalidArg(format!(
                "read-only backfill compatibility requires memories schema for {}: missing visible column '{required_name}'",
                operation.label()
            )));
        }
    }
    Ok(())
}

fn require_exact_virtual_shape(
    conn: &Connection,
    operation: ReadOnlyBackfillOperation,
    table: &str,
    shape: &str,
    expected: &[(&str, i64)],
) -> Result<(), MemoryError> {
    let columns = schema_columns(conn, table)?;
    let expected: Vec<SchemaColumn> = expected
        .iter()
        .map(|(name, hidden)| SchemaColumn {
            name: (*name).to_string(),
            hidden: *hidden,
        })
        .collect();
    if columns == expected {
        return Ok(());
    }
    Err(MemoryError::InvalidArg(format!(
        "{} requires {shape}: unexpected table_xinfo shape",
        operation.label()
    )))
}

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

/// Filesystem presence does not distinguish an operational database from an
/// empty path reservation. Only an unstamped database with no application
/// schema may enter ordinary initialization and install the canonical guards.
fn has_existing_application_schema(conn: &Connection) -> Result<bool, MemoryError> {
    if db::migrations::read_schema_version(conn)? != 0 {
        return Ok(true);
    }

    let application_objects: i64 = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM main.sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'
         )",
        [],
        |row| row.get(0),
    )?;
    Ok(application_objects != 0)
}

/// Prove that the compatibility handle can serve the selected read-only
/// backfill before exposing it. SQLite's table-list and extended-column
/// metadata distinguish ordinary tables, views, virtual tables, and each
/// module's hidden/shadow shape without interpreting stored DDL text.
fn validate_read_only_backfill_compat_schema(
    conn: &Connection,
    operation: ReadOnlyBackfillOperation,
) -> Result<(), MemoryError> {
    match schema_object_kind(conn, "memories")?.as_deref() {
        Some("table") => {}
        None => {
            return Err(MemoryError::InvalidArg(format!(
            "read-only backfill compatibility requires table 'memories' for {}; object is missing",
            operation.label()
        )))
        }
        Some(actual) => {
            return Err(MemoryError::InvalidArg(format!(
                "{} requires ordinary table 'memories'; found {actual}",
                operation.label()
            )))
        }
    }

    let memories_columns: &[&str] = match operation {
        ReadOnlyBackfillOperation::Vectors => &["id", "path", "source", "topic", "metadata"],
        ReadOnlyBackfillOperation::Summaries => &[
            "id", "path", "text", "summary", "revision", "scope", "category", "archived",
        ],
        ReadOnlyBackfillOperation::Metadata => &["id", "text", "summary", "revision", "keywords"],
        ReadOnlyBackfillOperation::Fts => &["id"],
    };
    require_visible_columns(conn, operation, "memories", memories_columns)?;

    match operation {
        ReadOnlyBackfillOperation::Vectors => {
            require_schema_object_kind(
                conn,
                operation,
                "memories_vec",
                "virtual",
                "virtual table",
            )?;
            require_exact_virtual_shape(
                conn,
                operation,
                "memories_vec",
                "vec0 memories_vec shape",
                &[("id", 0), ("embedding", 0), ("distance", 1), ("k", 1)],
            )?;
            require_vec0_backing_objects(conn, operation)?;
        }
        ReadOnlyBackfillOperation::Fts => {
            require_schema_object_kind(
                conn,
                operation,
                "memories_fts",
                "virtual",
                "virtual table",
            )?;
            require_exact_virtual_shape(
                conn,
                operation,
                "memories_fts",
                "fts5 memories_fts shape",
                &[
                    ("id", 0),
                    ("path", 0),
                    ("summary", 0),
                    ("text", 0),
                    ("keywords", 0),
                    ("entities", 0),
                    ("memories_fts", 1),
                    ("rank", 1),
                ],
            )?;
            require_shadow_objects(
                conn,
                operation,
                "fts5 memories_fts shape",
                &[
                    ("memories_fts_config", "shadow"),
                    ("memories_fts_content", "shadow"),
                    ("memories_fts_data", "shadow"),
                    ("memories_fts_docsize", "shadow"),
                    ("memories_fts_idx", "shadow"),
                ],
            )?;
        }
        ReadOnlyBackfillOperation::Summaries | ReadOnlyBackfillOperation::Metadata => {}
    }

    Ok(())
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
        db::install_reserved_reference_authorizer(&conn, Some(&reserved_reference_write))?;
        let existing_application_schema = has_existing_application_schema(&conn)?;
        let stored_schema_version = db::migrations::read_schema_version(&conn)?;
        // Missing guards are legitimate only before the versioned v23
        // migration (or on a truly empty fresh DB). Unexpected/tampered
        // definitions are always rejected. A partial unstamped application
        // schema remains strict and cannot claim fresh-build authority.
        let is_stamped_older_schema =
            (1..db::migrations::EXPECTED_SCHEMA_VERSION).contains(&stored_schema_version);
        let allow_missing_pre_migration = !existing_application_schema || is_stamped_older_schema;
        db::validate_persistent_trigger_inventory(&conn, !allow_missing_pre_migration)?;
        // Both labelled and unlabelled opens run schema init + data migrations
        // through init_schema_with_label_mut so the pre-migration backup and
        // post-migration fingerprint marker apply uniformly. Previously the
        // path_validation=false branch called init_schema directly, skipping
        // backups for all CLI/open_cli_store paths (#597 CP1).
        let p = std::path::PathBuf::from(db_path);
        let migration_authorization = db::authorize_schema_migration(&reserved_reference_write)?;
        let schema_result = db::init_schema_with_label_mut(&mut conn, db_label, &p, ctx);
        let vec_available = schema_result
            .as_ref()
            .map(|_| db::try_load_sqlite_vec(&conn))
            .unwrap_or(false);
        drop(migration_authorization);
        let _ = schema_result?;
        db::validate_persistent_trigger_inventory(&conn, true)?;
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
    /// doesn't understand. Older stamped DBs also refuse with the typed
    /// migration-required error: read-only opens never repair or migrate.
    ///
    /// This does not affect `crate::db::doctor_probe`, which never routes
    /// through `MemoryStore` — it opens raw `rusqlite::Connection`s directly
    /// for legacy/foreign/possibly-corrupt files, by design (see that
    /// module's doc comment).
    pub fn open_read_only(db_path: &str) -> Result<Self, MemoryError> {
        Self::open_read_only_inner(db_path, None)
    }

    /// Open an existing DB read-only while tolerating a stamped older schema.
    ///
    /// This is intentionally narrower than an ordinary compatibility open:
    /// the SQLite handle remains `SQLITE_OPEN_READ_ONLY`, no schema init or
    /// migration authority is granted, and a current-schema DB still requires
    /// the full canonical persistent-trigger inventory. It exists for dry-run
    /// operators that need to inspect a structurally valid pre-v23 DB without
    /// mutating its stamp or requiring migration approval. `operation` names
    /// the exact memories columns and optional virtual-table capability that
    /// must exist; unrelated optional capabilities are not required.
    pub fn open_read_only_existing_schema_compat(
        db_path: &str,
        operation: ReadOnlyBackfillOperation,
    ) -> Result<Self, MemoryError> {
        Self::open_read_only_inner(db_path, Some(operation))
    }

    fn open_read_only_inner(
        db_path: &str,
        compat_operation: Option<ReadOnlyBackfillOperation>,
    ) -> Result<Self, MemoryError> {
        crate::db::enable_simple_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn = db::open_read_only(db_path)?;
        let reserved_reference_write = db::register_reserved_reference_write_guard(&conn)?;
        db::install_reserved_reference_authorizer(&conn, Some(&reserved_reference_write))?;
        if compat_operation.is_some() {
            let raw_schema_version: i64 =
                conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if raw_schema_version < 0 {
                return Err(MemoryError::InvalidArg(format!(
                    "read-only backfill compatibility refuses negative PRAGMA user_version {raw_schema_version}"
                )));
            }
        }
        db::migrations::check_schema_version_gate(&conn)?;
        let stored_schema_version = db::migrations::read_schema_version(&conn)?;
        let is_stamped_older_schema =
            (1..db::migrations::EXPECTED_SCHEMA_VERSION).contains(&stored_schema_version);
        if is_stamped_older_schema {
            // Older legitimate DBs may lack guards, but an unexpected or
            // spoofed trigger is unsafe at every version and must remain loud.
            db::validate_persistent_trigger_inventory(&conn, false)?;
        } else {
            // Current DBs must retain every canonical guard; compatibility is
            // only for an older stamp, never a way to accept a damaged v23 DB.
            db::validate_persistent_trigger_inventory(&conn, true)?;
        }
        if compat_operation.is_none() || !is_stamped_older_schema {
            db::migrations::check_db_open_context_gate(
                &conn,
                std::path::Path::new(db_path),
                &DbOpenContext::open_existing_deny(),
            )?;
        }
        if let Some(operation) = compat_operation {
            validate_read_only_backfill_compat_schema(&conn, operation)?;
        }
        let vec_available = conn.prepare("SELECT id FROM memories_vec LIMIT 0").is_ok();
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
    /// refuses an incomplete trigger inventory before exposing a write-capable
    /// connection.
    pub fn open_existing_read_write(db_path: &str) -> Result<Self, MemoryError> {
        crate::db::enable_simple_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn =
            Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db::configure_connection(&conn)?;
        let reserved_reference_write = db::register_reserved_reference_write_guard(&conn)?;
        db::install_reserved_reference_authorizer(&conn, Some(&reserved_reference_write))?;
        let stored = db::migrations::read_schema_version(&conn)?;
        if stored != db::migrations::EXPECTED_SCHEMA_VERSION {
            return Err(MemoryError::InvalidArg(format!(
                "exact-dedupe apply requires schema {}, found {stored}",
                db::migrations::EXPECTED_SCHEMA_VERSION
            )));
        }
        db::validate_persistent_trigger_inventory(&conn, true)?;
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
        db::install_reserved_reference_authorizer(&conn, Some(&reserved_reference_write))?;
        db::validate_persistent_trigger_inventory(&conn, false)?;
        let migration_authorization = db::authorize_schema_migration(&reserved_reference_write)?;
        let schema_result = db::init_schema(&conn);
        let vec_available = schema_result
            .as_ref()
            .map(|_| db::try_load_sqlite_vec(&conn))
            .unwrap_or(false);
        drop(migration_authorization);
        schema_result?;
        db::validate_persistent_trigger_inventory(&conn, true)?;
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
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("upsert", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
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
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("insert_if_absent", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
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
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("upsert_idless", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::upsert_idless(&mut self.conn, entry, self.vec_available, identity)
        })
    }
}

#[cfg(test)]
mod exact_dedupe_open_tests {
    use super::*;

    fn open_compat_summaries(db_path: &str) -> Result<MemoryStore, MemoryError> {
        MemoryStore::open_read_only_existing_schema_compat(
            db_path,
            ReadOnlyBackfillOperation::Summaries,
        )
    }

    fn stamp_v22_without_v23_guards(conn: &Connection) {
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
             DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;
             DELETE FROM hard_state
              WHERE namespace = 'migrations'
                AND key = 'v23_reserved_reference_guards';
             PRAGMA user_version = 22;",
        )
        .unwrap();
    }

    fn test_memory_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/test/read-only-compat".to_string(),
            summary: String::new(),
            text: "read-only compatibility fixture".to_string(),
            importance: 0.5,
            timestamp: "2026-07-26T00:00:00Z".to_string(),
            valid_from: "2026-07-26T00:00:00Z".to_string(),
            valid_until: None,
            category: "fact".to_string(),
            topic: "test".to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: serde_json::json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn read_only_existing_schema_compat_opens_stamped_older_without_write_authority() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stamped-older.db");
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline
            .execute_batch(
                "DROP TRIGGER memories_reserved_refs_insert_guard;
                 DROP TRIGGER memories_reserved_refs_update_guard;
                 DELETE FROM hard_state
                  WHERE namespace = 'migrations'
                    AND key = 'v23_reserved_reference_guards';
                 PRAGMA user_version = 22;",
            )
            .unwrap();
        drop(offline);

        let mut store = open_compat_summaries(&path.to_string_lossy())
            .expect("stamped older DB must be inspectable without migration authority");
        assert_eq!(
            db::migrations::read_schema_version(store.connection()).unwrap(),
            22,
            "compatibility open must not rewrite the older stamp"
        );

        let error = store
            .upsert(&test_memory_entry("read-only-write-attempt"))
            .expect_err("compatibility open must remain read-only");
        assert!(
            error.to_string().to_ascii_lowercase().contains("readonly"),
            "unexpected read-only write refusal: {error}"
        );

        let offline = Connection::open(&path).unwrap();
        assert_eq!(
            db::migrations::read_schema_version(&offline).unwrap(),
            22,
            "rejected write must leave the older stamp unchanged"
        );
    }

    #[test]
    fn read_only_existing_schema_compat_rejects_version_only_v22_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("version-only-v22.db");
        let offline = Connection::open(&path).unwrap();
        offline.execute_batch("PRAGMA user_version = 22;").unwrap();
        drop(offline);

        let error = match open_compat_summaries(&path.to_string_lossy()) {
            Ok(_) => panic!("version-only v22 DB was accepted as backfill-compatible"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("read-only backfill compatibility requires table 'memories'"),
            "unexpected version-only v22 refusal: {error}"
        );
    }

    #[test]
    fn read_only_existing_schema_compat_rejects_malformed_v22_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("malformed-v22.db");
        let offline = Connection::open(&path).unwrap();
        offline
            .execute_batch(
                "CREATE TABLE memories(id TEXT PRIMARY KEY);
                 PRAGMA user_version = 22;",
            )
            .unwrap();
        drop(offline);

        let error = match open_compat_summaries(&path.to_string_lossy()) {
            Ok(_) => panic!("malformed v22 memories schema was accepted"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("read-only backfill compatibility requires memories schema"),
            "unexpected malformed v22 refusal: {error}"
        );
    }

    #[test]
    fn read_only_existing_schema_compat_rejects_negative_user_version_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("negative-version.db");
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline.execute_batch("PRAGMA user_version = -1;").unwrap();
        drop(offline);

        let error = match open_compat_summaries(&path.to_string_lossy()) {
            Ok(_) => panic!("negative user_version was normalized into an accepted v0 DB"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains(
                "read-only backfill compatibility refuses negative PRAGMA user_version -1"
            ),
            "unexpected negative-version refusal: {error}"
        );
    }

    #[test]
    fn read_only_existing_schema_compat_preserves_unstamped_current_shape_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unstamped-current-shape.db");
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline.execute_batch("PRAGMA user_version = 0;").unwrap();
        drop(offline);

        let store = open_compat_summaries(&path.to_string_lossy())
            .expect("documented v0 current-shape DB must remain readable");
        assert_eq!(
            db::migrations::read_schema_version(store.connection()).unwrap(),
            0,
            "read-only compatibility open must preserve the unstamped value"
        );
    }

    #[test]
    fn read_only_compat_operations_treat_missing_vector_as_optional_only_when_unused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v22-without-vector.db");
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline.execute("DROP TABLE memories_vec", []).unwrap();
        stamp_v22_without_v23_guards(&offline);
        drop(offline);

        for operation in [
            ReadOnlyBackfillOperation::Summaries,
            ReadOnlyBackfillOperation::Metadata,
        ] {
            let store = MemoryStore::open_read_only_existing_schema_compat(
                &path.to_string_lossy(),
                operation,
            )
            .unwrap_or_else(|error| {
                panic!("{operation:?} must not require optional vector capability: {error}")
            });
            assert_eq!(
                db::migrations::read_schema_version(store.connection()).unwrap(),
                22
            );
        }

        let error = match MemoryStore::open_read_only_existing_schema_compat(
            &path.to_string_lossy(),
            ReadOnlyBackfillOperation::Vectors,
        ) {
            Ok(_) => panic!("vector dry-run accepted a DB without memories_vec"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("vector dry-run requires virtual table 'memories_vec'"),
            "unexpected missing-vector refusal: {error}"
        );
    }

    #[test]
    fn read_only_compat_rejects_memories_view_with_required_column_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v22-memories-view.db");
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline
            .execute_batch(
                "DROP TABLE memories;
                 CREATE VIEW memories AS
                 SELECT '' AS id, '' AS path, '' AS source, '' AS topic,
                        '{}' AS metadata, '' AS text, '' AS summary,
                        1 AS revision, '[]' AS keywords, '' AS scope,
                        '' AS category, 0 AS archived
                 WHERE 0;",
            )
            .unwrap();
        stamp_v22_without_v23_guards(&offline);
        drop(offline);

        let error = match open_compat_summaries(&path.to_string_lossy()) {
            Ok(_) => panic!("a view impersonated the memories table"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("summary dry-run requires ordinary table 'memories'"),
            "unexpected memories-view refusal: {error}"
        );
    }

    #[test]
    fn read_only_compat_rejects_ordinary_tables_impersonating_virtual_capabilities() {
        for (table, replacement, operation, expected) in [
            (
                "memories_vec",
                "CREATE TABLE memories_vec(id TEXT PRIMARY KEY, embedding BLOB);",
                ReadOnlyBackfillOperation::Vectors,
                "vector dry-run requires virtual table 'memories_vec'",
            ),
            (
                "memories_fts",
                "CREATE TABLE memories_fts(
                    id TEXT, path TEXT, summary TEXT, text TEXT,
                    keywords TEXT, entities TEXT
                 );",
                ReadOnlyBackfillOperation::Fts,
                "FTS dry-run requires virtual table 'memories_fts'",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("ordinary-{table}.db"));
            drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

            let offline = Connection::open(&path).unwrap();
            offline
                .execute_batch(&format!("DROP TABLE {table}; {replacement}"))
                .unwrap();
            stamp_v22_without_v23_guards(&offline);
            drop(offline);

            let error = match MemoryStore::open_read_only_existing_schema_compat(
                &path.to_string_lossy(),
                operation,
            ) {
                Ok(_) => panic!("ordinary table impersonated {table}"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains(expected),
                "unexpected ordinary-{table} refusal: {error}"
            );
        }
    }

    #[test]
    fn read_only_compat_rejects_wrong_virtual_modules_and_shapes() {
        for (table, replacement, operation, expected) in [
            (
                "memories_vec",
                "CREATE VIRTUAL TABLE memories_vec USING fts5(id, embedding);",
                ReadOnlyBackfillOperation::Vectors,
                "vector dry-run requires vec0 memories_vec shape",
            ),
            (
                "memories_fts",
                "CREATE VIRTUAL TABLE memories_fts USING fts5(id);",
                ReadOnlyBackfillOperation::Fts,
                "FTS dry-run requires fts5 memories_fts shape",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("wrong-virtual-{table}.db"));
            drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

            let offline = Connection::open(&path).unwrap();
            offline
                .execute_batch(&format!("DROP TABLE {table}; {replacement}"))
                .unwrap();
            stamp_v22_without_v23_guards(&offline);
            drop(offline);

            let error = match MemoryStore::open_read_only_existing_schema_compat(
                &path.to_string_lossy(),
                operation,
            ) {
                Ok(_) => panic!("wrong virtual module/shape impersonated {table}"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains(expected),
                "unexpected wrong-{table} refusal: {error}"
            );
        }
    }

    #[test]
    fn existing_read_write_does_not_recreate_missing_memories_vec() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current.db");
        let store = MemoryStore::open(&path.to_string_lossy()).unwrap();
        let migration = db::authorize_schema_migration(&store.reserved_reference_write).unwrap();
        store
            .connection()
            .execute("DROP TABLE memories_vec", [])
            .unwrap();
        drop(migration);
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
    fn existing_opens_refuse_missing_reserved_reference_guards_before_repair() {
        type StoreOpener = fn(&str) -> Result<MemoryStore, MemoryError>;
        let openers: [(&str, StoreOpener); 3] = [
            ("ordinary", MemoryStore::open),
            ("maintenance", MemoryStore::open_existing_read_write),
            ("read-only-existing-schema-compat", open_compat_summaries),
        ];

        for (label, open) in openers {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("{label}.db"));
            drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

            let offline = Connection::open(&path).unwrap();
            offline
                .execute_batch(
                    "DROP TRIGGER memories_reserved_refs_insert_guard;
                     DROP TRIGGER memories_reserved_refs_update_guard;",
                )
                .unwrap();
            drop(offline);

            let error = match open(&path.to_string_lossy()) {
                Ok(_) => panic!("{label} open silently repaired missing evidence guards"),
                Err(error) => error,
            };
            assert!(
                error
                    .to_string()
                    .contains("required trigger 'memories_reserved_refs_insert_guard' is missing"),
                "unexpected {label} refusal: {error}"
            );

            let offline = Connection::open(&path).unwrap();
            let remaining: i64 = offline
                .query_row(
                    "SELECT COUNT(*) FROM main.sqlite_schema
                     WHERE type = 'trigger'
                       AND name IN (
                           'memories_reserved_refs_insert_guard',
                           'memories_reserved_refs_update_guard'
                       )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(remaining, 0, "{label} open repaired before refusing");
        }
    }

    #[test]
    fn partial_memories_schema_is_existing_and_refused_before_guard_repair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial-memories.db");
        let offline = Connection::open(&path).unwrap();
        offline
            .execute_batch(
                "CREATE TABLE memories(
                    id TEXT PRIMARY KEY,
                    metadata TEXT NOT NULL DEFAULT '{}'
                 );",
            )
            .unwrap();
        drop(offline);

        let error = match MemoryStore::open(&path.to_string_lossy()) {
            Ok(_) => panic!("partial memories schema was initialized and repaired"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("required trigger 'memories_reserved_refs_insert_guard' is missing"),
            "unexpected partial-schema refusal: {error}"
        );

        let offline = Connection::open(&path).unwrap();
        let schema: Vec<(String, String)> = offline
            .prepare(
                "SELECT type, name FROM main.sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%'
                 ORDER BY type, name",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            schema,
            vec![("table".to_string(), "memories".to_string())],
            "refused open must not initialize or repair partial schema"
        );
        assert_eq!(db::migrations::read_schema_version(&offline).unwrap(), 0);
    }

    #[test]
    fn authorized_v22_migration_installs_missing_reference_guards() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized-migration.db");
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

        let offline = Connection::open(&path).unwrap();
        offline
            .execute_batch(
                "DROP TRIGGER memories_reserved_refs_insert_guard;
                 DROP TRIGGER memories_reserved_refs_update_guard;
                 DELETE FROM hard_state
                  WHERE namespace = 'migrations'
                    AND key = 'v23_reserved_reference_guards';
                 PRAGMA user_version = 22;",
            )
            .unwrap();
        drop(offline);

        let context = DbOpenContext::open_existing_allow("test:trigger-repair");
        let store = MemoryStore::open_with_context(&path.to_string_lossy(), &context)
            .expect("authorized migration repairs canonical guards");
        db::validate_persistent_trigger_inventory(store.connection(), true)
            .expect("authorized migration restores complete canonical inventory");
        assert_eq!(
            db::migrations::read_schema_version(store.connection()).unwrap(),
            db::migrations::EXPECTED_SCHEMA_VERSION
        );
    }

    #[test]
    fn current_schema_reopen_preserves_canonical_reference_guards() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current-reopen.db");
        drop(MemoryStore::open(&path.to_string_lossy()).unwrap());
        let offline = Connection::open(&path).unwrap();
        db::validate_persistent_trigger_inventory(&offline, true)
            .expect("fresh committed DB has canonical guards before reopen");
        drop(offline);

        let reopened = MemoryStore::open(&path.to_string_lossy())
            .expect("current schema with canonical guards must reopen");
        db::validate_persistent_trigger_inventory(reopened.connection(), true)
            .expect("current reopen preserves canonical guards");
    }

    #[test]
    fn open_refuses_unknown_or_spoofed_persistent_triggers() {
        for (label, trigger_sql) in [
            (
                "unknown-auxiliary",
                "CREATE TRIGGER malicious_access_chain
                 AFTER INSERT ON access_history
                 BEGIN
                   UPDATE memories SET path = '/wiki/persistent-forged';
                 END;",
            ),
            (
                "spoofed-guard-case",
                "DROP TRIGGER memories_reserved_refs_update_guard;
                 CREATE TRIGGER MeMoRiEs_ReSeRvEd_ReFs_UpDaTe_GuArD
                 AFTER UPDATE ON memories
                 BEGIN
                   UPDATE memories SET source = 'wiki' WHERE id = NEW.id;
                 END;",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("{label}.db"));
            drop(MemoryStore::open(&path.to_string_lossy()).unwrap());

            let offline = Connection::open(&path).unwrap();
            offline.execute_batch(trigger_sql).unwrap();
            drop(offline);

            for (opener_label, open) in [
                (
                    "ordinary",
                    MemoryStore::open as fn(&str) -> Result<MemoryStore, MemoryError>,
                ),
                ("read-only-existing-schema-compat", open_compat_summaries),
            ] {
                let error = match open(&path.to_string_lossy()) {
                    Ok(_) => panic!("persistent trigger {label} was exposed by {opener_label}"),
                    Err(error) => error,
                };
                assert!(
                    error
                        .to_string()
                        .contains("unsafe persistent trigger inventory"),
                    "unexpected {label}/{opener_label} refusal: {error}"
                );
            }
        }
    }

    #[test]
    fn fresh_schema_initialization_installs_exact_canonical_trigger_inventory() {
        let store = MemoryStore::open_in_memory().expect("initialize fresh store");
        let triggers: Vec<(String, String)> = store
            .connection()
            .prepare(
                "SELECT name, tbl_name
                 FROM main.sqlite_schema
                 WHERE type = 'trigger'
                 ORDER BY name",
            )
            .expect("prepare trigger inventory")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query trigger inventory")
            .collect::<Result<_, _>>()
            .expect("read trigger inventory");
        assert_eq!(
            triggers,
            vec![
                (
                    "memories_reserved_refs_insert_guard".to_string(),
                    "memories".to_string(),
                ),
                (
                    "memories_reserved_refs_update_guard".to_string(),
                    "memories".to_string(),
                ),
                (
                    "memory_access_search_generation_after_delete".to_string(),
                    "access_history".to_string(),
                ),
                (
                    "memory_access_search_generation_after_insert".to_string(),
                    "access_history".to_string(),
                ),
                (
                    "memory_access_search_generation_after_update".to_string(),
                    "access_history".to_string(),
                ),
                (
                    "memory_edge_search_generation_after_delete".to_string(),
                    "memory_edges".to_string(),
                ),
                (
                    "memory_edge_search_generation_after_insert".to_string(),
                    "memory_edges".to_string(),
                ),
                (
                    "memory_edge_search_generation_after_update".to_string(),
                    "memory_edges".to_string(),
                ),
                (
                    "memory_search_generation_after_delete".to_string(),
                    "memories".to_string(),
                ),
                (
                    "memory_search_generation_after_insert".to_string(),
                    "memories".to_string(),
                ),
                (
                    "memory_search_generation_after_update".to_string(),
                    "memories".to_string(),
                ),
            ],
            "fresh schema initialization must install the exact canonical trigger inventory"
        );
        db::validate_persistent_trigger_inventory(store.connection(), true)
            .expect("fresh schema trigger inventory must be canonical");
    }

    #[test]
    fn existing_read_write_rejects_spoofed_current_version_without_initializing_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spoofed.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE decoy(value TEXT);
             CREATE TABLE memories(id TEXT PRIMARY KEY, metadata TEXT NOT NULL DEFAULT '{{}}');
             PRAGMA user_version = {}",
            db::migrations::EXPECTED_SCHEMA_VERSION
        ))
        .unwrap();
        db::install_reserved_reference_guard(&conn).unwrap();
        drop(conn);

        let error = match MemoryStore::open_existing_read_write(&path.to_string_lossy()) {
            Ok(_) => panic!("spoofed schema was accepted"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("no such table: memory_search_generation"),
            "unexpected spoofed-schema refusal: {error}"
        );

        let conn = Connection::open(&path).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(tables, vec!["decoy", "memories"]);
    }
}

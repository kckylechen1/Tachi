//! Opening, construction, and write-path entry points for [`MemoryStore`].

use rusqlite::Connection;

use crate::{db, error::MemoryError, path_router, MemoryEntry, MemoryStore};

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
    pub fn open(db_path: &str) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(db_path, "unknown", false)
    }

    /// Open (or create) a memory database with a known manifest label.
    /// Enables path-routing validation at write time and runs data migrations.
    pub fn open_with_label(db_path: &str, db_label: &str) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(db_path, db_label, true)
    }

    fn open_with_label_inner(
        db_path: &str,
        db_label: &str,
        path_validation: bool,
    ) -> Result<Self, MemoryError> {
        // Register extensions BEFORE opening the connection.
        libsimple::enable_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let _startup_guard = db::acquire_startup_lock();
        let mut conn = db::open_read_write(db_path)?;
        if path_validation {
            let p = std::path::PathBuf::from(db_path);
            let _ = db::init_schema_with_label_mut(&mut conn, db_label, &p)?;
        } else {
            db::init_schema(&conn)?;
        }
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
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
    pub fn open_read_only(db_path: &str) -> Result<Self, MemoryError> {
        libsimple::enable_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn = db::open_read_only(db_path)?;
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
            vec_available,
            db_label: "unknown".to_string(),
            path_validation: false,
        })
    }

    /// In-memory database (useful for tests and scripts).
    pub fn open_in_memory() -> Result<Self, MemoryError> {
        libsimple::enable_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        db::configure_connection(&conn)?;
        db::init_schema(&conn)?;
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
            vec_available,
            db_label: "unknown".to_string(),
            path_validation: false,
        })
    }

    /// Insert or update a memory entry (with optional embedding vector).
    pub fn upsert(&mut self, entry: &MemoryEntry) -> Result<(), MemoryError> {
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
        db::retry_memory_locked(|| db::upsert(&mut self.conn, entry, self.vec_available))
    }
}

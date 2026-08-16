//! Opening, construction, and write-path entry points for [`MemoryStore`].

use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

use crate::{
    db,
    db::DbOpenContext,
    error::MemoryError,
    path_router::{self, UNKNOWN_DB_LABEL},
    MemoryEntry, MemoryStore,
};

#[cfg(feature = "test-support")]
struct StartupOwnershipHook {
    db_path: String,
    before_lock: Option<Box<dyn FnOnce() + Send + 'static>>,
    after_lock: Option<Box<dyn FnOnce() + Send + 'static>>,
}

#[cfg(feature = "test-support")]
static STARTUP_OWNERSHIP_HOOK: std::sync::Mutex<Option<StartupOwnershipHook>> =
    std::sync::Mutex::new(None);

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub struct StartupOwnershipHookGuard;

#[cfg(feature = "test-support")]
impl Drop for StartupOwnershipHookGuard {
    fn drop(&mut self) {
        *STARTUP_OWNERSHIP_HOOK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

#[cfg(feature = "test-support")]
fn take_startup_ownership_hook_for_tests(db_path: &str) -> Option<StartupOwnershipHook> {
    let mut slot = STARTUP_OWNERSHIP_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let matches = slot
        .as_ref()
        .is_some_and(|candidate| candidate.db_path == db_path);
    if matches {
        slot.take()
    } else {
        None
    }
}

#[cfg(unix)]
fn has_stable_unix_file_identity(device: u64, inode: u64) -> bool {
    device != 0 && inode != 0
}

fn physical_db_identity_at_path(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if has_stable_unix_file_identity(metadata.dev(), metadata.ino()) {
            return Some(format!("unix:{}:{}", metadata.dev(), metadata.ino()));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let (Some(volume), Some(index)) =
            (metadata.volume_serial_number(), metadata.file_index())
        {
            return Some(format!("windows:{volume}:{index}"));
        }
    }
    Some(format!(
        "path:{}",
        std::fs::canonicalize(path).ok()?.display()
    ))
}

fn physical_db_identity_is_stable(identity: &str) -> bool {
    identity.starts_with("unix:") || identity.starts_with("windows:")
}

fn physical_db_identity_at_open(db_path: &str) -> Option<String> {
    physical_db_identity_at_path(Path::new(db_path))
}

fn validate_physical_db_identity_across_open(
    db_path: &str,
    before_open: Option<String>,
) -> Result<Option<String>, MemoryError> {
    let after_open = physical_db_identity_at_open(db_path).ok_or_else(|| {
        MemoryError::InvalidArg(format!(
            "database path has no physical identity after open: {db_path}"
        ))
    })?;
    if let Some(before_open) = before_open {
        if before_open != after_open {
            return Err(MemoryError::InvalidArg(format!(
                "database path identity changed while opening: {db_path}"
            )));
        }
    }
    Ok(Some(after_open))
}

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
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn install_startup_ownership_hook_for_tests(
        db_path: &str,
        before_lock: impl FnOnce() + Send + 'static,
        after_lock: impl FnOnce() + Send + 'static,
    ) -> StartupOwnershipHookGuard {
        let mut slot = STARTUP_OWNERSHIP_HOOK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(slot.is_none(), "startup ownership hook already installed");
        *slot = Some(StartupOwnershipHook {
            db_path: db_path.to_string(),
            before_lock: Some(Box::new(before_lock)),
            after_lock: Some(Box::new(after_lock)),
        });
        StartupOwnershipHookGuard
    }

    /// Open (or create) a memory database at the given path.
    ///
    /// Uses the fail-closed default [`DbOpenContext`] (`OpenExisting + Deny`):
    /// a fresh file builds, a current DB opens, but a *stamped older* DB
    /// refuses to migrate in place (#1119). Callers that are the intended
    /// deploy-time migrator, or that are provisioning a brand-new DB, use
    /// [`Self::open_with_context`] with an explicit context.
    pub fn open(db_path: &str) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(
            db_path,
            UNKNOWN_DB_LABEL,
            false,
            &DbOpenContext::default(),
            None,
            false,
        )
    }

    /// Open (or create) a memory database with a known manifest label.
    /// Enables path-routing validation at write time and runs data migrations.
    ///
    /// Uses the fail-closed default [`DbOpenContext`] (`OpenExisting + Deny`);
    /// see [`Self::open`] and [`Self::open_with_label_and_context`].
    pub fn open_with_label(db_path: &str, db_label: &str) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(
            db_path,
            db_label,
            true,
            &DbOpenContext::default(),
            None,
            false,
        )
    }

    /// Open (or create) with an explicit [`DbOpenContext`] and no manifest
    /// label (path-routing validation disabled, like [`Self::open`]).
    pub fn open_with_context(db_path: &str, ctx: &DbOpenContext) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(db_path, UNKNOWN_DB_LABEL, false, ctx, None, false)
    }

    /// Open an existing store under an explicit SQLite busy budget while
    /// preserving the caller's typed migration authority. This is for
    /// one-shot write phases that must join their blocking writer before the
    /// process may construct a subsequent owner of the same database.
    ///
    /// The ordinary open APIs deliberately retain the repository-wide default
    /// busy timeout. Callers opt into a smaller budget only when the phase
    /// contract has an independently justified deadline.
    pub fn open_with_context_and_busy_timeout(
        db_path: &str,
        ctx: &DbOpenContext,
        busy_timeout: Duration,
    ) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(
            db_path,
            UNKNOWN_DB_LABEL,
            false,
            ctx,
            Some(busy_timeout),
            false,
        )
    }

    /// Open a full-profile store and persist one provider-key health row while
    /// retaining the same process startup ownership across both operations.
    ///
    /// SQLite auto-extension callbacks execute inside `Connection::open` and
    /// may read the database.  A provider-health writer that released startup
    /// ownership after open but before its upsert could therefore overlap the
    /// next open in this process.  This narrow entry point closes exactly that
    /// open-then-write gap; it is not a general write lock or retry wrapper.
    #[cfg(feature = "admin")]
    pub fn open_and_vault_upsert_key_health_with_context_and_busy_timeout(
        db_path: &str,
        ctx: &DbOpenContext,
        busy_timeout: Duration,
        health: &crate::vault::VaultKeyHealth,
    ) -> Result<(), MemoryError> {
        Self::with_open_store_and_busy_timeout(db_path, ctx, busy_timeout, |store| {
            store.vault_upsert_key_health(health)
        })
    }

    #[cfg(feature = "admin")]
    fn with_open_store_and_busy_timeout<T>(
        db_path: &str,
        ctx: &DbOpenContext,
        busy_timeout: Duration,
        operation: impl FnOnce(&Self) -> Result<T, MemoryError>,
    ) -> Result<T, MemoryError> {
        Self::register_open_extensions()?;
        #[cfg(feature = "test-support")]
        let mut startup_hook = take_startup_ownership_hook_for_tests(db_path);
        #[cfg(feature = "test-support")]
        if let Some(before_lock) = startup_hook
            .as_mut()
            .and_then(|hook| hook.before_lock.take())
        {
            // The hook is one-shot and path-bound. Reaching this callback proves
            // this exact open is about to contend for startup ownership.
            before_lock();
        }
        let _startup_guard = db::acquire_startup_lock();
        #[cfg(feature = "test-support")]
        if let Some(after_lock) = startup_hook.and_then(|mut hook| hook.after_lock.take()) {
            after_lock();
        }
        let store = Self::open_with_label_inner_while_startup_owned(
            db_path,
            UNKNOWN_DB_LABEL,
            false,
            ctx,
            Some(busy_timeout),
            false,
        )?;
        operation(&store)
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
        Self::open_with_label_inner(db_path, db_label, true, ctx, None, false)
    }

    pub(crate) fn open_private_working_file(
        db_path: &str,
        ctx: &DbOpenContext,
        identity: crate::private_partition::AdmittedPartition,
    ) -> Result<Self, MemoryError> {
        let mut store =
            Self::open_with_label_inner(db_path, "private_partition", false, ctx, None, true)?;
        crate::private_partition::stamp_private_identity(&store.conn, &identity)?;
        store.admitted_partition = Some(identity);
        Ok(store)
    }

    fn open_with_label_inner(
        db_path: &str,
        db_label: &str,
        path_validation: bool,
        ctx: &DbOpenContext,
        busy_timeout: Option<Duration>,
        allow_private_partition: bool,
    ) -> Result<Self, MemoryError> {
        Self::register_open_extensions()?;
        #[cfg(feature = "test-support")]
        let mut startup_hook = take_startup_ownership_hook_for_tests(db_path);
        #[cfg(feature = "test-support")]
        if let Some(before_lock) = startup_hook
            .as_mut()
            .and_then(|hook| hook.before_lock.take())
        {
            before_lock();
        }
        let _startup_guard = db::acquire_startup_lock();
        #[cfg(feature = "test-support")]
        if let Some(after_lock) = startup_hook.and_then(|mut hook| hook.after_lock.take()) {
            after_lock();
        }
        Self::open_with_label_inner_while_startup_owned(
            db_path,
            db_label,
            path_validation,
            ctx,
            busy_timeout,
            allow_private_partition,
        )
    }

    fn register_open_extensions() -> Result<(), MemoryError> {
        // Register extensions BEFORE opening the connection.
        crate::db::enable_simple_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        Ok(())
    }

    /// Open while the caller retains `db::acquire_startup_lock()` ownership.
    /// Keeping this private prevents unrelated callers from bypassing the
    /// process-wide startup boundary.
    fn open_with_label_inner_while_startup_owned(
        db_path: &str,
        db_label: &str,
        path_validation: bool,
        ctx: &DbOpenContext,
        busy_timeout: Option<Duration>,
        allow_private_partition: bool,
    ) -> Result<Self, MemoryError> {
        crate::private_partition::refuse_generic_open_path(db_path)?;
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
        // A caller-owned busy budget covers the whole synchronous open path,
        // including schema initialization's explicit retry sleeps. Without
        // this guard a small per-operation SQLite timeout could still be
        // extended by the retry loop after the local deadline had elapsed.
        let _busy_deadline = busy_timeout.map(db::scoped_sqlite_busy_deadline);
        // #1132: one-time rename-on-open migration away from the legacy
        // `memory.db` filename, before the connection is opened. Single seam —
        // see `db::filename`'s doc comment for why it lives here and not
        // scattered across every call site that builds a `db_path`.
        db::migrate_legacy_filename_if_present(std::path::Path::new(db_path))?;
        let physical_identity_before_open = physical_db_identity_at_open(db_path);
        let mut conn = match busy_timeout {
            Some(busy_timeout) => db::open_read_write_with_busy_timeout(db_path, busy_timeout)?,
            None => db::open_read_write(db_path)?,
        };
        let opened_physical_db_identity = validate_physical_db_identity_across_open(
            db_path,
            physical_identity_before_open.clone(),
        )?;
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
        // tachi#1579: the store's identity is what the schema transaction just
        // resolved from the stamp inside the file — NOT `db_label`, which is
        // only the caller's claim and may legitimately be `unknown`. A conflict
        // between the two already failed the open above.
        let identity = schema_result?.identity;
        if !allow_private_partition {
            crate::private_partition::refuse_stamped_private_store(&conn)?;
        }
        db::validate_persistent_trigger_inventory(&conn, true)?;
        let opened_physical_db_identity = validate_physical_db_identity_across_open(
            db_path,
            opened_physical_db_identity.clone(),
        )?;
        if physical_identity_before_open.is_none() {
            // A connection that created the path cannot prove which directory
            // entry its SQLite handle owns from a post-open path sample alone:
            // another process may have replaced the path in between. Close
            // that initializer and reopen the now-existing file with a
            // before/open/after identity bracket before returning a cacheable
            // handle. The reopen must match the exact identity sampled after
            // initialization, not merely any valid database concurrently
            // substituted at the same path.
            let initialized_identity = opened_physical_db_identity.clone().ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "fresh database has no initialized physical identity: {db_path}"
                ))
            })?;
            drop(reserved_reference_write);
            drop(conn);
            return Self::reopen_initialized_file_store(
                db_path,
                identity,
                path_validation,
                initialized_identity,
                busy_timeout,
            );
        }
        Ok(Self {
            conn,
            reserved_reference_write,
            vec_available,
            db_label: identity.db_label,
            profile: identity.profile,
            path_validation,
            opened_physical_db_identity,
            // tachi#1585 D5: pure default, no env. `with_kernel_policy`
            // attaches a host-injected policy after open.
            policy: crate::KernelPolicy::default(),
            admitted_partition: None,
        })
    }

    fn reopen_initialized_file_store(
        db_path: &str,
        identity: db::StoreIdentity,
        path_validation: bool,
        initialized_identity: String,
        busy_timeout: Option<Duration>,
    ) -> Result<Self, MemoryError> {
        let before_reopen = physical_db_identity_at_open(db_path).ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "fresh database path disappeared before identity-bound reopen: {db_path}"
            ))
        })?;
        if before_reopen != initialized_identity {
            return Err(MemoryError::InvalidArg(format!(
                "fresh database path identity changed before identity-bound reopen: {db_path}"
            )));
        }
        let conn = match busy_timeout {
            Some(busy_timeout) => db::open_read_write_with_busy_timeout(db_path, busy_timeout)?,
            None => db::open_read_write(db_path)?,
        };
        let reserved_reference_write = db::register_reserved_reference_write_guard(&conn)?;
        db::install_reserved_reference_authorizer(&conn, Some(&reserved_reference_write))?;
        db::migrations::check_schema_version_gate(&conn)?;
        let stored = db::migrations::read_schema_version(&conn)?;
        if stored != db::migrations::EXPECTED_SCHEMA_VERSION {
            return Err(MemoryError::InvalidArg(format!(
                "fresh database identity-bound reopen requires schema {}, found {stored}",
                db::migrations::EXPECTED_SCHEMA_VERSION
            )));
        }
        db::migrations::validate_current_schema_integrity(&conn)?;
        db::validate_persistent_trigger_inventory(&conn, true)?;
        let vec_available = db::try_load_sqlite_vec(&conn);
        let opened_physical_db_identity =
            validate_physical_db_identity_across_open(db_path, Some(before_reopen))?;
        // The identity resolved by the initializing transaction, carried across
        // the reopen rather than re-derived: re-reading the stamp here would
        // open a window in which another process's write is observed instead of
        // the one this open committed.
        Ok(Self {
            conn,
            reserved_reference_write,
            vec_available,
            db_label: identity.db_label,
            profile: identity.profile,
            path_validation,
            opened_physical_db_identity,
            policy: crate::KernelPolicy::default(),
            admitted_partition: None,
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
        Self::open_read_only_inner(db_path, None, UNKNOWN_DB_LABEL, false)
    }

    /// Open one existing DB through SQLite's immutable URI mode.
    ///
    /// This is the strict operator-plan boundary: it cannot create or update
    /// WAL/SHM sidecars. Because immutable mode intentionally ignores WAL, a
    /// non-empty WAL is refused rather than returning a stale preview.
    pub fn open_read_only_immutable(db_path: &str) -> Result<Self, MemoryError> {
        let wal_path = std::path::PathBuf::from(format!("{db_path}-wal"));
        if std::fs::metadata(&wal_path).is_ok_and(|metadata| metadata.len() != 0) {
            return Err(MemoryError::InvalidArg(format!(
                "immutable maintenance plan refuses non-empty WAL at {}",
                wal_path.display()
            )));
        }
        Self::open_read_only_inner(db_path, None, UNKNOWN_DB_LABEL, true)
    }

    /// [`Self::open_read_only`] for a caller that knows which store this is.
    ///
    /// tachi#1569: read stores used to be born `db_label: "unknown"`, so every
    /// read-side decision that should have been keyed on store identity had to
    /// be keyed on the shape of the request instead (`path_prefix`) — see
    /// `db::memory_crud::search`'s wiki clause. `db_label` must be the *same*
    /// label the write path uses for the same file (the manifest role, e.g.
    /// `global`/`wiki`/a project name), because predicates like
    /// [`Self::is_wiki_corpus_store`] and `path_router::validate_path_for_db`
    /// compare against one set of label constants for both sides.
    ///
    /// Path-routing validation stays off (as for every read-only open): it is
    /// a write-time guard, and a read-only SQLite handle cannot write anyway.
    pub fn open_read_only_with_label(db_path: &str, db_label: &str) -> Result<Self, MemoryError> {
        Self::open_read_only_inner(db_path, None, db_label, false)
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
        Self::open_read_only_inner(db_path, Some(operation), UNKNOWN_DB_LABEL, false)
    }

    fn open_read_only_inner(
        db_path: &str,
        compat_operation: Option<ReadOnlyBackfillOperation>,
        db_label: &str,
        immutable: bool,
    ) -> Result<Self, MemoryError> {
        crate::private_partition::refuse_generic_open_path(db_path)?;
        crate::db::enable_simple_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let physical_identity_before_open = physical_db_identity_at_open(db_path);
        let conn = if immutable {
            let path = std::path::Path::new(db_path);
            #[cfg(unix)]
            let bytes = {
                use std::os::unix::ffi::OsStrExt;
                path.as_os_str().as_bytes()
            };
            #[cfg(not(unix))]
            let owned = path.to_string_lossy().into_owned();
            #[cfg(not(unix))]
            let bytes = owned.as_bytes();
            let mut encoded = String::with_capacity(bytes.len());
            for &byte in bytes {
                if byte.is_ascii_alphanumeric()
                    || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~' | b':')
                {
                    encoded.push(char::from(byte));
                } else {
                    use std::fmt::Write as _;
                    write!(&mut encoded, "%{byte:02X}").expect("write URI escape to string");
                }
            }
            let uri = format!("file:{encoded}?mode=ro&immutable=1");
            Connection::open_with_flags(
                uri,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
            )?
        } else {
            db::open_read_only(db_path)?
        };
        let opened_physical_db_identity =
            validate_physical_db_identity_across_open(db_path, physical_identity_before_open)?;
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
        db::migrations::validate_current_schema_integrity(&conn)?;
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
        // tachi#1579: a read-only handle resolves identity from the stamp too,
        // so read-side predicates (`is_wiki_corpus_store`) and the write path
        // read ONE authority. It cannot stamp, so an unstamped store keeps the
        // #1569 behavior of honoring a declared role — once, loudly.
        let path = std::path::Path::new(db_path);
        let (stored_role, stored_profile) = db::store_identity::read_identity(&conn, path)?;
        if stored_role.is_none() && db_label != UNKNOWN_DB_LABEL {
            db::store_identity::warn_read_only_declared_unstamped_once(db_path, db_label);
        }
        let resolved_label =
            db::store_identity::resolve_role(stored_role.as_deref(), db_label, path)?;
        crate::private_partition::refuse_stamped_private_store(&conn)?;
        Ok(Self {
            conn,
            reserved_reference_write,
            vec_available,
            db_label: resolved_label,
            // An unstamped store read read-only is a pre-#1585 database, i.e.
            // full Tachi. Nothing is written, so this is a description, not an
            // adoption.
            profile: stored_profile.unwrap_or_default(),
            path_validation: false,
            opened_physical_db_identity,
            policy: crate::KernelPolicy::default(),
            admitted_partition: None,
        })
    }

    /// Open an existing database for a narrowly-scoped maintenance write.
    /// This never creates a file, initializes schema, or runs migrations; it
    /// refuses an incomplete trigger inventory before exposing a write-capable
    /// connection.
    pub fn open_existing_read_write(db_path: &str) -> Result<Self, MemoryError> {
        crate::private_partition::refuse_generic_open_path(db_path)?;
        crate::db::enable_simple_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let physical_identity_before_open = physical_db_identity_at_open(db_path);
        let conn =
            Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        let opened_physical_db_identity =
            validate_physical_db_identity_across_open(db_path, physical_identity_before_open)?;
        db::configure_connection(&conn)?;
        let reserved_reference_write = db::register_reserved_reference_write_guard(&conn)?;
        db::install_reserved_reference_authorizer(&conn, Some(&reserved_reference_write))?;
        db::migrations::check_schema_version_gate(&conn)?;
        let stored = db::migrations::read_schema_version(&conn)?;
        if stored != db::migrations::EXPECTED_SCHEMA_VERSION {
            return Err(MemoryError::InvalidArg(format!(
                "exact-dedupe apply requires schema {}, found {stored}",
                db::migrations::EXPECTED_SCHEMA_VERSION
            )));
        }
        db::migrations::validate_current_schema_integrity(&conn)?;
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
        // Unlabelled maintenance handle: it claims no role, so it resolves to
        // whatever the file is stamped with, or `unknown` when it is unstamped.
        let path = std::path::Path::new(db_path);
        let (stored_role, stored_profile) = db::store_identity::read_identity(&conn, path)?;
        crate::private_partition::refuse_stamped_private_store(&conn)?;
        Ok(Self {
            conn,
            reserved_reference_write,
            vec_available,
            db_label: stored_role.unwrap_or_else(|| UNKNOWN_DB_LABEL.to_string()),
            profile: stored_profile.unwrap_or_default(),
            path_validation: false,
            opened_physical_db_identity,
            policy: crate::KernelPolicy::default(),
            admitted_partition: None,
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
            db_label: UNKNOWN_DB_LABEL.to_string(),
            profile: crate::db::StoreProfile::default(),
            path_validation: false,
            opened_physical_db_identity: None,
            policy: crate::KernelPolicy::default(),
            admitted_partition: None,
        })
    }

    pub fn opened_physical_db_identity(&self) -> Option<&str> {
        self.opened_physical_db_identity.as_deref()
    }

    /// Whether this handle addresses the Wiki corpus database.
    ///
    /// tachi#1569: the single typed answer to "is this read touching the Wiki
    /// corpus", used by [`Self::search`] and the list routes to decide the
    /// Wiki row gate from *store identity* instead of from the shape of the
    /// request. `path_router::validate_path_for_db` asks the same question on
    /// the write side, through the same function.
    ///
    /// A store opened without a label (`open`, `open_read_only`,
    /// `open_in_memory`) answers `false` — an unlabelled handle is not proof
    /// of anything, and answering `true` there would silently filter rows for
    /// CLI diagnostics and fixtures that never asked.
    pub fn is_wiki_corpus_store(&self) -> bool {
        path_router::db_label_is_wiki_corpus(&self.db_label)
    }

    /// This store's resolved manifest role (tachi#1579).
    ///
    /// Stamp-derived: for a store carrying a `store_identity` role row this is
    /// that row, whatever the caller passed at open. `unknown` means the store
    /// has no stamp AND the caller declared nothing — never "we could not be
    /// bothered to look".
    pub fn db_label(&self) -> &str {
        &self.db_label
    }

    /// This store's effective schema profile (#1585): the stamped one, or the
    /// full profile for a pre-#1585 database that has not been stamped yet.
    pub fn store_profile(&self) -> crate::db::StoreProfile {
        self.profile
    }

    /// Verify that this already-open connection still addresses the physical
    /// database currently present at `db_path`.
    ///
    /// Long-lived runtime caches are keyed by path, but replacing that path
    /// does not retarget SQLite's existing file descriptor. Callers that must
    /// never report success against a detached database check this immediately
    /// before and after their operation.
    pub fn verify_opened_physical_db_identity(&self, db_path: &Path) -> Result<(), MemoryError> {
        let opened = self.opened_physical_db_identity.as_deref().ok_or_else(|| {
            MemoryError::InvalidArg(
                "opened store has no file-backed physical database identity".to_string(),
            )
        })?;
        let current = physical_db_identity_at_path(db_path).ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "database path has no current physical identity: {}",
                db_path.display()
            ))
        })?;
        if opened != current {
            return Err(MemoryError::InvalidArg(format!(
                "database path identity changed after open: {}",
                db_path.display()
            )));
        }
        if !physical_db_identity_is_stable(opened) {
            return Err(MemoryError::InvalidArg(format!(
                "database path has no stable physical identity for detached-handle verification: {}",
                db_path.display()
            )));
        }
        Ok(())
    }

    /// `pub(crate)` since tachi#1607 so the snapshot-import path in
    /// `store::snapshot_import` runs *this* check rather than carrying a copy
    /// of it — a second copy is how the escape hatch and the `db_label`
    /// routing rule drift apart.
    pub(crate) fn validate_write_path(&self, entry: &MemoryEntry) -> Result<(), MemoryError> {
        // Retirement is a global write boundary, not an ordinary routing
        // decision. Check it before the cross-project metadata bypass and
        // before the host-injected KernelPolicy escape hatch, and keep legacy
        // `/sticky` rows available only to the read/cutover SQL paths.
        if let Err(error) = path_router::validate_retired_sticky_write(&entry.path, &entry.category)
        {
            eprintln!(
                "warning: retired-memory write rejected db_label={} path={} category={} error={}",
                self.db_label, entry.path, entry.category, error
            );
            return Err(MemoryError::InvalidArg(error.to_string()));
        }
        // tachi#1585 D5: this store's `KernelPolicy::path_validation_escape_hatch`,
        // not a `TACHI_DISABLE_PATH_VALIDATION` env read.
        if self.path_validation && !self.policy.path_validation_escape_hatch {
            let allow_cross = entry
                .metadata
                .get("allow_cross_project")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Err(e) = path_router::validate_memory_write_for_db(
                &entry.path,
                &entry.category,
                &self.db_label,
                allow_cross,
            ) {
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

    /// Shared body for [`Self::upsert_batch`] and
    /// [`Self::upsert_batch_with_precommit`]: validate every entry's write
    /// path (tachi#1585 D5 path routing) and acquire the reserved-reference
    /// write authorization *before any write*, so a batch with an invalid
    /// entry never opens a transaction at all; then run the whole batch's
    /// main-row + FTS + vector projections through [`db::upsert_within_tx`]
    /// inside one `BEGIN IMMEDIATE` transaction, run `postcommit` while the
    /// writes are still rollbackable, and commit only if it succeeds. This
    /// is a private helper — the closure it takes is never part of a public
    /// method's signature, which is the entire reason `upsert_batch` can be
    /// ungated while `upsert_batch_with_precommit`'s closure-carrying public
    /// signature stays admin/test-gated (see that method's doc comment).
    ///
    /// `allow_reserved_anchor_ids` selects which `db` seam the per-row loop
    /// uses: `false` (every ordinary caller) refuses reserved `anchor:` ids
    /// inside the shared upsert body, `true` is the trusted whole-store-copy
    /// channel described on
    /// [`Self::upsert_batch_with_precommit_preserving_anchor_rows`].
    fn upsert_batch_in_tx<T, F>(
        &mut self,
        entries: &[MemoryEntry],
        allow_reserved_anchor_ids: bool,
        postcommit: F,
    ) -> Result<T, MemoryError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, MemoryError>,
    {
        for entry in entries {
            self.validate_write_path(entry)?;
        }
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for entry in entries {
            if allow_reserved_anchor_ids {
                db::upsert_within_tx_allowing_reserved_anchor_ids(
                    &tx,
                    entry,
                    self.vec_available,
                    None,
                )?;
            } else {
                db::upsert_within_tx(&tx, entry, self.vec_available, None)?;
            }
        }
        let output = postcommit(&tx)?;
        tx.commit()?;
        Ok(output)
    }

    /// Upsert a bounded batch in one transaction and run a caller-supplied
    /// pre-commit guard while every main/FTS/vector write is still rollbackable.
    ///
    /// This is for cross-resource maintenance that must validate or complete
    /// an external boundary before the database half becomes durable. The
    /// closure receives read access to the transaction for exact post-state
    /// accounting; any closure error drops the transaction without commit.
    ///
    /// Gated with the raw-`Connection` accessors (#1585 review round 4): the
    /// closure's `&Transaction` derefs to `&Connection`, which would hand the
    /// portable surface the same raw-SQL bypass of the `store_identity`
    /// write-once guards. Its only production caller is tachi-server's tidy
    /// migration (admin build), through the
    /// [`Self::upsert_batch_with_precommit_preserving_anchor_rows`] variant.
    ///
    /// Reserved-id refusals are not weakened by batching: since tachi#1602 a
    /// blank `id` and an `id` in the reserved `anchor:` namespace are refused
    /// inside the shared [`db::upsert_within_tx`] body this method runs per
    /// row, so a bad entry anywhere in the batch rolls the whole batch back
    /// exactly as single-row `upsert` refuses.
    #[cfg(any(feature = "admin", test))]
    pub fn upsert_batch_with_precommit<T, F>(
        &mut self,
        entries: &[MemoryEntry],
        precommit: F,
    ) -> Result<T, MemoryError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, MemoryError>,
    {
        self.upsert_batch_in_tx(entries, false, precommit)
    }

    /// Trusted whole-store-copy variant of
    /// [`Self::upsert_batch_with_precommit`]: identical behavior, except rows
    /// whose `id` is already in the reserved `anchor:` namespace are copied
    /// verbatim instead of refused (tachi#1602).
    ///
    /// This is the batch analogue of [`Self::upsert_wiki_operation_log`]: the
    /// reserved-namespace refusal is enforced at the shared seam for every
    /// ordinary caller, and exactly one named internal writer opts out. That
    /// writer is tachi-server's tidy migration, which copies *every* row of a
    /// source database into the target — its row selection is an unfiltered
    /// `SELECT ... FROM memories`, so an `anchor:` row in the source is an
    /// existing row being carried across, not a new anchor being minted
    /// behind `ensure_anchor`'s back. Every other reserved-namespace guard
    /// (`wiki-rem:`, the Wiki operation log) still applies to this variant.
    #[cfg(any(feature = "admin", test))]
    pub fn upsert_batch_with_precommit_preserving_anchor_rows<T, F>(
        &mut self,
        entries: &[MemoryEntry],
        precommit: F,
    ) -> Result<T, MemoryError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, MemoryError>,
    {
        self.upsert_batch_in_tx(entries, true, precommit)
    }

    /// Upsert a bounded batch atomically, with **no handle exposure**: unlike
    /// [`Self::upsert_batch_with_precommit`], this takes no closure and its
    /// signature never mentions a raw `Connection`/`Transaction`, so it is
    /// safe to leave ungated for the portable build (tachi#1599).
    ///
    /// Every entry's `KernelPolicy`-driven write-path check
    /// ([`Self::validate_write_path`] — the same per-row check ordinary
    /// [`Self::upsert`] runs) and the reserved-reference write authorization
    /// are done before any row is written — an invalid entry anywhere in the
    /// batch means zero writes happen, not a partial batch. The pre-walk
    /// below also refuses, before any transaction opens, a blank/empty `id`
    /// and an `id` in the reserved `anchor:` namespace (reserved for
    /// `memcore::db::anchor::ensure_anchor`). Since tachi#1602 that pair is
    /// belt-and-braces rather than the only enforcement: the same two
    /// refusals, with byte-identical error text, now live inside the shared
    /// [`db::upsert_within_tx`] body, so every batch path —
    /// [`Self::upsert_batch_with_precommit`] included — refuses them too, and
    /// the pre-walk's remaining value is failing without opening a writer
    /// transaction. All main rows, FTS rows, and vector
    /// projections for the whole batch are then written inside a single
    /// `BEGIN IMMEDIATE` transaction via the same [`db::upsert_within_tx`]
    /// seam `upsert_batch_with_precommit` and (indirectly, through
    /// `db::upsert`) ordinary `upsert` both use — same main-row/FTS/vector
    /// write body, same reserved-metadata merge, same blank-id/`anchor:`/
    /// `wiki-rem:`/wiki-log id guards (enforced inside
    /// `upsert_within_tx`/`upsert_prepared_within_tx`, not this pre-walk);
    /// only the outer entry point differs. Any row or
    /// projection failure rolls back the entire batch. An empty slice is a
    /// successful no-op: no transaction is opened.
    ///
    /// This exists ungated for the same reason
    /// [`Self::upsert_batch_with_precommit`] does not (tachi#1585's class
    /// rule): the raw `&Transaction`/`&Connection` handle is what must stay
    /// admin/test-gated, not batching or atomicity themselves. This method
    /// never hands out that handle, so the portable surface gets atomic
    /// batch writes without the raw-SQL bypass those accessors would open.
    pub fn upsert_batch(&mut self, entries: &[MemoryEntry]) -> Result<(), MemoryError> {
        if entries.is_empty() {
            return Ok(());
        }
        // Refuse a blank id and the reserved `anchor:` namespace before any
        // transaction opens. Since tachi#1602 the shared
        // `upsert_within_tx`/`upsert_prepared_within_tx` seam enforces both
        // with the same error text for every batch path, so this pre-walk is
        // a redundant early exit, not the guard of record.
        for entry in entries {
            if entry.id.trim().is_empty() {
                return Err(MemoryError::InvalidArg(
                    "entry.id must be provided by caller".to_string(),
                ));
            }
            if entry.id.starts_with("anchor:") {
                return Err(MemoryError::InvalidArg(format!(
                    "id '{}' is in the reserved 'anchor:' namespace; use ensure_anchor, not upsert",
                    entry.id
                )));
            }
        }
        self.upsert_batch_in_tx(entries, false, |_tx| Ok(()))
    }

    /// Atomically insert a memory and all of its search projections, without
    /// rewriting an existing id.
    pub fn insert_if_absent(
        &mut self,
        entry: &MemoryEntry,
    ) -> Result<db::InsertMemoryResult, MemoryError> {
        if crate::namespace::is_reserved_wiki_rem_id(&entry.id) {
            return Err(MemoryError::InvalidArg(format!(
                "id '{}' is in the reserved 'wiki-rem:' namespace; use the REM operation transaction",
                entry.id
            )));
        }
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

    #[cfg(unix)]
    #[test]
    fn partial_unix_file_identity_is_not_mutation_authority() {
        assert!(!has_stable_unix_file_identity(7, 0));
        assert!(!has_stable_unix_file_identity(0, 11));
        assert!(!has_stable_unix_file_identity(0, 0));
        assert!(has_stable_unix_file_identity(7, 11));
    }

    #[cfg(unix)]
    #[test]
    fn physical_identity_validation_rejects_path_replacement_during_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let replacement = dir.path().join("replacement.db");
        std::fs::write(&path, b"original inode").unwrap();
        let before = physical_db_identity_at_open(path.to_str().unwrap());
        std::fs::write(&replacement, b"replacement inode").unwrap();
        std::fs::rename(&replacement, &path).unwrap();

        let error = validate_physical_db_identity_across_open(path.to_str().unwrap(), before)
            .expect_err("path replacement must not be recorded as the opened connection");
        assert!(error.to_string().contains("identity changed while opening"));
    }

    #[cfg(unix)]
    #[test]
    fn fresh_reopen_rejects_valid_database_substituted_after_initialization() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        drop(MemoryStore::open(path.to_str().unwrap()).unwrap());
        let initialized_identity = physical_db_identity_at_open(path.to_str().unwrap())
            .expect("initialized DB has physical identity");

        let replacement = dir.path().join("replacement.db");
        drop(MemoryStore::open(replacement.to_str().unwrap()).unwrap());
        std::fs::rename(&replacement, &path).unwrap();

        let error = match MemoryStore::reopen_initialized_file_store(
            path.to_str().unwrap(),
            db::StoreIdentity {
                db_label: "unknown".to_string(),
                profile: db::StoreProfile::TachiFull,
            },
            false,
            initialized_identity,
            None,
        ) {
            Ok(_) => panic!("fresh reopen accepted a valid substituted DB"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("identity changed before identity-bound reopen"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn detached_handle_verification_requires_a_stable_file_identity() {
        assert!(physical_db_identity_is_stable("unix:1:2"));
        assert!(physical_db_identity_is_stable("windows:1:2"));
        assert!(!physical_db_identity_is_stable("path:/tmp/memory.db"));
    }

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
            scored_count: 0,
            last_access: None,
            last_use_at: None,
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
            error.to_string().contains("no such table: hard_state"),
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

    // ── #1599: upsert_batch ─────────────────────────────────────────────────

    #[test]
    fn upsert_batch_empty_slice_is_a_successful_no_op() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        store.upsert_batch(&[]).expect("empty batch is Ok");
        let stats = store.stats(true).expect("stats");
        assert_eq!(stats.total, 0, "empty batch must not write a row");
    }

    #[test]
    fn retired_sticky_write_guard_precedes_cross_project_and_policy_bypasses() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let path = dir.path().join("retired-sticky-guard.db");
        let policy = crate::KernelPolicy {
            path_validation_escape_hatch: true,
            ..Default::default()
        };
        let mut store = MemoryStore::open_with_label(&path.to_string_lossy(), "global")
            .expect("open labelled store")
            .with_kernel_policy(policy);

        let mut rejected = |id: &str, path: &str, category: &str| {
            let mut entry = test_memory_entry(id);
            entry.path = path.to_string();
            entry.category = category.to_string();
            entry.metadata = serde_json::json!({"allow_cross_project": true});
            let error = store
                .upsert(&entry)
                .expect_err("ordinary upsert must reject retired sticky input");
            assert!(
                error.to_string().contains("tachi_a2a"),
                "unexpected error: {error}"
            );
            assert!(store.get(id).expect("get after refusal").is_none());
        };

        rejected("sticky-path-root", "/sticky", "fact");
        rejected("sticky-path-alias", "//STICKY///legacy/", "fact");
        rejected("sticky-category-case", "/notes/ordinary", " Sticky ");

        let mut insert_only = test_memory_entry("sticky-insert-only");
        insert_only.path = "/sticky/legacy".to_string();
        insert_only.metadata = serde_json::json!({"allow_cross_project": true});
        let error = store
            .insert_if_absent(&insert_only)
            .expect_err("insert-only writer must reject retired path");
        assert!(error.to_string().contains("tachi_a2a"), "{error}");

        let mut idless = test_memory_entry("sticky-idless");
        idless.category = "STICKY".to_string();
        let error = store
            .upsert_idless(&idless, "retired-sticky-identity")
            .expect_err("id-less writer must reject retired category");
        assert!(error.to_string().contains("tachi_a2a"), "{error}");

        let mut batch = test_memory_entry("sticky-batch");
        batch.path = "/sticky/batch".to_string();
        let error = store
            .upsert_batch(&[batch])
            .expect_err("batch writer must reject retired path");
        assert!(error.to_string().contains("tachi_a2a"), "{error}");

        let mut validated = test_memory_entry("sticky-validated");
        validated.category = "sticky".to_string();
        let error = store
            .upsert_with_validated_reference_mutations(
                &validated,
                None,
                &serde_json::Map::new(),
                &[],
            )
            .expect_err("validated-reference writer must reject retired category");
        assert!(error.to_string().contains("tachi_a2a"), "{error}");

        let mut ordinary = test_memory_entry("ordinary-near-sticky");
        ordinary.path = "/stickiness/allowed".to_string();
        ordinary.category = "fact".to_string();
        ordinary.metadata = serde_json::json!({"allow_cross_project": true});
        store
            .upsert(&ordinary)
            .expect("near-match path remains allowed");
    }

    #[test]
    fn upsert_batch_atomically_writes_main_rows_fts_and_vector_projections() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        // Each row needs a distinct body. memcore's write path runs
        // write-time near-duplicate consolidation
        // (`memcore::db::memory_crud::merge_into_jaccard_candidate`,
        // crates/memcore/src/db/memory_crud.rs:284; token-Jaccard > 0.9 at
        // :324) on every net-new id within the same transaction. Giving both
        // rows of a batch the shared `test_memory_entry` body ("read-only
        // compatibility fixture", token-Jaccard 1.0) merges the second row
        // into the first at write time and stamps it `superseded_by` before
        // this test's assertions ever run (tachi#1571/#1572's exact
        // pattern) — `store.get` still returns the superseded row, but
        // ordinary `search` correctly excludes it, which is what made
        // `hit_ids.contains("batch-ok-2")` fail. Distinct bodies keep both
        // rows live while the shared prefix keeps them both matching the FTS
        // query below.
        let mut e1 = test_memory_entry("batch-ok-1");
        e1.text = "read-only compatibility fixture batch entry one".to_string();
        e1.vector = Some(vec![0.25_f32; 1024]);
        let mut e2 = test_memory_entry("batch-ok-2");
        e2.text = "read-only compatibility fixture batch entry two".to_string();
        e2.vector = Some(vec![0.75_f32; 1024]);

        store
            .upsert_batch(&[e1, e2])
            .expect("a valid batch must succeed as a single transaction");

        assert!(store.get("batch-ok-1").expect("get").is_some());
        assert!(store.get("batch-ok-2").expect("get").is_some());

        // Both rows must still be live, not silently folded into each other
        // by write-time near-duplicate merge: `MemoryStore::get` returns
        // superseded rows too, so this is the assertion that would actually
        // catch a regression back to a shared body.
        for id in ["batch-ok-1", "batch-ok-2"] {
            let superseded_by: Option<String> = store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap_or_else(|error| panic!("row {id} must exist: {error}"));
            assert!(
                superseded_by.is_none(),
                "row {id} was superseded by {superseded_by:?}; the batch's rows merged into \
                 one another instead of staying independent"
            );
        }

        // FTS projection: the fixture's shared text prefix is queryable, and
        // both distinct-bodied rows are live hits.
        let hits = store
            .search("read-only compatibility fixture", None)
            .expect("fts search");
        let hit_ids: std::collections::BTreeSet<String> =
            hits.into_iter().map(|r| r.entry.id).collect();
        assert!(hit_ids.contains("batch-ok-1"));
        assert!(hit_ids.contains("batch-ok-2"));
        assert_eq!(
            hit_ids.len(),
            2,
            "expected exactly the batch's two rows as FTS hits, got {hit_ids:?}"
        );

        // Vector projection: both rows landed in memories_vec, not just `memories`.
        let vec_ids: std::collections::BTreeSet<String> = store
            .connection()
            .prepare("SELECT id FROM memories_vec ORDER BY id")
            .expect("prepare")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("read memories_vec ids");
        assert!(vec_ids.contains("batch-ok-1"));
        assert!(vec_ids.contains("batch-ok-2"));
        assert_eq!(
            vec_ids.len(),
            2,
            "expected exactly the batch's two rows in memories_vec, got {vec_ids:?}"
        );
    }

    #[test]
    fn upsert_batch_rolls_back_the_whole_batch_when_a_later_entry_fails() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let ok_entry = test_memory_entry("batch-rollback-ok");
        // `wiki-rem:` ids are reserved for the dedicated REM insert-once seam
        // (see `is_reserved_wiki_rem_id`, enforced inside
        // `upsert_prepared_within_tx`). This entry passes
        // `validate_write_path` (its path is ordinary) but fails once its
        // write actually reaches the transaction body, proving the whole
        // `BEGIN IMMEDIATE` transaction — not just pre-validation — rolls
        // back the earlier, otherwise-valid entry too.
        let bad_entry = test_memory_entry("wiki-rem:batch-rollback-bad");

        let error = store
            .upsert_batch(&[ok_entry, bad_entry])
            .expect_err("a batch with a later invalid entry must fail entirely");
        assert!(
            matches!(error, MemoryError::InvalidArg(_)),
            "unexpected error variant: {error:?}"
        );

        assert!(
            store.get("batch-rollback-ok").expect("get").is_none(),
            "the earlier, individually-valid entry must not have been persisted"
        );
        let stats = store.stats(true).expect("stats");
        assert_eq!(
            stats.total, 0,
            "a rolled-back batch must leave zero rows behind"
        );
    }

    #[test]
    fn upsert_batch_refuses_an_anchor_namespace_id_and_persists_nothing() {
        // tachi#1599 checkpoint 4: single-row `upsert` refuses `anchor:`-ids
        // at the top level (memory_crud.rs:2561-2566, reserved for
        // `ensure_anchor`); `upsert_batch` must mirror that guard in its
        // pre-walk instead of silently letting the shared `upsert_within_tx`
        // seam create/overwrite a reserved anchor row.
        let mut reference_store = MemoryStore::open_in_memory().expect("open_in_memory");
        let reference_error = reference_store
            .upsert(&test_memory_entry("anchor:batch-guard-bad"))
            .expect_err("single-row upsert must refuse an anchor: id");

        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let ok_entry = test_memory_entry("batch-anchor-guard-ok");
        let anchor_entry = test_memory_entry("anchor:batch-guard-bad");

        let error = store
            .upsert_batch(&[ok_entry, anchor_entry])
            .expect_err("a batch containing an anchor: id must be refused");
        assert!(
            matches!(error, MemoryError::InvalidArg(_)),
            "unexpected error variant: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            reference_error.to_string(),
            "batch refusal must match the single-row refusal's error shape"
        );

        assert!(
            store.get("batch-anchor-guard-ok").expect("get").is_none(),
            "the earlier, individually-valid entry must not have been persisted"
        );
        let stats = store.stats(true).expect("stats");
        assert_eq!(
            stats.total, 0,
            "a batch refused for an anchor: id must leave zero rows behind, and no \
             transaction should even have opened"
        );
    }

    #[test]
    fn upsert_batch_refuses_a_blank_id_and_persists_nothing() {
        // tachi#1599 checkpoint 4: single-row `upsert` refuses a blank/empty
        // id at the top level (memory_crud.rs:2550-2554); `upsert_batch`
        // must mirror that guard in its pre-walk for the same reason as the
        // `anchor:` guard above.
        let mut reference_store = MemoryStore::open_in_memory().expect("open_in_memory");
        let reference_error = reference_store
            .upsert(&test_memory_entry("   "))
            .expect_err("single-row upsert must refuse a blank id");

        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let ok_entry = test_memory_entry("batch-blank-guard-ok");
        let blank_entry = test_memory_entry("   ");

        let error = store
            .upsert_batch(&[ok_entry, blank_entry])
            .expect_err("a batch containing a blank id must be refused");
        assert!(
            matches!(error, MemoryError::InvalidArg(_)),
            "unexpected error variant: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            reference_error.to_string(),
            "batch refusal must match the single-row refusal's error shape"
        );

        assert!(
            store.get("batch-blank-guard-ok").expect("get").is_none(),
            "the earlier, individually-valid entry must not have been persisted"
        );
        let stats = store.stats(true).expect("stats");
        assert_eq!(
            stats.total, 0,
            "a batch refused for a blank id must leave zero rows behind, and no \
             transaction should even have opened"
        );
    }

    // ── #1602: the refusals live at the shared transactional seam ───────────
    //
    // `upsert_batch_with_precommit` has no pre-walk of its own — it goes
    // straight to `upsert_batch_in_tx`. So every assertion below is a direct
    // test of the guard inside
    // `db::upsert_within_tx`/`memory_crud::upsert_prepared_within_tx`: if the
    // refusal were only at `upsert_with_idless_identity` (single-row) or in
    // `upsert_batch`'s pre-walk, these batches would commit.

    #[test]
    fn upsert_batch_with_precommit_refuses_an_anchor_namespace_id_at_the_seam() {
        let mut reference_store = MemoryStore::open_in_memory().expect("open_in_memory");
        let reference_error = reference_store
            .upsert(&test_memory_entry("anchor:precommit-guard-bad"))
            .expect_err("single-row upsert must refuse an anchor: id");

        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let ok_entry = test_memory_entry("precommit-anchor-guard-ok");
        let anchor_entry = test_memory_entry("anchor:precommit-guard-bad");

        let precommit_ran = std::cell::Cell::new(false);
        let error = store
            .upsert_batch_with_precommit(&[ok_entry, anchor_entry], |_tx| {
                precommit_ran.set(true);
                Ok(())
            })
            .expect_err("a precommit batch containing an anchor: id must be refused");
        assert!(
            matches!(error, MemoryError::InvalidArg(_)),
            "unexpected error variant: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            reference_error.to_string(),
            "the seam's refusal must be byte-identical to the single-row refusal"
        );
        assert!(
            !precommit_ran.get(),
            "the row loop must fail before the pre-commit closure runs"
        );

        assert!(
            store
                .get("precommit-anchor-guard-ok")
                .expect("get")
                .is_none(),
            "the earlier, individually-valid entry must roll back with the batch"
        );
        let stats = store.stats(true).expect("stats");
        assert_eq!(
            stats.total, 0,
            "a batch refused mid-loop must leave zero rows behind"
        );
    }

    #[test]
    fn upsert_batch_with_precommit_refuses_a_blank_id_at_the_seam() {
        let mut reference_store = MemoryStore::open_in_memory().expect("open_in_memory");
        let reference_error = reference_store
            .upsert(&test_memory_entry("   "))
            .expect_err("single-row upsert must refuse a blank id");

        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let ok_entry = test_memory_entry("precommit-blank-guard-ok");
        let blank_entry = test_memory_entry("   ");

        let error = store
            .upsert_batch_with_precommit(&[ok_entry, blank_entry], |_tx| Ok(()))
            .expect_err("a precommit batch containing a blank id must be refused");
        assert_eq!(
            error.to_string(),
            reference_error.to_string(),
            "the seam's refusal must be byte-identical to the single-row refusal"
        );

        assert!(
            store
                .get("precommit-blank-guard-ok")
                .expect("get")
                .is_none(),
            "the earlier, individually-valid entry must roll back with the batch"
        );
        let stats = store.stats(true).expect("stats");
        assert_eq!(
            stats.total, 0,
            "a batch refused mid-loop must leave zero rows behind"
        );
    }

    #[test]
    fn preserving_anchor_rows_variant_carries_an_existing_anchor_row_verbatim() {
        // The tidy-migration exemption: a whole-store copy must be able to
        // carry an `anchor:` row that `ensure_anchor` legitimately created in
        // the source database, while the ordinary batch entry point still
        // refuses the very same entry.
        let source = MemoryStore::open_in_memory().expect("open_in_memory");
        let anchor_id = source
            .ensure_anchor(crate::db::AnchorKind::Issue, "kckylechen1/tachi:1602")
            .expect("ensure_anchor creates the source anchor row");
        let anchor_entry = source
            .get(&anchor_id)
            .expect("get")
            .expect("the source anchor row must be readable");

        let mut target = MemoryStore::open_in_memory().expect("open_in_memory");
        let refusal = target
            .upsert_batch_with_precommit(std::slice::from_ref(&anchor_entry), |_tx| Ok(()))
            .expect_err("the ordinary precommit entry point must still refuse anchor: ids");
        assert!(
            matches!(refusal, MemoryError::InvalidArg(_)),
            "unexpected error variant: {refusal:?}"
        );

        let rows_after: usize = target
            .upsert_batch_with_precommit_preserving_anchor_rows(
                &[
                    test_memory_entry("migration-carried-ordinary"),
                    anchor_entry,
                ],
                |tx| {
                    tx.query_row("SELECT COUNT(*) FROM memories", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map(|count| count as usize)
                    .map_err(MemoryError::from)
                },
            )
            .expect("the trusted whole-store-copy variant must carry the anchor row");
        assert_eq!(rows_after, 2, "both copied rows must be visible in-tx");

        assert!(
            target.get(&anchor_id).expect("get").is_some(),
            "the carried anchor row must be durable in the target"
        );
        // The carried row is still a valid anchor: `ensure_anchor`'s
        // read-verify guard (a) accepts it as the existing row for the same
        // (kind, key) instead of failing closed on a mismatch.
        assert_eq!(
            target
                .ensure_anchor(crate::db::AnchorKind::Issue, "kckylechen1/tachi:1602")
                .expect("ensure_anchor must accept the carried row"),
            anchor_id
        );
    }

    #[test]
    fn preserving_anchor_rows_variant_still_refuses_other_reserved_namespaces() {
        // The exemption is anchor-only: `wiki-rem:` and the blank id stay
        // refused even for the trusted whole-store-copy writer.
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let wiki_rem_error = store
            .upsert_batch_with_precommit_preserving_anchor_rows(
                &[test_memory_entry("wiki-rem:1602-narrowness")],
                |_tx| Ok(()),
            )
            .expect_err("wiki-rem: ids must stay refused");
        assert!(
            wiki_rem_error
                .to_string()
                .contains("reserved 'wiki-rem:' namespace"),
            "unexpected error: {wiki_rem_error}"
        );

        let blank_error = store
            .upsert_batch_with_precommit_preserving_anchor_rows(
                &[test_memory_entry("   ")],
                |_tx| Ok(()),
            )
            .expect_err("a blank id must stay refused");
        assert!(
            blank_error
                .to_string()
                .contains("entry.id must be provided by caller"),
            "unexpected error: {blank_error}"
        );

        let stats = store.stats(true).expect("stats");
        assert_eq!(stats.total, 0, "no refused batch may leave a row behind");
    }
}

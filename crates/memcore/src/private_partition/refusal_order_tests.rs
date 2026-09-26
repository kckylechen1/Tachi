//! tachi#1990: a generic open that ends in [`MemoryError::PrivatePartitionRefused`]
//! must be an admission decision with no side effects.
//!
//! Before the fix (main at fbab02c7d) the private-partition check
//! (`refuse_stamped_private_store`) ran in `store/open.rs` only after
//! `db::init_store_schema_with_label_mut` had returned, i.e. after the
//! migration backup, the committed identity stamps and the `.migration-marker`
//! write. It is now part of schema init's admission: evaluated in the
//! read-only preflight (before backup, PRAGMAs, DDL, stamp or marker) and again
//! inside `BEGIN IMMEDIATE`.
//!
//! The rows below cover every generic door (`open_with_label_inner`,
//! `open_read_only_inner`, `open_existing_read_write`) against the #1990 input
//! set, plus the post-commit physical-identity bracket that stays after init.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{
    snapshot_sqlite_image, stamp_private_identity, AdmittedPartition, CapabilityReceipt,
    PartitionCapability, PrivatePartition, PrivatePartitionOpenContext, StaticKeyProvider,
    SubjectId, TrustDomainId, STORE_PRIVATE_PARTITION_KEY,
};
use crate::db::store_profile::{STORE_IDENTITY_NAMESPACE, STORE_PROFILE_KEY, STORE_ROLE_KEY};
use crate::db::{self, DbOpenContext, StoreProfile};
use crate::error::MemoryError;
use crate::path_router::UNKNOWN_DB_LABEL;
use crate::MemoryStore;

/// Everything a refused open could leave behind, sampled without opening the
/// observed file: SQL facts are read from a byte copy in a separate directory,
/// so the sampler itself cannot create `-wal`/`-shm` siblings or checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoreArtifacts {
    pub db_sha256: String,
    /// SQLite header bytes 18/19 (write/read format: 1 = rollback journal,
    /// 2 = WAL), so a journal-mode flip is visible separately from row writes.
    pub header_format_versions: (u8, u8),
    /// Every file in the store directory other than the database itself.
    pub siblings: Vec<String>,
    pub marker: Option<String>,
    pub backups: Vec<String>,
    pub user_version: i64,
    /// `PRAGMA schema_version`: SQLite bumps it on every DDL statement.
    pub schema_cookie: i64,
    pub role: Option<String>,
    pub profile: Option<String>,
    pub private_partition_stamped: bool,
    /// `(key, value_json, version)` of every `store_identity` row, verbatim.
    pub identity_rows: Vec<(String, String, i64)>,
}

pub(super) fn sha256_hex(path: &Path) -> String {
    let bytes = fs::read(path).expect("read file for sha256");
    let digest = Sha256::digest(&bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn marker_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push(".migration-marker");
    PathBuf::from(name)
}

pub(super) fn backup_names(db_path: &Path) -> Vec<String> {
    let prefix = format!(
        "{}.migration-bak.",
        db_path.file_name().unwrap().to_string_lossy()
    );
    let mut names: Vec<String> = fs::read_dir(db_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&prefix))
        .collect();
    names.sort();
    names
}

/// Remove the migration marker and every migration backup, producing the
/// issue's "no backup marker" precondition.
pub(super) fn strip_migration_artifacts(db_path: &Path) {
    let _ = fs::remove_file(marker_path(db_path));
    for name in backup_names(db_path) {
        fs::remove_file(db_path.parent().unwrap().join(name)).unwrap();
    }
}

pub(super) fn observe(db_path: &Path) -> StoreArtifacts {
    let dir = db_path.parent().unwrap();
    let db_name = db_path.file_name().unwrap().to_string_lossy().into_owned();
    let db_sha256 = sha256_hex(db_path);
    let header = fs::read(db_path).expect("read database header");
    let header_format_versions = (header[18], header[19]);
    let mut siblings: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| *name != db_name)
        .collect();
    siblings.sort();
    let marker = fs::read_to_string(marker_path(db_path)).ok();
    let backups = backup_names(db_path);

    let copy_dir = tempfile::tempdir().unwrap();
    let copy = copy_dir.path().join("observed.db");
    fs::copy(db_path, &copy).unwrap();
    let wal = dir.join(format!("{db_name}-wal"));
    if wal.exists() {
        fs::copy(&wal, copy_dir.path().join("observed.db-wal")).unwrap();
    }
    let conn = rusqlite::Connection::open(&copy).unwrap();
    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let schema_cookie: i64 = conn
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap();
    let has_hard_state: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='hard_state')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let identity_rows: Vec<(String, String, i64)> = if has_hard_state {
        let mut statement = conn
            .prepare(
                "SELECT key, value_json, version FROM hard_state \
                 WHERE namespace = ?1 ORDER BY key",
            )
            .unwrap();
        let rows = statement
            .query_map([STORE_IDENTITY_NAMESPACE], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    } else {
        Vec::new()
    };
    let stamp = |key: &str| {
        identity_rows
            .iter()
            .find(|(row_key, _, _)| row_key == key)
            .map(|(_, value_json, _)| {
                let parsed: serde_json::Value = serde_json::from_str(value_json).unwrap();
                parsed["value"]
                    .as_str()
                    .unwrap_or("<non-string>")
                    .to_string()
            })
    };
    StoreArtifacts {
        db_sha256,
        header_format_versions,
        siblings,
        marker,
        backups,
        user_version,
        schema_cookie,
        role: stamp(STORE_ROLE_KEY),
        profile: stamp(STORE_PROFILE_KEY),
        private_partition_stamped: stamp(STORE_PRIVATE_PARTITION_KEY).is_some(),
        identity_rows,
    }
}

/// Every field that differs between two samples, rendered for a failure
/// message.
pub(super) fn artifact_diff(before: &StoreArtifacts, after: &StoreArtifacts) -> Vec<String> {
    let mut diffs = Vec::new();
    macro_rules! field {
        ($name:ident) => {
            if before.$name != after.$name {
                diffs.push(format!(
                    "{}: before={:?} after={:?}",
                    stringify!($name),
                    before.$name,
                    after.$name
                ));
            }
        };
    }
    field!(db_sha256);
    field!(header_format_versions);
    field!(siblings);
    field!(marker);
    field!(backups);
    field!(user_version);
    field!(schema_cookie);
    field!(role);
    field!(profile);
    field!(private_partition_stamped);
    field!(identity_rows);
    diffs
}

/// The #1990 fixture, built through the real open and stamp APIs: a valid,
/// current-schema working image with profile `PortableKernel`, no role stamp,
/// the write-once private-partition stamp, and no migration marker or backup.
pub(super) fn portable_unroled_private_stamped_store(dir: &Path) -> PathBuf {
    let path = portable_unroled_private_stamped_store_keeping_marker(dir);
    strip_migration_artifacts(&path);
    path
}

/// [`portable_unroled_private_stamped_store`] with the creating open's
/// `.migration-marker` left in place. Stamping writes a `hard_state` row, not
/// DDL, so the marker still matches the file's fingerprint and the backup
/// heuristic alone would not back up.
fn portable_unroled_private_stamped_store_keeping_marker(dir: &Path) -> PathBuf {
    let path = portable_unroled_store(dir);
    let store = MemoryStore::open_with_context(&path.to_string_lossy(), &portable_deny())
        .expect("reopen portable fixture store");
    {
        let _authorization =
            db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("enter typed identity-write scope");
        stamp_private_identity(store.connection(), &fixture_partition())
            .expect("stamp private-partition identity");
    }
    drop(store);
    path
}

fn fixture_partition() -> AdmittedPartition {
    AdmittedPartition {
        partition_id: "tachi-1990-fixture-partition".to_string(),
    }
}

/// A current-schema `PortableKernel` store with no role and no private stamp:
/// the non-private twin of the #1990 fixture.
fn portable_unroled_store(dir: &Path) -> PathBuf {
    let path = dir.join("store.db");
    let store = MemoryStore::open_with_context(
        &path.to_string_lossy(),
        &DbOpenContext::create_fresh().with_profile(StoreProfile::PortableKernel),
    )
    .expect("create portable fixture store");
    assert_eq!(store.db_label(), UNKNOWN_DB_LABEL);
    assert_eq!(store.store_profile(), StoreProfile::PortableKernel);
    drop(store);
    path
}

fn private_door_context() -> (PrivatePartitionOpenContext, StaticKeyProvider) {
    let ctx = PrivatePartitionOpenContext {
        trust_domain_id: TrustDomainId::new("td-1990").unwrap(),
        subject_id: SubjectId::new("subject-1990").unwrap(),
        receipt: CapabilityReceipt::new("rcpt-1990").unwrap(),
        capabilities: [PartitionCapability::Read, PartitionCapability::Write]
            .into_iter()
            .collect(),
        key_version: "kv1990".to_string(),
        revoked: false,
    };
    let keys = StaticKeyProvider::new([19u8; 32])
        .with_admission(
            "td-1990",
            "subject-1990",
            "rcpt-1990",
            [PartitionCapability::Read, PartitionCapability::Write],
            "kv1990",
        )
        .unwrap();
    (ctx, keys)
}

/// The working SQLite image of a real private partition, exactly as the
/// private door builds and seals it (`open_private_image` +
/// `snapshot_sqlite_image`), written to `dir/store.db`: role
/// `private_partition`, profile `portable_kernel`, the private stamp, and a
/// rollback-journal header (it is serialized from an in-memory database).
fn real_private_image_file(dir: &Path) -> PathBuf {
    let (ctx, keys) = private_door_context();
    let part = PrivatePartition::open_in_memory(&ctx, &keys).expect("open private partition");
    let image = snapshot_sqlite_image(&part.store.conn, &part.identity.partition_id)
        .expect("serialize the private working image");
    drop(part);
    let path = dir.join("store.db");
    fs::write(&path, image).unwrap();
    path
}

fn expect_refused(result: Result<MemoryStore, MemoryError>, want: fn(&MemoryError) -> bool) {
    match result {
        Err(error) if want(&error) => {}
        Err(other) => panic!("unexpected refusal: {other:?}"),
        Ok(_) => panic!("expected a refusal, the open succeeded"),
    }
}

fn is_private_refusal(error: &MemoryError) -> bool {
    matches!(error, MemoryError::PrivatePartitionRefused)
}

/// `resolve_generic_open_path`'s content-free refusal of a non-SQLite image.
fn is_generic_not_a_database(error: &MemoryError) -> bool {
    matches!(error, MemoryError::InvalidArg(message) if message == "database path is not a database")
}

fn is_path_identity_changed(error: &MemoryError) -> bool {
    matches!(error, MemoryError::InvalidArg(message) if message.contains("identity changed while opening"))
}

/// Open `path` through `open`, require the refusal `want`, and require that
/// nothing the refused open could leave behind changed.
fn assert_refused_without_side_effects(
    path: &Path,
    open: impl FnOnce(&str) -> Result<MemoryStore, MemoryError>,
    want: fn(&MemoryError) -> bool,
) -> StoreArtifacts {
    let before = observe(path);
    expect_refused(open(&path.to_string_lossy()), want);
    let after = observe(path);
    let diffs = artifact_diff(&before, &after);
    assert!(
        diffs.is_empty(),
        "refused open left side effects behind:\n  {}",
        diffs.join("\n  ")
    );
    after
}

/// A generic `MemoryStore` door taking only a path.
type GenericDoor = fn(&str) -> Result<MemoryStore, MemoryError>;

fn portable_deny() -> DbOpenContext {
    DbOpenContext::open_existing_deny().with_profile(StoreProfile::PortableKernel)
}

#[test]
fn private_partition_refusal_leaves_no_role_marker_backup_or_byte_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = portable_unroled_private_stamped_store(dir.path());

    let before = observe(&path);
    assert_eq!(before.role, None, "fixture must carry no role stamp");
    assert_eq!(before.profile.as_deref(), Some("portable_kernel"));
    assert!(
        before.private_partition_stamped,
        "fixture must be private-stamped"
    );
    assert_eq!(
        before.marker, None,
        "fixture must carry no migration marker"
    );
    assert!(before.backups.is_empty(), "fixture must carry no backup");

    let result = MemoryStore::open_with_label_and_context(
        &path.to_string_lossy(),
        "global",
        &DbOpenContext::open_existing_deny().with_profile(StoreProfile::PortableKernel),
    );
    match result {
        Err(MemoryError::PrivatePartitionRefused) => {}
        Err(other) => panic!("expected PrivatePartitionRefused, got {other:?}"),
        Ok(_) => panic!("expected PrivatePartitionRefused, open succeeded"),
    }

    let after = observe(&path);
    let diffs = artifact_diff(&before, &after);
    assert!(
        diffs.is_empty(),
        "refused open left side effects behind:\n  {}",
        diffs.join("\n  ")
    );
}

/// #1990 row: the fixture with its creating open's marker still matching, so
/// the backup heuristic alone would not back up. Before the fix the refusal
/// still followed a committed `global` role stamp and a changed file.
#[test]
fn private_refusal_with_a_matching_marker_commits_no_role() {
    let dir = tempfile::tempdir().unwrap();
    let path = portable_unroled_private_stamped_store_keeping_marker(dir.path());
    let before = observe(&path);
    assert_eq!(
        before.marker,
        Some(format!(
            "schema:{}:{}",
            before.user_version, before.schema_cookie
        )),
        "precondition: the marker matches the file's backup fingerprint"
    );
    assert_refused_without_side_effects(
        &path,
        |path| MemoryStore::open_with_label_and_context(path, "global", &portable_deny()),
        is_private_refusal,
    );
}

/// #1990 row: no label (`open_with_context`, the path-validation-off door).
/// No role is claimed, so before the fix only the backup, the marker and the
/// file change remained.
#[test]
fn private_refusal_without_a_label_leaves_no_backup_or_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = portable_unroled_private_stamped_store(dir.path());
    assert_refused_without_side_effects(
        &path,
        |path| MemoryStore::open_with_context(path, &portable_deny()),
        is_private_refusal,
    );
}

/// #1990 row: a private-stamped store with a pending migration, opened with
/// migration authority. Before the fix the generic door backed it up, ran the
/// migration, stamped `user_version` and then refused.
#[test]
fn private_refusal_precedes_an_authorized_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = portable_unroled_private_stamped_store(dir.path());
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .unwrap();
    }
    let after = assert_refused_without_side_effects(
        &path,
        |path| {
            MemoryStore::open_with_label_and_context(
                path,
                "global",
                &DbOpenContext::open_existing_allow("test:tachi-1990")
                    .with_profile(StoreProfile::PortableKernel),
            )
        },
        is_private_refusal,
    );
    assert_eq!(
        after.user_version,
        i64::from(db::migrations::EXPECTED_SCHEMA_VERSION - 1),
        "the pending migration must not run"
    );
}

/// #1990 row: the working image of a real private partition (role
/// `private_partition`, rollback-journal header) under an unlabelled generic
/// open. Before the fix: backup, marker, and the header flipped to WAL.
#[test]
fn real_private_image_without_a_label_is_refused_before_any_side_effect() {
    let dir = tempfile::tempdir().unwrap();
    let path = real_private_image_file(dir.path());
    let before = observe(&path);
    assert_eq!(before.role.as_deref(), Some("private_partition"));
    assert_eq!(before.profile.as_deref(), Some("portable_kernel"));
    assert!(before.private_partition_stamped);
    assert_eq!(
        before.header_format_versions,
        (1, 1),
        "the serialized private image is a rollback-journal file"
    );
    let after = assert_refused_without_side_effects(
        &path,
        |path| MemoryStore::open_with_context(path, &portable_deny()),
        is_private_refusal,
    );
    assert_eq!(after.header_format_versions, (1, 1), "no WAL flip");
}

/// The same real private image under a claimed role and under the default
/// full-Tachi open. The identity refusal wins (it is evaluated first, as it was
/// before #1990) and, since #1983, is already a preflight refusal: pinned here
/// so the private-partition move cannot reintroduce a side effect.
#[test]
fn real_private_image_identity_refusals_leave_no_side_effect() {
    let dir = tempfile::tempdir().unwrap();
    let path = real_private_image_file(dir.path());
    assert_refused_without_side_effects(
        &path,
        |path| MemoryStore::open_with_label_and_context(path, "global", &portable_deny()),
        |error| matches!(error, MemoryError::StoreRoleConflict { .. }),
    );
    assert_refused_without_side_effects(&path, MemoryStore::open, |error| {
        matches!(error, MemoryError::StoreProfileMismatch { .. })
    });
}

/// The sealed envelope itself under a generic open: refused as "not a
/// database" by `resolve_generic_open_path` before SQLite opens the path.
#[test]
fn sealed_private_envelope_under_a_generic_open_leaves_no_side_effect() {
    let root = tempfile::tempdir().unwrap();
    let (ctx, keys) = private_door_context();
    let sealed = {
        let mut part = PrivatePartition::open(root.path(), &ctx, &keys).expect("open partition");
        part.persist().expect("seal partition");
        part.sealed_path.clone().expect("file-backed partition")
    };
    let dir = sealed.parent().unwrap();
    let siblings = |dir: &Path| {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let sha_before = sha256_hex(&sealed);
    let siblings_before = siblings(dir);
    let generic_doors: [GenericDoor; 3] = [
        MemoryStore::open,
        MemoryStore::open_read_only,
        MemoryStore::open_existing_read_write,
    ];
    for open in generic_doors {
        expect_refused(open(&sealed.to_string_lossy()), is_generic_not_a_database);
    }
    assert_eq!(sha256_hex(&sealed), sha_before);
    assert_eq!(siblings(dir), siblings_before);
}

/// Authoritative half: the store is not private-stamped at the preflight, and
/// another process stamps it in the window before `BEGIN IMMEDIATE`. The
/// in-transaction evaluation must refuse before any DDL or identity write, so
/// the role stays unstamped. (A raced refusal may keep the backup and the WAL
/// switch, #1983's documented raced-refusal residue.)
#[test]
fn private_stamp_committed_in_the_window_is_refused_in_the_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = portable_unroled_store(dir.path());
    strip_migration_artifacts(&path);
    let before = observe(&path);
    assert!(!before.private_partition_stamped);
    assert_eq!(before.role, None);

    db::schema_test_hooks::arm_before_schema_transaction(|path| {
        let other = rusqlite::Connection::open(path).expect("other process connection");
        stamp_private_identity(&other, &fixture_partition()).expect("stamp in the window");
    });
    let result = MemoryStore::open_with_label_and_context(
        &path.to_string_lossy(),
        "global",
        &portable_deny(),
    );
    db::schema_test_hooks::disarm_window_hooks();
    expect_refused(result, is_private_refusal);

    let after = observe(&path);
    assert!(after.private_partition_stamped, "the window's stamp stays");
    assert_eq!(after.role, None, "no role may be committed");
    assert_eq!(after.user_version, before.user_version);
    assert_eq!(
        after.identity_rows.len(),
        before.identity_rows.len() + 1,
        "only the window's private stamp may be added: {:?}",
        after.identity_rows
    );
    assert_eq!(after.marker, None, "no marker after a refusal");
}

/// Read-only door. It never initializes schema, and reading the stamp needs an
/// open connection, so the only file effect a refusal can have is SQLite's
/// read-only WAL sidecars. Pin that the refusal leaves nothing a successful
/// read-only open of the non-private twin does not also leave.
#[test]
fn read_only_private_refusal_leaves_only_what_a_successful_read_only_open_leaves() {
    let refused_dir = tempfile::tempdir().unwrap();
    let refused = portable_unroled_private_stamped_store(refused_dir.path());
    let control_dir = tempfile::tempdir().unwrap();
    let control = portable_unroled_store(control_dir.path());
    strip_migration_artifacts(&control);

    let refused_before = observe(&refused);
    expect_refused(
        MemoryStore::open_read_only_with_label(&refused.to_string_lossy(), "global"),
        is_private_refusal,
    );
    let refused_after = observe(&refused);

    let control_before = observe(&control);
    drop(
        MemoryStore::open_read_only_with_label(&control.to_string_lossy(), "global")
            .expect("the non-private twin opens read-only"),
    );
    let control_after = observe(&control);

    assert_eq!(refused_after.db_sha256, refused_before.db_sha256);
    assert_eq!(refused_after.identity_rows, refused_before.identity_rows);
    assert_eq!(refused_after.marker, None);
    assert!(refused_after.backups.is_empty());
    assert_eq!(control_after.db_sha256, control_before.db_sha256);
    assert_eq!(
        (&refused_before.siblings, &refused_after.siblings),
        (&control_before.siblings, &control_after.siblings),
        "a refused read-only open may leave only the sidecars a successful one leaves"
    );
}

/// Maintenance door (`open_existing_read_write`): no init, no PRAGMA write; the
/// refusal leaves nothing on the #1990 fixture or on a real private image
/// (which also keeps its rollback-journal header).
#[test]
fn maintenance_private_refusal_leaves_no_side_effect() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = portable_unroled_private_stamped_store(dir.path());
    assert_refused_without_side_effects(
        &fixture,
        MemoryStore::open_existing_read_write,
        is_private_refusal,
    );
    let image_dir = tempfile::tempdir().unwrap();
    let image = real_private_image_file(image_dir.path());
    let after = assert_refused_without_side_effects(
        &image,
        MemoryStore::open_existing_read_write,
        is_private_refusal,
    );
    assert_eq!(after.header_format_versions, (1, 1));
}

/// The path→handle binding. The path is replaced by another store in the
/// window before `BEGIN IMMEDIATE`; the open must refuse before `COMMIT`.
/// SQLite names the `-wal` after the path, so a transaction committed after
/// the swap sits in `store.db-wal` next to the substitute, and the next opener
/// of the path reads it as part of the substitute (measured before the fix:
/// the substitute came back with role `global`). What the next opener of the
/// path sees must be exactly the substitute's own state, and no marker is
/// written. (The backup and the WAL switch happen before the window: #1983's
/// raced-refusal residue.)
#[cfg(unix)]
#[test]
fn path_replaced_during_open_is_refused_before_commit_and_the_substitute_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = portable_unroled_store(dir.path());
    strip_migration_artifacts(&path);
    let substitute_dir = tempfile::tempdir().unwrap();
    let substitute_src = portable_unroled_store(substitute_dir.path());
    strip_migration_artifacts(&substitute_src);
    let substitute = observe(&substitute_src);
    assert_eq!(substitute.role, None);
    let moved = dir.path().join("moved-aside.db");

    {
        let moved = moved.clone();
        let substitute_src = substitute_src.clone();
        db::schema_test_hooks::arm_before_schema_transaction(move |path| {
            fs::rename(path, &moved).expect("move the opened store aside");
            fs::copy(&substitute_src, path).expect("substitute another store");
        });
    }
    let result = MemoryStore::open_with_label_and_context(
        &path.to_string_lossy(),
        "global",
        &portable_deny(),
    );
    db::schema_test_hooks::disarm_window_hooks();
    expect_refused(result, is_path_identity_changed);

    assert_eq!(
        sha256_hex(&path),
        substitute.db_sha256,
        "the substituted file must be untouched"
    );
    let seen = observe(&path);
    assert_eq!(
        (&seen.role, &seen.identity_rows, seen.user_version),
        (
            &substitute.role,
            &substitute.identity_rows,
            substitute.user_version
        ),
        "the next opener of the path must see the substitute's own state, not this open's writes"
    );
    assert_eq!(seen.marker, None, "no marker after a refusal");
    assert_eq!(
        observe(&moved).role,
        None,
        "the refused transaction rolled back on the opened file too"
    );
}

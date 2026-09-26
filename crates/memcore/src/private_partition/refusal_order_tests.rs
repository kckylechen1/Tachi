//! tachi#1990: a generic open that ends in [`MemoryError::PrivatePartitionRefused`]
//! must be an admission decision with no side effects.
//!
//! On main at 2898c6512 the private-partition check
//! (`refuse_stamped_private_store`) runs in `store/open.rs` only after
//! `db::init_schema_with_label_mut` has returned, i.e. after the migration
//! backup, the committed identity stamps and the `.migration-marker` write.
//! The test below states the correct behaviour (the refused file and its
//! siblings are exactly as they were before the open), so it is red until the
//! refusal moves into the open preflight.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{stamp_private_identity, AdmittedPartition, STORE_PRIVATE_PARTITION_KEY};
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
    let path = dir.join("store.db");
    let path_str = path.to_string_lossy().into_owned();
    let store = MemoryStore::open_with_context(
        &path_str,
        &DbOpenContext::create_fresh().with_profile(StoreProfile::PortableKernel),
    )
    .expect("create portable fixture store");
    assert_eq!(store.db_label(), UNKNOWN_DB_LABEL);
    assert_eq!(store.store_profile(), StoreProfile::PortableKernel);
    {
        let _authorization =
            db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("enter typed identity-write scope");
        stamp_private_identity(
            store.connection(),
            &AdmittedPartition {
                partition_id: "tachi-1990-fixture-partition".to_string(),
            },
        )
        .expect("stamp private-partition identity");
    }
    drop(store);
    strip_migration_artifacts(&path);
    path
}

#[test]
#[ignore = "tachi#1990: red until the fix lands after #1983"]
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

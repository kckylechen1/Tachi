//! Read-only inventory identity for discovered SQLite paths.
//!
//! A discovered path is evidence, not a database identity and never an
//! authority grant. Unix inventories bind existing files by `(dev, ino)`;
//! other platforms (and Unix metadata failures) use a canonical path only
//! when one can be resolved without mutating the filesystem.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryFailureKind {
    BrokenSymlink,
    NotFound,
    PermissionDenied,
    Locked,
    NotDatabase,
    SchemaMismatch,
    ExtensionUnavailable,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenPathBasis {
    CanonicalFallback,
    ShmVisible,
    WalVisible,
    WalAndShmVisible,
}

/// Whether a physical store has a deterministic mutation owner.
///
/// Inventory stays read-only for every store. Mutation is intentionally more
/// restrictive: two independently live sidecar locations for one main inode
/// could represent divergent WAL histories, so neither lexical alias may be
/// selected until a human adjudicates the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicalStoreMutationState {
    Authorized,
    AmbiguousPhysicalStore,
}

impl PhysicalStoreMutationState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Authorized => "authorized",
            Self::AmbiguousPhysicalStore => "ambiguous_physical_store",
        }
    }
}

impl OpenPathBasis {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalFallback => "canonical_fallback",
            Self::ShmVisible => "shm_visible",
            Self::WalVisible => "wal_visible",
            Self::WalAndShmVisible => "wal_and_shm_visible",
        }
    }
}

impl InventoryFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::BrokenSymlink => "broken_symlink",
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::Locked => "locked",
            Self::NotDatabase => "not_database",
            Self::SchemaMismatch => "schema_mismatch",
            Self::ExtensionUnavailable => "extension_unavailable",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PhysicalDbStore {
    pub physical_id: String,
    pub canonical_path: String,
    /// Path used only for read-only SQLite probes. This may differ from the
    /// deterministic display path when a symlink target or hardlink alias
    /// owns live WAL/SHM. It never grants rename/remove/checkpoint authority.
    pub open_path: String,
    pub open_path_basis: OpenPathBasis,
    /// Candidate paths with a visible non-empty WAL and/or SHM sidecar.
    pub sidecar_paths: Vec<String>,
    pub primary_path: String,
    pub aliases: Vec<String>,
    pub open_failure_kind: Option<InventoryFailureKind>,
    /// Read-only inventory can represent an ambiguous store, but no mutation
    /// sink may select one of its aliases by lexical order.
    pub mutation_state: PhysicalStoreMutationState,
    /// Scan-time object binding. This is intentionally not rendered: reports
    /// expose the immutable identity evidence above, while mutation callers
    /// must hold this typed capability and revalidate it immediately before
    /// acting.
    #[serde(skip_serializing)]
    pub(crate) mutation_authority: Option<PhysicalMutationAuthority>,
}

#[derive(Debug, Clone)]
pub(crate) struct UnresolvedDbPath {
    pub path: PathBuf,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
    pub target_exists: Option<bool>,
    pub failure_kind: InventoryFailureKind,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PhysicalDbInventory {
    pub stores: Vec<PhysicalDbStore>,
    pub unresolved_paths: Vec<UnresolvedDbPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum IdentityKey {
    #[cfg(unix)]
    Unix {
        device: u64,
        inode: u64,
    },
    Canonical(String),
}

/// A capability for mutating one exact directory entry that was observed
/// during inventory. A path spelling alone is not authority: revalidation
/// binds the original spelling, resolved canonical identity, Unix physical
/// identity when available, and the entry's file-type/symlink state.
#[derive(Debug, Clone)]
pub(crate) struct PhysicalMutationAuthority {
    discovered_path: PathBuf,
    canonical_path: PathBuf,
    physical_identity: IdentityKey,
    path_state: MutationPathState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MutationPathState {
    is_symlink: bool,
    is_regular_file: bool,
    /// `metadata()` follows a symlink and binds the SQLite object. On Unix we
    /// also pin the directory entry itself, so replacing a symlink with a new
    /// symlink to the same target is still a stale authority.
    #[cfg(unix)]
    directory_entry_identity: Option<UnixFileIdentity>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnixFileIdentity {
    device: u64,
    inode: u64,
}

impl PhysicalMutationAuthority {
    pub(crate) fn capture(path: &Path) -> Result<Self, String> {
        Self::capture_inner(path, false)
    }

    #[cfg(test)]
    fn capture_with_canonical_fallback(path: &Path) -> Result<Self, String> {
        Self::capture_inner(path, true)
    }

    fn capture_inner(path: &Path, force_canonical_identity: bool) -> Result<Self, String> {
        let link_metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("symlink_metadata({}): {error}", path.display()))?;
        let resolved_metadata = std::fs::metadata(path)
            .map_err(|error| format!("metadata({}): {error}", path.display()))?;
        if !resolved_metadata.is_file() {
            return Err(format!(
                "invariant: mutation authority path {} no longer resolves to a regular file",
                path.display()
            ));
        }

        let canonical_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let physical_identity = identity_key(
            &resolved_metadata,
            &canonical_path,
            force_canonical_identity,
        );
        let path_state = MutationPathState {
            is_symlink: link_metadata.file_type().is_symlink(),
            is_regular_file: resolved_metadata.is_file(),
            #[cfg(unix)]
            directory_entry_identity: unix_file_identity(&link_metadata),
        };

        Ok(Self {
            discovered_path: path.to_path_buf(),
            canonical_path,
            physical_identity,
            path_state,
        })
    }

    pub(crate) fn discovered_path(&self) -> &Path {
        &self.discovered_path
    }

    /// Refuse whenever the scanned object is no longer the same object, or
    /// whenever it has become the target that this operation would mutate.
    /// Callers invoke this directly before every source-open/archive boundary.
    pub(crate) fn revalidate_for_mutation(&self, target: Option<&Path>) -> Result<(), String> {
        let current = Self::capture(&self.discovered_path).map_err(|error| {
            format!(
                "invariant: mutation authority for {} cannot be revalidated: {error}",
                self.discovered_path.display()
            )
        })?;

        if let Some(target) = target {
            if target == self.discovered_path.as_path()
                || Self::capture(target)
                    .map(|candidate| candidate.physical_identity == current.physical_identity)
                    .unwrap_or(false)
            {
                return Err(format!(
                    "invariant: migration source {} became physically equal to target {}",
                    self.discovered_path.display(),
                    target.display()
                ));
            }
        }

        if current.canonical_path != self.canonical_path {
            return Err(format!(
                "invariant: mutation authority for {} changed canonical identity from {} to {}",
                self.discovered_path.display(),
                self.canonical_path.display(),
                current.canonical_path.display()
            ));
        }
        if current.physical_identity != self.physical_identity {
            return Err(format!(
                "invariant: mutation authority for {} changed physical identity",
                self.discovered_path.display()
            ));
        }
        if current.path_state != self.path_state {
            return Err(format!(
                "invariant: mutation authority for {} changed file type or symlink state",
                self.discovered_path.display()
            ));
        }

        Ok(())
    }
}

fn identity_key(
    metadata: &std::fs::Metadata,
    canonical_path: &Path,
    force_canonical_identity: bool,
) -> IdentityKey {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !force_canonical_identity && (metadata.dev() != 0 || metadata.ino() != 0) {
            return IdentityKey::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
            };
        }
    }

    #[cfg(not(unix))]
    let _ = (metadata, force_canonical_identity);

    IdentityKey::Canonical(canonical_path.display().to_string())
}

#[cfg(unix)]
fn unix_file_identity(metadata: &std::fs::Metadata) -> Option<UnixFileIdentity> {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev() != 0 || metadata.ino() != 0).then_some(UnixFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[derive(Debug)]
struct ResolvedPath {
    path: String,
    canonical_path: String,
    key: IdentityKey,
    sidecar_score: u8,
    mutation_authority: PhysicalMutationAuthority,
}

pub(crate) fn classify_paths(paths: impl IntoIterator<Item = PathBuf>) -> PhysicalDbInventory {
    let mut unique = paths.into_iter().collect::<Vec<_>>();
    unique.sort();
    unique.dedup();

    let mut grouped = BTreeMap::<IdentityKey, Vec<ResolvedPath>>::new();
    let mut unresolved_paths = Vec::new();

    for path in unique {
        match resolve_path(&path) {
            Ok(resolved) => grouped
                .entry(resolved.key.clone())
                .or_default()
                .push(resolved),
            Err(unresolved) => unresolved_paths.push(unresolved),
        }
    }

    let mut stores = grouped
        .into_iter()
        .map(|(key, mut paths)| {
            paths.sort_by(|a, b| a.path.cmp(&b.path));
            let mut aliases = paths
                .iter()
                .map(|path| path.path.clone())
                .collect::<Vec<_>>();
            aliases.sort();
            aliases.dedup();

            let canonical_path = paths
                .iter()
                .map(|path| path.canonical_path.clone())
                .min()
                .expect("identity group is non-empty");
            let mut open_candidates = paths
                .iter()
                .flat_map(|path| {
                    [
                        (path.path.clone(), path.sidecar_score),
                        (
                            path.canonical_path.clone(),
                            sqlite_sidecar_score(Path::new(&path.canonical_path)),
                        ),
                    ]
                })
                .collect::<Vec<_>>();
            open_candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            open_candidates.dedup_by(|a, b| a.0 == b.0);
            let mut sidecar_paths = open_candidates
                .iter()
                .filter(|candidate| candidate.1 > 0)
                .map(|candidate| candidate.0.clone())
                .collect::<Vec<_>>();
            sidecar_paths.sort();
            sidecar_paths.dedup();
            let (open_path, open_path_basis) = match open_candidates.first() {
                Some((path, score)) if *score > 0 => {
                    let basis = match *score {
                        3 => OpenPathBasis::WalAndShmVisible,
                        2 => OpenPathBasis::WalVisible,
                        1 => OpenPathBasis::ShmVisible,
                        _ => unreachable!("sidecar score is bounded to 0..=3"),
                    };
                    (path.clone(), basis)
                }
                _ => (canonical_path.clone(), OpenPathBasis::CanonicalFallback),
            };
            let primary_path = if aliases.contains(&open_path) {
                open_path.clone()
            } else {
                aliases
                    .iter()
                    .find(|path| {
                        std::fs::canonicalize(path)
                            .map(|canonical| canonical == Path::new(&open_path))
                            .unwrap_or(false)
                    })
                    .cloned()
                    .or_else(|| {
                        aliases
                            .iter()
                            .find(|path| path.as_str() == canonical_path)
                            .cloned()
                    })
                    .unwrap_or_else(|| aliases[0].clone())
            };
            let physical_id = match key {
                #[cfg(unix)]
                IdentityKey::Unix { device, inode } => format!("unix:{device}:{inode}"),
                IdentityKey::Canonical(path) => format!("path:{path}"),
            };
            let mutation_state = if sidecar_paths.len() > 1 {
                PhysicalStoreMutationState::AmbiguousPhysicalStore
            } else {
                PhysicalStoreMutationState::Authorized
            };
            let mutation_authority = (mutation_state
                == PhysicalStoreMutationState::Authorized)
                .then(|| {
                    paths
                        .iter()
                        .find(|path| path.path == primary_path)
                        .expect("primary alias belongs to its physical store")
                        .mutation_authority
                        .clone()
                });

            PhysicalDbStore {
                physical_id,
                canonical_path,
                open_path,
                open_path_basis,
                sidecar_paths,
                primary_path,
                aliases,
                open_failure_kind: None,
                mutation_state,
                mutation_authority,
            }
        })
        .collect::<Vec<_>>();
    stores.sort_by(|a, b| a.canonical_path.cmp(&b.canonical_path));
    unresolved_paths.sort_by(|a, b| a.path.cmp(&b.path));

    PhysicalDbInventory {
        stores,
        unresolved_paths,
    }
}

pub(crate) fn same_physical_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(left_metadata), Ok(right_metadata)) =
            (std::fs::metadata(left), std::fs::metadata(right))
        {
            if left_metadata.dev() != 0
                || left_metadata.ino() != 0
                || right_metadata.dev() != 0
                || right_metadata.ino() != 0
            {
                return left_metadata.dev() == right_metadata.dev()
                    && left_metadata.ino() == right_metadata.ino();
            }
        }
    }

    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn resolve_path(path: &Path) -> Result<ResolvedPath, UnresolvedDbPath> {
    let link_meta = std::fs::symlink_metadata(path);
    let is_symlink = link_meta
        .as_ref()
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false);
    let symlink_target = is_symlink
        .then(|| std::fs::read_link(path).ok())
        .flatten()
        .map(|target| target.display().to_string());

    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            let failure_kind = if is_symlink && error.kind() == std::io::ErrorKind::NotFound {
                InventoryFailureKind::BrokenSymlink
            } else {
                failure_kind_from_io(&error)
            };
            return Err(UnresolvedDbPath {
                path: path.to_path_buf(),
                is_symlink,
                symlink_target,
                target_exists: is_symlink.then_some(false),
                failure_kind,
                error: error.to_string(),
            });
        }
    };

    if !metadata.is_file() {
        return Err(UnresolvedDbPath {
            path: path.to_path_buf(),
            is_symlink,
            symlink_target,
            target_exists: is_symlink.then_some(true),
            failure_kind: InventoryFailureKind::Other,
            error: "resolved path is not a regular file".to_string(),
        });
    }

    let mutation_authority = match PhysicalMutationAuthority::capture(path) {
        Ok(authority) => authority,
        Err(error) => {
            return Err(UnresolvedDbPath {
                path: path.to_path_buf(),
                is_symlink,
                symlink_target,
                target_exists: is_symlink.then_some(true),
                failure_kind: InventoryFailureKind::Other,
                error,
            });
        }
    };
    let canonical_path = mutation_authority.canonical_path.display().to_string();
    let key = mutation_authority.physical_identity.clone();

    Ok(ResolvedPath {
        path: path.display().to_string(),
        canonical_path,
        key,
        sidecar_score: sqlite_sidecar_score(path),
        mutation_authority,
    })
}

fn sqlite_sidecar_score(path: &Path) -> u8 {
    let wal = sidecar(path, "-wal");
    let shm = sidecar(path, "-shm");
    let visible_wal = std::fs::metadata(wal)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    let visible_shm = std::fs::metadata(shm)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    u8::from(visible_wal) * 2 + u8::from(visible_shm)
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

pub(crate) fn classify_open_failure(error: &dyn std::fmt::Display) -> InventoryFailureKind {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("permission denied") || message.contains("readonly") {
        InventoryFailureKind::PermissionDenied
    } else if message.contains("not found") || message.contains("unable to open database file") {
        InventoryFailureKind::NotFound
    } else if message.contains("locked") || message.contains("busy") {
        InventoryFailureKind::Locked
    } else if message.contains("not a database") || message.contains("malformed") {
        InventoryFailureKind::NotDatabase
    } else if message.contains("schema") || message.contains("no such table") {
        InventoryFailureKind::SchemaMismatch
    } else if message.contains("no such module") || message.contains("vec0") {
        InventoryFailureKind::ExtensionUnavailable
    } else {
        InventoryFailureKind::Other
    }
}

pub(crate) fn classify_memory_failure(error: &memcore::MemoryError) -> InventoryFailureKind {
    match error {
        memcore::MemoryError::Sqlite(error) => match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt) => {
                InventoryFailureKind::NotDatabase
            }
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                InventoryFailureKind::Locked
            }
            Some(rusqlite::ErrorCode::CannotOpen) => InventoryFailureKind::NotFound,
            Some(rusqlite::ErrorCode::ReadOnly) => InventoryFailureKind::PermissionDenied,
            _ => classify_open_failure(error),
        },
        memcore::MemoryError::Io(error) => failure_kind_from_io(error),
        memcore::MemoryError::NotFound(_) => InventoryFailureKind::NotFound,
        memcore::MemoryError::SchemaMigrationOptInRequired { .. }
        | memcore::MemoryError::DbCreateTargetExists { .. } => InventoryFailureKind::SchemaMismatch,
        _ => classify_open_failure(error),
    }
}

fn failure_kind_from_io(error: &std::io::Error) -> InventoryFailureKind {
    match error.kind() {
        std::io::ErrorKind::NotFound => InventoryFailureKind::NotFound,
        std::io::ErrorKind::PermissionDenied => InventoryFailureKind::PermissionDenied,
        _ => InventoryFailureKind::Other,
    }
}

/// Open a SQLite store read-only while retaining live WAL visibility.
///
/// Unlike an `immutable=1` URI this sees committed WAL frames. The open flags
/// cannot create a database or grant schema/manifest mutation authority.
pub(crate) fn open_read_only_connection(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    memcore::db::register_sqlite_vec();
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_identity_groups_path_symlink_and_hardlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.db");
        let symlink = dir.path().join("symlink.db");
        let hardlink = dir.path().join("hardlink.db");
        std::fs::write(&real, b"sqlite identity fixture").unwrap();
        std::os::unix::fs::symlink(&real, &symlink).unwrap();
        std::fs::hard_link(&real, &hardlink).unwrap();

        let inventory = classify_paths(vec![real, symlink, hardlink]);
        assert_eq!(inventory.stores.len(), 1);
        assert_eq!(inventory.stores[0].aliases.len(), 3);
        assert!(inventory.unresolved_paths.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn multiple_live_sidecar_owners_make_store_ambiguous_and_unmutable() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.db");
        let second = dir.path().join("second.db");
        std::fs::write(&first, b"sqlite identity fixture").unwrap();
        std::fs::hard_link(&first, &second).unwrap();
        std::fs::write(sidecar(&first, "-wal"), b"first live WAL").unwrap();
        std::fs::write(sidecar(&second, "-wal"), b"second live WAL").unwrap();

        let inventory = classify_paths(vec![first, second]);
        assert_eq!(inventory.stores.len(), 1);
        assert_eq!(
            inventory.stores[0].mutation_state,
            PhysicalStoreMutationState::AmbiguousPhysicalStore
        );
        assert_eq!(inventory.stores[0].sidecar_paths.len(), 2);
        assert!(inventory.stores[0].mutation_authority.is_none());
    }

    #[test]
    fn canonical_fallback_authority_refuses_replacement_and_target_equality() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let replacement = dir.path().join("replacement.db");
        let target = dir.path().join("target.db");
        std::fs::write(&source, b"source object").unwrap();
        std::fs::write(&replacement, b"replacement object").unwrap();
        std::fs::write(&target, b"target object").unwrap();

        let authority = PhysicalMutationAuthority::capture_with_canonical_fallback(&source)
            .expect("portable canonical fallback authority");
        assert!(matches!(authority.physical_identity, IdentityKey::Canonical(_)));
        authority
            .revalidate_for_mutation(Some(&target))
            .expect("distinct canonical fallback target remains valid");
        assert!(authority
            .revalidate_for_mutation(Some(&source))
            .unwrap_err()
            .contains("physically equal"));

        std::fs::remove_file(&source).unwrap();
        std::fs::rename(&replacement, &source).unwrap();
        assert!(authority
            .revalidate_for_mutation(Some(&target))
            .unwrap_err()
            .contains("canonical identity"));
    }
}

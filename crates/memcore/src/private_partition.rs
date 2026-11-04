//! Physically isolated encrypted private partitions (tachi#1668).
//!
//! # Frozen design (issue/PR packet)
//!
//! 1. **Physical boundary.** One sealed file per `(trust_domain, subject)`.
//!    Durable path is *derived* (`sha256` of those ids) under a caller-supplied
//!    estate root. Path labels and first-open order are not authority. The
//!    SQLite remains in memory while open; only the sealed envelope is durable.
//! 2. **Key provider.** [`PartitionKeyProvider`] injects a 32-byte AES-256-GCM
//!    key via `vault-kit`. The kernel never reads environment variables.
//!    [`PrivatePartitionOpenContext::key_version`] is an identity token, never
//!    key material.
//! 3. **Open context.** Opaque `trust_domain_id` + `subject_id` + capability
//!    receipt + key version + revoked bit. Display names and carriers are not
//!    fields.
//! 4. **Capability lattice.** `Read` is required to open. `Write` is required
//!    to create, persist, or mutate. `Export` is required to emit a sealed
//!    backup. Missing capability is the same refusal as a wrong key.
//! 5. **Non-disclosure.** Every admission/crypto/path/stamp failure is
//!    [`MemoryError::PrivatePartitionRefused`] — no path, id, count, hash, or
//!    existence signal.
//! 6. **Schema / profile.** Fresh partitions are
//!    [`crate::db::StoreProfile::PortableKernel`] and stamp a write-once private-partition
//!    identity beside the #1585 profile/role stamps.
//! 7. **#1585.** Generic [`MemoryStore`] open/read-only/maintenance paths
//!    refuse a sealed envelope *and* a working SQLite file that carries the
//!    private-partition stamp. A private partition never satisfies `TachiFull`.
//! 8. **Filesystem trust.** The estate root is operator-owned storage. Final
//!    envelope and lock components are opened without following symlinks, but
//!    this format does not claim rollback protection against an attacker with
//!    arbitrary same-principal write access to the entire estate directory;
//!    that requires an external monotonic authority.
//!
//! Trust is not encoded in `MemoryEntry::scope`, path prefixes, or SQL
//! filters. Those remain defenses in depth only.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::db;
#[cfg(test)]
use crate::db::StoreProfile;
use crate::error::MemoryError;
use crate::store::immutable_supersession::ImmutableSupersessionTransaction;
use crate::types::MemoryEntry;
use crate::MemoryStore;

/// Write-once identity key: this file is a private partition (#1668 / #1585).
pub const STORE_PRIVATE_PARTITION_KEY: &str = "private_partition";

/// Private on-disk envelope marker. This is deliberately not part of the
/// public API: generic openers classify every non-SQLite image identically.
const SEALED_MAGIC: &[u8] = b"TACHI-PRIVPART-1\n";
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

#[cfg(feature = "test-support")]
type GenericOpenTestHook = std::sync::Arc<dyn Fn(&Path) + Send + Sync + 'static>;

#[cfg(feature = "test-support")]
fn generic_open_test_hooks(
) -> &'static std::sync::Mutex<std::collections::BTreeMap<PathBuf, GenericOpenTestHook>> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<PathBuf, GenericOpenTestHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// Owns one disposable path's before-open hook. Dropping an old guard cannot
/// remove another owner's later hook for the same path.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub struct GenericOpenTestHookGuard {
    db_path: PathBuf,
    hook: GenericOpenTestHook,
}

#[cfg(feature = "test-support")]
impl Drop for GenericOpenTestHookGuard {
    fn drop(&mut self) {
        let mut hooks = generic_open_test_hooks()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if hooks
            .get(&self.db_path)
            .is_some_and(|hook| std::sync::Arc::ptr_eq(hook, &self.hook))
        {
            hooks.remove(&self.db_path);
        }
    }
}

/// Invoke `hook` before every native header open of exactly this canonical
/// fixture path. The registry lock is released before invoking test code.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn install_before_generic_open_hook_for_test(
    db_path: &Path,
    hook: impl Fn(&Path) + Send + Sync + 'static,
) -> Result<GenericOpenTestHookGuard, String> {
    let db_path = fs::canonicalize(db_path)
        .map_err(|error| format!("canonicalize before-open fixture: {error}"))?;
    let hook: GenericOpenTestHook = std::sync::Arc::new(hook);
    let mut hooks = generic_open_test_hooks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if hooks.contains_key(&db_path) {
        return Err("a before-open hook already owns this fixture path".to_string());
    }
    hooks.insert(db_path.clone(), hook.clone());
    Ok(GenericOpenTestHookGuard { db_path, hook })
}

#[cfg(feature = "test-support")]
fn run_before_generic_open_hook_for_test(db_path: &Path) {
    let hook = generic_open_test_hooks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(db_path)
        .cloned();
    if let Some(hook) = hook {
        hook(db_path);
    }
}

const DERIVATION_DOMAIN: &[u8] = b"tachi.private_partition.v1";

/// Opaque admitted trust-domain identifier.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct TrustDomainId(String);

impl TrustDomainId {
    pub fn new(raw: impl Into<String>) -> Result<Self, MemoryError> {
        parse_opaque_id(raw.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TrustDomainId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TrustDomainId(<redacted>)")
    }
}

/// Opaque admitted subject identifier. Not a display name or carrier.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SubjectId(String);

impl SubjectId {
    pub fn new(raw: impl Into<String>) -> Result<Self, MemoryError> {
        parse_opaque_id(raw.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SubjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SubjectId(<redacted>)")
    }
}

/// Capability receipt: an opaque token the [`PartitionKeyProvider`] admits.
#[derive(Clone, PartialEq, Eq)]
pub struct CapabilityReceipt(String);

impl CapabilityReceipt {
    pub fn new(raw: impl Into<String>) -> Result<Self, MemoryError> {
        parse_opaque_id(raw.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CapabilityReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CapabilityReceipt(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PartitionCapability {
    Read,
    Write,
    Export,
}

/// Admitted open request. Caller-supplied path labels are not fields.
#[derive(Clone)]
pub struct PrivatePartitionOpenContext {
    pub trust_domain_id: TrustDomainId,
    pub subject_id: SubjectId,
    pub receipt: CapabilityReceipt,
    pub capabilities: BTreeSet<PartitionCapability>,
    pub key_version: String,
    pub revoked: bool,
}

impl PrivatePartitionOpenContext {
    #[cfg(test)]
    fn partition_id(&self) -> String {
        derive_partition_id(&self.trust_domain_id, &self.subject_id)
    }
}

impl fmt::Debug for PrivatePartitionOpenContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivatePartitionOpenContext")
            .field("trust_domain_id", &"<redacted>")
            .field("subject_id", &"<redacted>")
            .field("receipt", &"<redacted>")
            .field("capabilities", &self.capabilities)
            .field("key_version", &"<redacted>")
            .field("revoked", &self.revoked)
            .finish()
    }
}

/// Provider-granted admission. The caller's [`PrivatePartitionOpenContext`]
/// requests capabilities; this object is the trusted provider's grant.
#[derive(Clone, PartialEq, Eq)]
pub struct PartitionAdmission {
    trust_domain_id: TrustDomainId,
    subject_id: SubjectId,
    capabilities: BTreeSet<PartitionCapability>,
    key_version: String,
}

impl PartitionAdmission {
    pub fn new(
        trust_domain_id: TrustDomainId,
        subject_id: SubjectId,
        capabilities: impl IntoIterator<Item = PartitionCapability>,
        key_version: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let key_version = parse_opaque_id(key_version.into())?;
        let capabilities = capabilities.into_iter().collect();
        Ok(Self {
            trust_domain_id,
            subject_id,
            capabilities,
            key_version,
        })
    }

    fn effective_capabilities(
        &self,
        ctx: &PrivatePartitionOpenContext,
    ) -> Result<BTreeSet<PartitionCapability>, MemoryError> {
        if ctx.revoked
            || self.trust_domain_id != ctx.trust_domain_id
            || self.subject_id != ctx.subject_id
            || self.key_version != ctx.key_version
            || !ctx.capabilities.is_subset(&self.capabilities)
        {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        Ok(ctx.capabilities.clone())
    }
}

impl fmt::Debug for PartitionAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PartitionAdmission")
            .field("trust_domain_id", &"<redacted>")
            .field("subject_id", &"<redacted>")
            .field("capabilities", &self.capabilities)
            .field("key_version", &"<redacted>")
            .finish()
    }
}

/// Injected key material. Portable kernel never reads the environment.
pub trait PartitionKeyProvider {
    /// Validate the caller receipt and return the provider-bound grant and key
    /// as one operation. Keeping key materialization inside admission prevents
    /// a caller from constructing a look-alike [`PartitionAdmission`] and
    /// presenting it later as proof that a receipt was checked.
    fn admit_and_materialize(
        &self,
        ctx: &PrivatePartitionOpenContext,
    ) -> Result<(PartitionAdmission, [u8; 32]), MemoryError>;
}

/// Test-support key provider. Production callers should inject a provider backed
/// by their real admission authority; this fixture has no production
/// constructor.
#[derive(Clone)]
pub struct StaticKeyProvider {
    key: [u8; 32],
    admissions: Vec<StaticKeyAdmission>,
}

#[derive(Clone)]
struct StaticKeyAdmission {
    trust_domain_id: String,
    subject_id: String,
    receipt: String,
    capabilities: BTreeSet<PartitionCapability>,
    key_version: String,
}

#[cfg(any(test, feature = "test-support"))]
impl StaticKeyProvider {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key,
            admissions: Vec::new(),
        }
    }

    pub fn with_admission(
        mut self,
        trust_domain_id: impl Into<String>,
        subject_id: impl Into<String>,
        receipt: impl Into<String>,
        capabilities: impl IntoIterator<Item = PartitionCapability>,
        key_version: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let trust_domain_id = parse_opaque_id(trust_domain_id.into())?;
        let subject_id = parse_opaque_id(subject_id.into())?;
        let receipt = parse_opaque_id(receipt.into())?;
        let key_version = parse_opaque_id(key_version.into())?;
        self.admissions.push(StaticKeyAdmission {
            trust_domain_id,
            subject_id,
            receipt,
            capabilities: capabilities.into_iter().collect(),
            key_version,
        });
        Ok(self)
    }
}

impl fmt::Debug for StaticKeyProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StaticKeyProvider")
            .field("key", &"<redacted>")
            .field("admissions", &self.admissions.len())
            .finish()
    }
}

impl PartitionKeyProvider for StaticKeyProvider {
    fn admit_and_materialize(
        &self,
        ctx: &PrivatePartitionOpenContext,
    ) -> Result<(PartitionAdmission, [u8; 32]), MemoryError> {
        if ctx.revoked {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        let Some(admission) = self.admissions.iter().find(|admission| {
            admission.trust_domain_id == ctx.trust_domain_id.as_str()
                && admission.subject_id == ctx.subject_id.as_str()
                && admission.receipt == ctx.receipt.as_str()
                && admission.key_version == ctx.key_version
        }) else {
            return Err(MemoryError::PrivatePartitionRefused);
        };
        let admission = PartitionAdmission::new(
            ctx.trust_domain_id.clone(),
            ctx.subject_id.clone(),
            admission.capabilities.iter().copied(),
            admission.key_version.clone(),
        )
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
        Ok((admission, self.key))
    }
}

/// Opaque partition identity stamped inside the working store and encrypted
/// inside the sealed envelope. Trust-domain, subject, and key-version values
/// stay provider-side and are not serialized into memory metadata.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedPartition {
    pub partition_id: String,
}

impl fmt::Debug for AdmittedPartition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdmittedPartition")
            .field("partition_id", &"<redacted>")
            .finish()
    }
}

struct LivePartitionGuard {
    _lock_file: fs::File,
}

struct PartitionKey([u8; 32]);

impl PartitionKey {
    fn as_array(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Drop for PartitionKey {
    fn drop(&mut self) {
        vault_kit::zero_key(&mut self.0);
    }
}

impl LivePartitionGuard {
    fn acquire(path: &Path) -> Result<Self, MemoryError> {
        let lock_path = lock_path_for(path);
        let parent = ensure_sealed_parent(path)?;
        if lock_path.parent() != Some(parent) {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        let lock_file = open_live_lock_file(&lock_path)?;
        lock_file
            .try_lock()
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
        Ok(Self {
            _lock_file: lock_file,
        })
    }
}

/// Handle that does not expose a raw SQLite connection on the portable API.
pub struct PrivatePartition {
    store: MemoryStore,
    _live_guard: Option<LivePartitionGuard>,
    identity: AdmittedPartition,
    key: PartitionKey,
    sealed_path: Option<PathBuf>,
    can_write: bool,
    can_export: bool,
    dirty: bool,
}

impl PrivatePartition {
    /// Open or create the partition for `ctx` under `estate_root`.
    pub fn open(
        estate_root: &Path,
        ctx: &PrivatePartitionOpenContext,
        keys: &dyn PartitionKeyProvider,
    ) -> Result<Self, MemoryError> {
        let (admission, key) = admit_open(ctx, keys)?;
        let effective_capabilities = admission.effective_capabilities(ctx)?;
        let identity = identity_from_admission(&admission);
        let sealed_path = sealed_path_for(estate_root, &identity.partition_id);
        let exists = sealed_file_exists(&sealed_path)?;
        if !exists && !effective_capabilities.contains(&PartitionCapability::Write) {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        let live_guard = LivePartitionGuard::acquire(&sealed_path)?;
        let exists = sealed_file_exists(&sealed_path)?;
        if !exists && !effective_capabilities.contains(&PartitionCapability::Write) {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        let store = if exists {
            let bytes = unseal_file(&sealed_path, key.as_array(), &identity)?;
            MemoryStore::open_private_image(Some(&bytes), identity.clone())?
        } else {
            MemoryStore::open_private_image(None, identity.clone())?
        };
        verify_stamped_identity(&store, &identity)?;
        Ok(Self {
            store,
            _live_guard: Some(live_guard),
            identity,
            key,
            sealed_path: Some(sealed_path),
            can_write: effective_capabilities.contains(&PartitionCapability::Write),
            can_export: effective_capabilities.contains(&PartitionCapability::Export),
            dirty: !exists,
        })
    }

    /// In-memory / tempfile partition (tests). Still requires a live provider.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_in_memory(
        ctx: &PrivatePartitionOpenContext,
        keys: &dyn PartitionKeyProvider,
    ) -> Result<Self, MemoryError> {
        let (admission, key) = admit_open(ctx, keys)?;
        let effective_capabilities = admission.effective_capabilities(ctx)?;
        if !effective_capabilities.contains(&PartitionCapability::Write) {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        let identity = identity_from_admission(&admission);
        let store = MemoryStore::open_private_image(None, identity.clone())?;
        Ok(Self {
            store,
            _live_guard: None,
            identity,
            key,
            sealed_path: None,
            can_write: true,
            can_export: effective_capabilities.contains(&PartitionCapability::Export),
            dirty: true,
        })
    }

    pub fn identity(&self) -> &AdmittedPartition {
        &self.identity
    }

    pub fn insert_if_absent(
        &mut self,
        entry: &MemoryEntry,
    ) -> Result<db::InsertMemoryResult, MemoryError> {
        self.require_write()?;
        let result = self.store.insert_if_absent(entry)?;
        self.dirty = true;
        Ok(result)
    }

    pub fn get(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        self.store.get(id)
    }

    pub fn with_immutable_supersession_transaction<T>(
        &mut self,
        operation: impl FnMut(&mut ImmutableSupersessionTransaction<'_>) -> Result<T, MemoryError>,
    ) -> Result<T, MemoryError> {
        self.require_write()?;
        let changes_before = self.store.conn.total_changes();
        let result = self
            .store
            .with_immutable_supersession_transaction(operation)?;
        self.dirty |= self.store.conn.total_changes() != changes_before;
        Ok(result)
    }

    /// Encrypted backup. Refuses without `Export`; emits no path/count/hash.
    pub fn export_sealed(&self) -> Result<Vec<u8>, MemoryError> {
        if !self.can_export {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        encode_envelope(self.key.as_array(), &self.identity, &self.store)
    }

    pub fn persist(&mut self) -> Result<(), MemoryError> {
        if !self.dirty {
            return Ok(());
        }
        self.require_write()?;
        let Some(path) = self.sealed_path.clone() else {
            return Ok(());
        };
        persist_sealed(&self.store, self.key.as_array(), &self.identity, &path)?;
        let _authorization =
            db::authorize_reserved_reference_write(&self.store.reserved_reference_write)?;
        mark_partition_receipts_sealed(&self.store.conn, &self.identity.partition_id)?;
        self.dirty = false;
        Ok(())
    }

    pub fn close(mut self) -> Result<(), MemoryError> {
        self.persist()
    }

    fn require_write(&self) -> Result<(), MemoryError> {
        if self.can_write {
            Ok(())
        } else {
            Err(MemoryError::PrivatePartitionRefused)
        }
    }
}

fn generic_non_sqlite_image() -> MemoryError {
    MemoryError::InvalidArg("database path is not a database".to_string())
}

/// Resolve an existing path to its canonical target, then require a regular
/// file (not a link) carrying the SQLite header, read via SQLite's VFS. This
/// preserves the repository's supported global-store symlink while ensuring
/// SQLite receives the already-resolved target rather than racing a second
/// traversal of the caller path. Private envelopes and arbitrary malformed
/// files receive the same content-free refusal.
pub(crate) fn resolve_generic_open_path(db_path: &str) -> Result<PathBuf, MemoryError> {
    let path = Path::new(db_path);
    let resolved = match fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(path.to_path_buf()),
        Err(_) => return Err(generic_non_sqlite_image()),
    };
    #[cfg(feature = "test-support")]
    run_before_generic_open_hook_for_test(&resolved);
    let metadata = fs::symlink_metadata(&resolved).map_err(|_| generic_non_sqlite_image())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(generic_non_sqlite_image());
    }
    // #2137: never open/close a std::fs handle on a possibly live DB file.
    if metadata.len() > 0
        && db::read_db_file_prefix::<16>(&resolved).as_ref() != Some(SQLITE_HEADER)
    {
        return Err(generic_non_sqlite_image());
    }
    Ok(resolved)
}

/// Generic MemoryStore open: a working file that still carries the stamp
/// is not a portable/Tachi handle.
pub(crate) fn refuse_stamped_private_store(conn: &rusqlite::Connection) -> Result<(), MemoryError> {
    match db::store_identity::read_stamp(conn, STORE_PRIVATE_PARTITION_KEY) {
        Ok(Some(_)) => Err(MemoryError::PrivatePartitionRefused),
        Ok(None) => Ok(()),
        Err(_) => Err(MemoryError::PrivatePartitionRefused),
    }
}

fn identity_from_admission(admission: &PartitionAdmission) -> AdmittedPartition {
    AdmittedPartition {
        partition_id: derive_partition_id(&admission.trust_domain_id, &admission.subject_id),
    }
}

fn admit_open(
    ctx: &PrivatePartitionOpenContext,
    keys: &dyn PartitionKeyProvider,
) -> Result<(PartitionAdmission, PartitionKey), MemoryError> {
    if ctx.revoked || ctx.key_version.trim().is_empty() {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    if !ctx.capabilities.contains(&PartitionCapability::Read) {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    let (admission, key) = keys
        .admit_and_materialize(ctx)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    Ok((admission, PartitionKey(key)))
}

fn parse_opaque_id(raw: String) -> Result<String, MemoryError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed != raw || trimmed.chars().any(|c| c.is_control()) {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    Ok(raw)
}

fn derive_partition_id(trust_domain: &TrustDomainId, subject: &SubjectId) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DERIVATION_DOMAIN);
    hasher.update([0x1f]);
    hasher.update(trust_domain.as_str().as_bytes());
    hasher.update([0x1f]);
    hasher.update(subject.as_str().as_bytes());
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn sealed_path_for(estate_root: &Path, partition_id: &str) -> PathBuf {
    estate_root.join(partition_id).join("partition.sealed")
}

fn lock_path_for(path: &Path) -> PathBuf {
    path.with_extension("sealed.lock")
}

fn sealed_file_exists(path: &Path) -> Result<bool, MemoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                Err(MemoryError::PrivatePartitionRefused)
            } else {
                Ok(true)
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(MemoryError::PrivatePartitionRefused),
    }
}

fn open_live_lock_file(path: &Path) -> Result<fs::File, MemoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(MemoryError::PrivatePartitionRefused),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let metadata = file
        .metadata()
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    }
    Ok(file)
}

pub(crate) fn stamp_private_identity(
    conn: &rusqlite::Connection,
    identity: &AdmittedPartition,
) -> Result<(), MemoryError> {
    let payload =
        serde_json::to_string(identity).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    db::store_identity::write_stamp_if_absent(
        conn,
        STORE_PRIVATE_PARTITION_KEY,
        &payload,
        "open:private-partition",
    )
    .map(|_| ())
    .map_err(|_| MemoryError::PrivatePartitionRefused)
}

fn verify_stamped_identity(
    store: &MemoryStore,
    expected: &AdmittedPartition,
) -> Result<(), MemoryError> {
    let raw = db::store_identity::read_stamp(&store.conn, STORE_PRIVATE_PARTITION_KEY)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?
        .ok_or(MemoryError::PrivatePartitionRefused)?;
    // `read_stamp` returns the inner `value` string; we stored JSON as value.
    let stamped: AdmittedPartition =
        serde_json::from_str(&raw).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    if stamped != *expected {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    Ok(())
}

fn persist_sealed(
    store: &MemoryStore,
    key: &[u8; 32],
    identity: &AdmittedPartition,
    path: &Path,
) -> Result<(), MemoryError> {
    let parent = ensure_sealed_parent(path)?;
    let blob = encode_envelope(key, identity, store)?;
    ensure_replace_target_safe(path)?;
    let mut tmp =
        NamedTempFile::new_in(parent).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o600))
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    }
    tmp.write_all(&blob)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    tmp.as_file()
        .sync_all()
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let persisted = tmp
        .persist(path)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    persisted
        .sync_all()
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    sync_parent_dir(parent)?;
    Ok(())
}

fn ensure_sealed_parent(path: &Path) -> Result<&Path, MemoryError> {
    let parent = path.parent().ok_or(MemoryError::PrivatePartitionRefused)?;
    fs::create_dir_all(parent).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let metadata =
        fs::symlink_metadata(parent).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    }
    Ok(parent)
}

fn ensure_replace_target_safe(path: &Path) -> Result<(), MemoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(MemoryError::PrivatePartitionRefused)
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(MemoryError::PrivatePartitionRefused),
    }
}

fn sync_parent_dir(parent: &Path) -> Result<(), MemoryError> {
    #[cfg(unix)]
    {
        let dir = fs::File::open(parent).map_err(|_| MemoryError::PrivatePartitionRefused)?;
        dir.sync_all()
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct SealedEnvelope {
    nonce_b64: String,
    ciphertext_b64: String,
}

fn encode_envelope(
    key: &[u8; 32],
    identity: &AdmittedPartition,
    store: &MemoryStore,
) -> Result<Vec<u8>, MemoryError> {
    let sqlite = snapshot_sqlite_image(&store.conn, &identity.partition_id)?;
    let payload = serde_json::to_vec(&(identity, sqlite))
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let (ciphertext_b64, nonce_b64) =
        vault_kit::encrypt(key, &payload).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let envelope = SealedEnvelope {
        nonce_b64,
        ciphertext_b64,
    };
    let json = serde_json::to_vec(&envelope).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let mut out = Vec::with_capacity(SEALED_MAGIC.len() + json.len());
    out.extend_from_slice(SEALED_MAGIC);
    out.extend_from_slice(&json);
    Ok(out)
}

fn snapshot_sqlite_image(
    conn: &rusqlite::Connection,
    partition_id: &str,
) -> Result<Vec<u8>, MemoryError> {
    let mut snapshot =
        rusqlite::Connection::open_in_memory().map_err(|_| MemoryError::PrivatePartitionRefused)?;
    {
        let backup = rusqlite::backup::Backup::new(conn, &mut snapshot)
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
        backup
            .run_to_completion(128, Duration::from_millis(100), None)
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    }
    mark_partition_receipts_sealed(&snapshot, partition_id)?;
    let data = snapshot
        .serialize(rusqlite::MAIN_DB)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    Ok(data.to_vec())
}

fn mark_partition_receipts_sealed(
    conn: &rusqlite::Connection,
    partition_id: &str,
) -> Result<(), MemoryError> {
    let mut statement =
        conn.prepare("SELECT key, value_json FROM hard_state WHERE namespace = ?1 ORDER BY key")?;
    let rows = statement.query_map([crate::SUPERSESSION_RECEIPT_NAMESPACE], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut updates = Vec::new();
    for row in rows {
        let (key, value_json) = row?;
        let mut receipt: crate::SupersessionReceipt = serde_json::from_str(&value_json)?;
        if receipt.partition_id.as_deref() == Some(partition_id) && !receipt.durable {
            let before = receipt.clone();
            receipt.durable = true;
            updates.push((key, before, receipt));
        }
    }
    drop(statement);
    for (key, before, after) in updates {
        crate::store::immutable_supersession::replace_supersession_event_receipt_after_seal(
            conn, &before, &after,
        )?;
        if conn.execute(
            "UPDATE hard_state SET value_json = ?1, updated_at = ?2
             WHERE namespace = ?3 AND key = ?4 AND version = 1",
            rusqlite::params![
                serde_json::to_string(&after)?,
                db::now_utc_iso(),
                crate::SUPERSESSION_RECEIPT_NAMESPACE,
                key,
            ],
        )? != 1
        {
            return Err(MemoryError::PrivatePartitionRefused);
        }
    }
    Ok(())
}

fn unseal_file(
    path: &Path,
    key: &[u8; 32],
    expected: &AdmittedPartition,
) -> Result<Vec<u8>, MemoryError> {
    let bytes = read_regular_file_no_follow(path)?;
    if !bytes.starts_with(SEALED_MAGIC) {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    let envelope: SealedEnvelope = serde_json::from_slice(&bytes[SEALED_MAGIC.len()..])
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let payload = vault_kit::decrypt(key, &envelope.ciphertext_b64, &envelope.nonce_b64)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let (identity, sqlite): (AdmittedPartition, Vec<u8>) =
        serde_json::from_slice(&payload).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    if identity != *expected {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    Ok(sqlite)
}

fn read_regular_file_no_follow(path: &Path) -> Result<Vec<u8>, MemoryError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options
        .open(path)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let metadata = file
        .metadata()
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    Ok(bytes)
}

#[cfg(test)]
mod refusal_order_tests;

#[cfg(test)]
mod tests;

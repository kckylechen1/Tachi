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

/// On-disk magic. Generic open inspects this before handing the file to SQLite.
pub const SEALED_MAGIC: &[u8] = b"TACHI-PRIVPART-1\n";

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
    key: [u8; 32],
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
            let bytes = unseal_file(&sealed_path, &key, &identity)?;
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
        let result = self
            .store
            .with_immutable_supersession_transaction(operation)?;
        self.dirty = true;
        Ok(result)
    }

    /// Encrypted backup. Refuses without `Export`; emits no path/count/hash.
    pub fn export_sealed(&self) -> Result<Vec<u8>, MemoryError> {
        if !self.can_export {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        encode_envelope(&self.key, &self.identity, &self.store)
    }

    pub fn persist(&mut self) -> Result<(), MemoryError> {
        if !self.dirty {
            return Ok(());
        }
        self.require_write()?;
        let Some(path) = self.sealed_path.clone() else {
            return Ok(());
        };
        persist_sealed(&self.store, &self.key, &self.identity, &path)?;
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

impl Drop for PrivatePartition {
    fn drop(&mut self) {
        vault_kit::zero_key(&mut self.key);
    }
}

/// True when `path` is a sealed private-partition envelope.
pub fn path_is_sealed_partition(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; SEALED_MAGIC.len()];
    file.read_exact(&mut magic).is_ok() && magic == SEALED_MAGIC
}

/// Generic MemoryStore open: sealed envelope never becomes a connection.
pub(crate) fn refuse_generic_open_path(db_path: &str) -> Result<(), MemoryError> {
    if path_is_sealed_partition(Path::new(db_path)) {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    Ok(())
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
) -> Result<(PartitionAdmission, [u8; 32]), MemoryError> {
    if ctx.revoked || ctx.key_version.trim().is_empty() {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    if !ctx.capabilities.contains(&PartitionCapability::Read) {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    keys.admit_and_materialize(ctx)
        .map_err(|_| MemoryError::PrivatePartitionRefused)
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
    let file = options
        .open(path)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
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
    let raw = db::store_identity::read_stamp(&store.conn, STORE_PRIVATE_PARTITION_KEY)?
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
            receipt.durable = true;
            updates.push((key, serde_json::to_string(&receipt)?));
        }
    }
    drop(statement);
    for (key, value_json) in updates {
        if conn.execute(
            "UPDATE hard_state SET value_json = ?1, updated_at = ?2
             WHERE namespace = ?3 AND key = ?4 AND version = 1",
            rusqlite::params![
                value_json,
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
mod tests {
    use super::*;
    use crate::db::InsertMemoryResult;

    const TRUST_DOMAIN: &str = "td-alpha";
    const RECEIPT_OK: &str = "rcpt-ok";
    const KEY_VERSION: &str = "kv1";
    const TEST_KEY: [u8; 32] = [7u8; 32];

    fn ctx(
        subject: &str,
        receipt: &str,
        caps: &[PartitionCapability],
        revoked: bool,
    ) -> PrivatePartitionOpenContext {
        PrivatePartitionOpenContext {
            trust_domain_id: TrustDomainId::new(TRUST_DOMAIN).unwrap(),
            subject_id: SubjectId::new(subject).unwrap(),
            receipt: CapabilityReceipt::new(receipt).unwrap(),
            capabilities: caps.iter().copied().collect(),
            key_version: KEY_VERSION.to_string(),
            revoked,
        }
    }

    fn keys() -> StaticKeyProvider {
        keys_for_subjects(&[
            (
                "subject-alice",
                &[
                    PartitionCapability::Read,
                    PartitionCapability::Write,
                    PartitionCapability::Export,
                ][..],
            ),
            (
                "subject-bob",
                &[
                    PartitionCapability::Read,
                    PartitionCapability::Write,
                    PartitionCapability::Export,
                ][..],
            ),
        ])
    }

    fn keys_for_subjects(subjects: &[(&str, &[PartitionCapability])]) -> StaticKeyProvider {
        let mut provider = StaticKeyProvider::new(TEST_KEY);
        for (subject, capabilities) in subjects {
            provider = provider
                .with_admission(
                    TRUST_DOMAIN,
                    *subject,
                    RECEIPT_OK,
                    capabilities.iter().copied(),
                    KEY_VERSION,
                )
                .unwrap();
        }
        provider
    }

    fn entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/".to_string(),
            summary: String::new(),
            text: text.to_string(),
            importance: 0.5,
            timestamp: "2026-08-17T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
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

    #[test]
    fn single_write_persist_drop_reopen_retains_committed_row() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let keys = keys();
        {
            let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
            assert!(matches!(
                part.insert_if_absent(&entry("first-row", "committed-before-seal"))
                    .unwrap(),
                InsertMemoryResult::Inserted
            ));
            part.persist().unwrap();
        }
        let reopened = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        assert_eq!(
            reopened.get("first-row").unwrap().unwrap().text,
            "committed-before-seal"
        );
    }

    #[test]
    fn drop_without_explicit_persist_does_not_create_successful_seal() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let keys = keys();
        {
            let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
            part.insert_if_absent(&entry("unpersisted-row", "must-not-claim-success"))
                .unwrap();
        }
        let sealed = root
            .path()
            .join(ctx.partition_id())
            .join("partition.sealed");
        assert!(
            !sealed.exists(),
            "Drop must not silently claim persistence success"
        );

        let reopened = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        assert!(reopened.get("unpersisted-row").unwrap().is_none());
    }

    #[test]
    fn second_live_opener_refuses_until_first_handle_drops() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let keys = keys();
        let first = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        let err = match PrivatePartition::open(root.path(), &ctx, &keys) {
            Err(err) => err,
            Ok(_) => panic!("second live opener must refuse"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
        drop(first);
        PrivatePartition::open(root.path(), &ctx, &keys)
            .expect("dropping the first handle releases exclusive ownership");
    }

    #[cfg(unix)]
    #[test]
    fn live_lock_symlink_is_refused_without_touching_its_target() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let partition_dir = root.path().join(ctx.partition_id());
        fs::create_dir_all(&partition_dir).unwrap();
        let lock_path = lock_path_for(&partition_dir.join("partition.sealed"));
        let victim = root.path().join("lock-victim.txt");
        fs::write(&victim, b"do-not-touch").unwrap();
        std::os::unix::fs::symlink(&victim, &lock_path).unwrap();

        let err = match PrivatePartition::open(root.path(), &ctx, &keys()) {
            Err(err) => err,
            Ok(_) => panic!("a symlink must never serve as the partition lock"),
        };

        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
        assert_eq!(fs::read(&victim).unwrap(), b"do-not-touch");
        assert!(fs::symlink_metadata(lock_path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn predictable_tmp_symlink_is_not_followed_or_clobbered() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let keys = keys();
        {
            let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
            part.insert_if_absent(&entry("row-one", "one")).unwrap();
            part.persist().unwrap();
        }

        let sealed = root
            .path()
            .join(ctx.partition_id())
            .join("partition.sealed");
        let predictable_tmp = sealed.with_extension("sealed.tmp");
        let victim = root.path().join("victim.txt");
        fs::write(&victim, b"do-not-clobber").unwrap();
        std::os::unix::fs::symlink(&victim, &predictable_tmp).unwrap();

        {
            let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
            part.insert_if_absent(&entry("row-two", "two")).unwrap();
            part.persist().unwrap();
        }

        assert_eq!(fs::read(&victim).unwrap(), b"do-not-clobber");
        assert!(fs::symlink_metadata(&predictable_tmp)
            .unwrap()
            .file_type()
            .is_symlink());
        let reopened = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        assert_eq!(reopened.get("row-two").unwrap().unwrap().text, "two");
    }

    #[cfg(unix)]
    #[test]
    fn sealed_reader_refuses_symlink_to_a_valid_prior_envelope() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let keys = keys();
        PrivatePartition::open(root.path(), &ctx, &keys)
            .unwrap()
            .persist()
            .unwrap();
        let sealed = root
            .path()
            .join(ctx.partition_id())
            .join("partition.sealed");
        let prior = sealed.with_extension("sealed.prior");
        fs::rename(&sealed, &prior).unwrap();
        std::os::unix::fs::symlink(&prior, &sealed).unwrap();

        let err = match PrivatePartition::open(root.path(), &ctx, &keys) {
            Err(err) => err,
            Ok(_) => panic!("sealed envelope symlinks must not be followed"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    }

    #[test]
    fn raw_admin_openers_refuse_a_stamped_private_image() {
        let ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let part = PrivatePartition::open_in_memory(&ctx, &keys()).unwrap();
        let image = snapshot_sqlite_image(&part.store.conn, &part.identity.partition_id).unwrap();
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&image).unwrap();
        file.as_file().sync_all().unwrap();

        assert!(crate::db::open_raw(file.path()).is_err());
        assert!(crate::db::open_for_wal_checkpoint(&file.path().to_string_lossy()).is_err());
        let uri = format!("file:{}?mode=ro&immutable=1", file.path().display());
        assert!(crate::db::open_immutable_readonly(&uri).is_err());
    }

    #[test]
    fn caller_cannot_self_escalate_provider_capabilities() {
        let root = tempfile::tempdir().unwrap();
        let write_ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let write_keys = keys();
        {
            let mut part = PrivatePartition::open(root.path(), &write_ctx, &write_keys).unwrap();
            part.insert_if_absent(&entry("existing", "present"))
                .unwrap();
            part.persist().unwrap();
        }

        let read_only_keys =
            keys_for_subjects(&[("subject-alice", &[PartitionCapability::Read][..])]);
        let escalated_ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[
                PartitionCapability::Read,
                PartitionCapability::Write,
                PartitionCapability::Export,
            ],
            false,
        );
        let err = match PrivatePartition::open(root.path(), &escalated_ctx, &read_only_keys) {
            Err(err) => err,
            Ok(_) => panic!("caller-requested Write/Export must not exceed provider grant"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));

        let read_ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read],
            false,
        );
        let part = PrivatePartition::open(root.path(), &read_ctx, &read_only_keys)
            .expect("provider-granted read remains admitted");
        assert_eq!(part.get("existing").unwrap().unwrap().text, "present");
    }

    #[test]
    fn read_only_handle_closes_without_rewriting_the_sealed_partition() {
        let root = tempfile::tempdir().unwrap();
        let write_ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let write_keys = keys();
        {
            let mut part = PrivatePartition::open(root.path(), &write_ctx, &write_keys).unwrap();
            part.insert_if_absent(&entry("existing", "present"))
                .unwrap();
            part.close().unwrap();
        }
        let sealed = root
            .path()
            .join(write_ctx.partition_id())
            .join("partition.sealed");
        let before = fs::read(&sealed).unwrap();

        let read_ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read],
            false,
        );
        let read_keys = keys_for_subjects(&[("subject-alice", &[PartitionCapability::Read][..])]);
        let part = PrivatePartition::open(root.path(), &read_ctx, &read_keys).unwrap();
        assert_eq!(part.get("existing").unwrap().unwrap().text, "present");
        part.close().unwrap();

        assert_eq!(
            fs::read(sealed).unwrap(),
            before,
            "closing a clean read-only handle must not rewrite the encrypted partition"
        );
    }

    #[test]
    fn private_partition_debug_and_envelope_redact_secret_identity_material() {
        let root = tempfile::tempdir().unwrap();
        let ctx = PrivatePartitionOpenContext {
            trust_domain_id: TrustDomainId::new("td-secret-debug-domain").unwrap(),
            subject_id: SubjectId::new("subject-secret-debug-id").unwrap(),
            receipt: CapabilityReceipt::new("receipt-secret-debug-token").unwrap(),
            capabilities: [PartitionCapability::Read, PartitionCapability::Write]
                .into_iter()
                .collect(),
            key_version: "kv-secret-debug-version".to_string(),
            revoked: false,
        };
        let keys = StaticKeyProvider::new(TEST_KEY)
            .with_admission(
                "td-secret-debug-domain",
                "subject-secret-debug-id",
                "receipt-secret-debug-token",
                [PartitionCapability::Read, PartitionCapability::Write],
                "kv-secret-debug-version",
            )
            .unwrap();
        let debug_surfaces = [
            format!("{ctx:?}"),
            format!("{:?}", ctx.trust_domain_id),
            format!("{:?}", ctx.subject_id),
            format!("{:?}", ctx.receipt),
            format!("{keys:?}"),
        ];
        for rendered in debug_surfaces {
            for secret in [
                "td-secret-debug-domain",
                "subject-secret-debug-id",
                "receipt-secret-debug-token",
                "kv-secret-debug-version",
                "7, 7, 7",
            ] {
                assert!(
                    !rendered.contains(secret),
                    "debug surface leaked {secret}: {rendered}"
                );
            }
        }

        let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        part.insert_if_absent(&entry("debug-redaction-row", "stored"))
            .unwrap();
        part.persist().unwrap();
        let sealed = root
            .path()
            .join(ctx.partition_id())
            .join("partition.sealed");
        let sealed_text = String::from_utf8_lossy(&fs::read(sealed).unwrap()).into_owned();
        for secret in [
            "td-secret-debug-domain",
            "subject-secret-debug-id",
            "receipt-secret-debug-token",
            "kv-secret-debug-version",
            &ctx.partition_id(),
        ] {
            assert!(
                !sealed_text.contains(secret),
                "sealed envelope plaintext leaked {secret}: {sealed_text}"
            );
        }
    }

    #[test]
    fn denied_contexts_refuse_before_creating_or_opening_store() {
        let root = tempfile::tempdir().unwrap();
        let keys = keys();
        let missing_read = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Write],
            false,
        );
        let err = match PrivatePartition::open(root.path(), &missing_read, &keys) {
            Err(err) => err,
            Ok(_) => panic!("missing Read must refuse"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
        assert!(!root.path().join(missing_read.partition_id()).exists());

        let wrong_receipt = ctx(
            "subject-alice",
            "receipt-wrong",
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let err = match PrivatePartition::open(root.path(), &wrong_receipt, &keys) {
            Err(err) => err,
            Ok(_) => panic!("wrong receipt must refuse"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
        assert!(!root.path().join(wrong_receipt.partition_id()).exists());

        let revoked = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            true,
        );
        let err = match PrivatePartition::open(root.path(), &revoked, &keys) {
            Err(err) => err,
            Ok(_) => panic!("revoked context must refuse"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
        assert!(!root.path().join(revoked.partition_id()).exists());
    }

    #[test]
    fn two_subjects_cannot_read_each_others_content_or_existence() {
        let root = tempfile::tempdir().unwrap();
        let alice = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let bob = ctx(
            "subject-bob",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let keys = keys();
        {
            let mut part = PrivatePartition::open(root.path(), &alice, &keys).unwrap();
            assert!(matches!(
                part.insert_if_absent(&entry("alice-secret", "SECRET-A"))
                    .unwrap(),
                InsertMemoryResult::Inserted
            ));
            part.persist().unwrap();
        }
        let bob_part = PrivatePartition::open(root.path(), &bob, &keys).unwrap();
        assert!(bob_part.get("alice-secret").unwrap().is_none());
        // Opening alice's sealed file under bob's context is not possible: path
        // is derived. Pointing MemoryStore at alice's sealed file refuses.
        let alice_id = alice.partition_id();
        let sealed = root.path().join(alice_id).join("partition.sealed");
        let err = match MemoryStore::open(&sealed.to_string_lossy()) {
            Err(err) => err,
            Ok(_) => panic!("generic open must not yield a private-partition handle"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
        assert_eq!(err.to_string(), "private partition refused");
        assert!(!err.to_string().contains("alice"));
        let _ = bob_part;
    }

    #[test]
    fn different_receipt_same_display_shape_refuses() {
        let root = tempfile::tempdir().unwrap();
        let ok = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let keys = keys();
        PrivatePartition::open(root.path(), &ok, &keys)
            .unwrap()
            .persist()
            .unwrap();
        let spoofed = ctx(
            "subject-alice",
            "rcpt-other",
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let err = match PrivatePartition::open(root.path(), &spoofed, &keys) {
            Err(err) => err,
            Ok(_) => panic!("spoofed receipt must not open"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    }

    #[test]
    fn wrong_path_cannot_acquire_authority() {
        let root = tempfile::tempdir().unwrap();
        let keys = keys();
        let alice = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let bob = ctx(
            "subject-bob",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        {
            let mut part = PrivatePartition::open(root.path(), &alice, &keys).unwrap();
            part.insert_if_absent(&entry("alice-secret", "SECRET-A"))
                .unwrap();
            part.persist().unwrap();
        }
        let alice_sealed = root
            .path()
            .join(alice.partition_id())
            .join("partition.sealed");
        let bob_dir = root.path().join(bob.partition_id());
        fs::create_dir_all(&bob_dir).unwrap();
        fs::copy(&alice_sealed, bob_dir.join("partition.sealed")).unwrap();
        let err = match PrivatePartition::open(root.path(), &bob, &keys) {
            Err(err) => err,
            Ok(_) => panic!("copied sealed file must not open as bob"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    }

    #[test]
    fn revoked_context_refuses_even_if_file_present() {
        let root = tempfile::tempdir().unwrap();
        let keys = keys();
        let live = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        PrivatePartition::open(root.path(), &live, &keys)
            .unwrap()
            .persist()
            .unwrap();
        let revoked = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            true,
        );
        let err = match PrivatePartition::open(root.path(), &revoked, &keys) {
            Err(err) => err,
            Ok(_) => panic!("revoked context must not open"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    }

    #[test]
    fn export_without_capability_refuses_and_mentions_nothing() {
        let ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let keys = keys();
        let part = PrivatePartition::open_in_memory(&ctx, &keys).unwrap();
        let err = match part.export_sealed() {
            Err(err) => err,
            Ok(_) => panic!("export without capability must refuse"),
        };
        assert!(matches!(err, MemoryError::PrivatePartitionRefused));
        assert!(!format!("{err:?}").contains("SECRET"));
    }

    #[test]
    fn cross_partition_claim_cannot_see_foreign_rows() {
        let keys = keys();
        let alice_ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let bob_ctx = ctx(
            "subject-bob",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let mut alice = PrivatePartition::open_in_memory(&alice_ctx, &keys).unwrap();
        let mut bob = PrivatePartition::open_in_memory(&bob_ctx, &keys).unwrap();
        alice
            .insert_if_absent(&entry("alice-row", "SECRET-A"))
            .unwrap();
        bob.insert_if_absent(&entry("bob-row", "SECRET-B")).unwrap();
        // A source that only exists in Bob's partition cannot be claimed on Alice.
        let err = match alice.with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("bob-row", "alice-row")
        }) {
            Err(err) => err,
            Ok(receipt) => panic!("foreign source must not claim: {receipt:?}"),
        };
        assert!(
            err.to_string().contains("CAS refused") || err.to_string().contains("refused"),
            "{err}"
        );
        let bob_row = bob.get("bob-row").unwrap().expect("bob row stays");
        assert_eq!(bob_row.text, "SECRET-B");
        let result = alice
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("alice-row", "alice-successor")
            })
            .expect("same-partition claim with in-flight target remains legal");
        assert_eq!(
            result,
            crate::SupersessionCommitResult::Applied,
            "same-partition claim must install the edge"
        );
        let receipt_id = crate::SupersessionReceipt::id_for(
            crate::SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
            crate::SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
            "alice-row",
            "alice-successor",
        );
        let (receipt_json, _) = alice
            .store
            .get_state_kv(crate::SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .expect("read private-partition receipt")
            .expect("private-partition claim persists a receipt");
        let receipt: crate::SupersessionReceipt =
            serde_json::from_str(&receipt_json).expect("deserialize private receipt");
        assert_eq!(
            receipt.partition_id.as_deref(),
            Some(alice.identity().partition_id.as_str())
        );
        assert!(receipt.durable);
        assert_eq!(alice.identity().partition_id, alice_ctx.partition_id());
        assert_ne!(alice.identity().partition_id, bob.identity().partition_id);
    }

    #[test]
    fn portable_profile_is_stamped_not_tachi_full() {
        let ctx = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[
                PartitionCapability::Read,
                PartitionCapability::Write,
                PartitionCapability::Export,
            ],
            false,
        );
        let keys = keys();
        let part = PrivatePartition::open_in_memory(&ctx, &keys).unwrap();
        assert_eq!(part.store.profile, StoreProfile::PortableKernel);
        assert_eq!(part.identity().partition_id, ctx.partition_id());
    }
}

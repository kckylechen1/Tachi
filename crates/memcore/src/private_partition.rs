//! Physically isolated encrypted private partitions (tachi#1668).
//!
//! # Frozen design (issue/PR packet)
//!
//! 1. **Physical boundary.** One sealed file per `(trust_domain, subject)`.
//!    Durable path is *derived* (`sha256` of those ids) under a caller-supplied
//!    estate root. Path labels and first-open order are not authority. The
//!    working SQLite file is a process-private tempfile; only the sealed
//!    envelope is durable.
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
//!    [`StoreProfile::PortableKernel`] and stamp a write-once private-partition
//!    identity beside the #1585 profile/role stamps.
//! 7. **#1585.** Generic [`MemoryStore`] open/read-only/maintenance paths
//!    refuse a sealed envelope *and* a working SQLite file that carries the
//!    private-partition stamp. A private partition never satisfies `TachiFull`.
//!
//! Trust is not encoded in `MemoryEntry::scope`, path prefixes, or SQL
//! filters. Those remain defenses in depth only.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::db::{self, DbOpenContext, StoreProfile};
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrustDomainId(String);

impl TrustDomainId {
    pub fn new(raw: impl Into<String>) -> Result<Self, MemoryError> {
        parse_opaque_id(raw.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque admitted subject identifier. Not a display name or carrier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubjectId(String);

impl SubjectId {
    pub fn new(raw: impl Into<String>) -> Result<Self, MemoryError> {
        parse_opaque_id(raw.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Capability receipt: an opaque token the [`PartitionKeyProvider`] admits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityReceipt(String);

impl CapabilityReceipt {
    pub fn new(raw: impl Into<String>) -> Result<Self, MemoryError> {
        parse_opaque_id(raw.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PartitionCapability {
    Read,
    Write,
    Export,
}

/// Admitted open request. Caller-supplied path labels are not fields.
#[derive(Debug, Clone)]
pub struct PrivatePartitionOpenContext {
    pub trust_domain_id: TrustDomainId,
    pub subject_id: SubjectId,
    pub receipt: CapabilityReceipt,
    pub capabilities: BTreeSet<PartitionCapability>,
    pub key_version: String,
    pub revoked: bool,
}

impl PrivatePartitionOpenContext {
    pub fn partition_id(&self) -> String {
        derive_partition_id(&self.trust_domain_id, &self.subject_id)
    }
}

/// Injected key material. Portable kernel never reads the environment.
pub trait PartitionKeyProvider {
    fn admit(&self, ctx: &PrivatePartitionOpenContext) -> Result<(), MemoryError>;
    fn materialize_key(&self, ctx: &PrivatePartitionOpenContext) -> Result<[u8; 32], MemoryError>;
}

/// One key; admits a single receipt unless the context is revoked.
#[derive(Debug, Clone)]
pub struct StaticKeyProvider {
    key: [u8; 32],
    admitted_receipt: String,
}

impl StaticKeyProvider {
    pub fn new(key: [u8; 32], admitted_receipt: impl Into<String>) -> Self {
        Self {
            key,
            admitted_receipt: admitted_receipt.into(),
        }
    }
}

impl PartitionKeyProvider for StaticKeyProvider {
    fn admit(&self, ctx: &PrivatePartitionOpenContext) -> Result<(), MemoryError> {
        if ctx.revoked || ctx.receipt.as_str() != self.admitted_receipt {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        Ok(())
    }

    fn materialize_key(&self, ctx: &PrivatePartitionOpenContext) -> Result<[u8; 32], MemoryError> {
        self.admit(ctx)?;
        Ok(self.key)
    }
}

/// Identity stamped inside the working store and sealed envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedPartition {
    pub partition_id: String,
    pub trust_domain_id: String,
    pub subject_id: String,
    pub key_version: String,
}

/// Handle that does not expose a raw SQLite connection on the portable API.
pub struct PrivatePartition {
    store: MemoryStore,
    /// Keeps the working SQLite file alive for the handle's lifetime.
    _working: NamedTempFile,
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
        admit_open(ctx, keys)?;
        let key = keys
            .materialize_key(ctx)
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
        let identity = identity_from_ctx(ctx);
        let sealed_path = sealed_path_for(estate_root, &identity.partition_id);
        let exists = match fs::metadata(&sealed_path) {
            Ok(_) => true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err(MemoryError::PrivatePartitionRefused),
        };
        if !exists && !ctx.capabilities.contains(&PartitionCapability::Write) {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        let working = NamedTempFile::new().map_err(|_| MemoryError::PrivatePartitionRefused)?;
        let store = if exists {
            let bytes = unseal_file(&sealed_path, &key, &identity)?;
            fs::write(working.path(), bytes).map_err(|_| MemoryError::PrivatePartitionRefused)?;
            MemoryStore::open_private_working_file(
                &working.path().to_string_lossy(),
                &DbOpenContext::open_existing_deny().with_profile(StoreProfile::PortableKernel),
                identity.clone(),
            )?
        } else {
            MemoryStore::open_private_working_file(
                &working.path().to_string_lossy(),
                &DbOpenContext::create_fresh().with_profile(StoreProfile::PortableKernel),
                identity.clone(),
            )?
        };
        verify_stamped_identity(&store, &identity)?;
        Ok(Self {
            store,
            _working: working,
            identity,
            key,
            sealed_path: Some(sealed_path),
            can_write: ctx.capabilities.contains(&PartitionCapability::Write),
            can_export: ctx.capabilities.contains(&PartitionCapability::Export),
            dirty: !exists,
        })
    }

    /// In-memory / tempfile partition (tests). Still requires a live provider.
    pub fn open_in_memory(
        ctx: &PrivatePartitionOpenContext,
        keys: &dyn PartitionKeyProvider,
    ) -> Result<Self, MemoryError> {
        admit_open(ctx, keys)?;
        if !ctx.capabilities.contains(&PartitionCapability::Write) {
            return Err(MemoryError::PrivatePartitionRefused);
        }
        let key = keys
            .materialize_key(ctx)
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
        let identity = identity_from_ctx(ctx);
        let working = NamedTempFile::new().map_err(|_| MemoryError::PrivatePartitionRefused)?;
        let store = MemoryStore::open_private_working_file(
            &working.path().to_string_lossy(),
            &DbOpenContext::create_fresh().with_profile(StoreProfile::PortableKernel),
            identity.clone(),
        )?;
        Ok(Self {
            store,
            _working: working,
            identity,
            key,
            sealed_path: None,
            can_write: true,
            can_export: ctx.capabilities.contains(&PartitionCapability::Export),
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
        encode_envelope(&self.key, &self.identity, self._working.path())
    }

    pub fn persist(&mut self) -> Result<(), MemoryError> {
        self.require_write()?;
        let Some(path) = self.sealed_path.clone() else {
            return Ok(());
        };
        persist_sealed(self._working.path(), &self.key, &self.identity, &path)?;
        self.dirty = false;
        Ok(())
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
        if self.can_write && self.dirty {
            if let Some(path) = self.sealed_path.clone() {
                let _ = persist_sealed(self._working.path(), &self.key, &self.identity, &path);
            }
        }
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

fn identity_from_ctx(ctx: &PrivatePartitionOpenContext) -> AdmittedPartition {
    AdmittedPartition {
        partition_id: ctx.partition_id(),
        trust_domain_id: ctx.trust_domain_id.as_str().to_string(),
        subject_id: ctx.subject_id.as_str().to_string(),
        key_version: ctx.key_version.clone(),
    }
}

fn admit_open(
    ctx: &PrivatePartitionOpenContext,
    keys: &dyn PartitionKeyProvider,
) -> Result<(), MemoryError> {
    if ctx.revoked || ctx.key_version.trim().is_empty() {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    if !ctx.capabilities.contains(&PartitionCapability::Read) {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    keys.admit(ctx)
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
    working: &Path,
    key: &[u8; 32],
    identity: &AdmittedPartition,
    path: &Path,
) -> Result<(), MemoryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| MemoryError::PrivatePartitionRefused)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let blob = encode_envelope(key, identity, working)?;
    let tmp = path.with_extension("sealed.tmp");
    {
        let mut file = fs::File::create(&tmp).map_err(|_| MemoryError::PrivatePartitionRefused)?;
        file.write_all(&blob)
            .map_err(|_| MemoryError::PrivatePartitionRefused)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
    }
    fs::rename(&tmp, path).map_err(|_| MemoryError::PrivatePartitionRefused)
}

#[derive(Serialize, Deserialize)]
struct SealedEnvelope {
    key_version: String,
    partition_id: String,
    nonce_b64: String,
    ciphertext_b64: String,
}

fn encode_envelope(
    key: &[u8; 32],
    identity: &AdmittedPartition,
    working: &Path,
) -> Result<Vec<u8>, MemoryError> {
    let sqlite = fs::read(working).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let payload = serde_json::to_vec(&(identity, sqlite))
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let (ciphertext_b64, nonce_b64) =
        vault_kit::encrypt(key, &payload).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let envelope = SealedEnvelope {
        key_version: identity.key_version.clone(),
        partition_id: identity.partition_id.clone(),
        nonce_b64,
        ciphertext_b64,
    };
    let json = serde_json::to_vec(&envelope).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let mut out = Vec::with_capacity(SEALED_MAGIC.len() + json.len());
    out.extend_from_slice(SEALED_MAGIC);
    out.extend_from_slice(&json);
    Ok(out)
}

fn unseal_file(
    path: &Path,
    key: &[u8; 32],
    expected: &AdmittedPartition,
) -> Result<Vec<u8>, MemoryError> {
    let bytes = fs::read(path).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    if !bytes.starts_with(SEALED_MAGIC) {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    let envelope: SealedEnvelope = serde_json::from_slice(&bytes[SEALED_MAGIC.len()..])
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    if envelope.key_version != expected.key_version
        || envelope.partition_id != expected.partition_id
    {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    let payload = vault_kit::decrypt(key, &envelope.ciphertext_b64, &envelope.nonce_b64)
        .map_err(|_| MemoryError::PrivatePartitionRefused)?;
    let (identity, sqlite): (AdmittedPartition, Vec<u8>) =
        serde_json::from_slice(&payload).map_err(|_| MemoryError::PrivatePartitionRefused)?;
    if identity != *expected {
        return Err(MemoryError::PrivatePartitionRefused);
    }
    Ok(sqlite)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::InsertMemoryResult;

    fn ctx(
        subject: &str,
        receipt: &str,
        caps: &[PartitionCapability],
        revoked: bool,
    ) -> PrivatePartitionOpenContext {
        PrivatePartitionOpenContext {
            trust_domain_id: TrustDomainId::new("td-alpha").unwrap(),
            subject_id: SubjectId::new(subject).unwrap(),
            receipt: CapabilityReceipt::new(receipt).unwrap(),
            capabilities: caps.iter().copied().collect(),
            key_version: "kv1".to_string(),
            revoked,
        }
    }

    fn keys() -> StaticKeyProvider {
        StaticKeyProvider::new([7u8; 32], "rcpt-ok")
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
    fn two_subjects_cannot_read_each_others_content_or_existence() {
        let root = tempfile::tempdir().unwrap();
        let alice = ctx(
            "subject-alice",
            "rcpt-ok",
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let bob = ctx(
            "subject-bob",
            "rcpt-ok",
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
            "rcpt-ok",
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
            "rcpt-ok",
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let bob = ctx(
            "subject-bob",
            "rcpt-ok",
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
            "rcpt-ok",
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        PrivatePartition::open(root.path(), &live, &keys)
            .unwrap()
            .persist()
            .unwrap();
        let revoked = ctx(
            "subject-alice",
            "rcpt-ok",
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
            "rcpt-ok",
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
            "rcpt-ok",
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let bob_ctx = ctx(
            "subject-bob",
            "rcpt-ok",
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
        assert_eq!(alice.identity().partition_id, alice_ctx.partition_id());
        assert_ne!(alice.identity().partition_id, bob.identity().partition_id);
    }

    #[test]
    fn portable_profile_is_stamped_not_tachi_full() {
        let ctx = ctx(
            "subject-alice",
            "rcpt-ok",
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
        assert_eq!(part.identity().key_version, "kv1");
    }
}

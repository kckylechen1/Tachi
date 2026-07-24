//! Durable, privacy-safe progress ledger for the 400-call pilot.
//!
//! The ledger stores only call keys, engine receipts, output digests, and
//! categorical failure state. It never stores source text or model output.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tachi_params::LessonEngineReceiptV1;

use super::pilot::{PilotRowV1, PilotSourceRouteV1};
use super::source::{DEFAULT_ANTIGRAVITY_SOURCE_DB, DEFAULT_HAPI_PROJECT_DB};

const PROGRESS_FORMAT_V2: &str = "lesson_forge_pilot_progress_v2";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotEngineUsageV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// `Some` means the provider explicitly reported the actual cost. Zero
    /// is therefore distinguishable from an unknown/unreported `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

/// Pilot-only receipt wrapper. Keeping usage outside the shipped
/// `LessonEngineReceiptV1` preserves downstream Rust struct-literal
/// compatibility while the pilot report still captures all required fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotEngineReceiptV1 {
    pub identity: LessonEngineReceiptV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<PilotEngineUsageV1>,
}

impl PilotEngineReceiptV1 {
    pub fn has_known_identity(&self) -> bool {
        self.identity.has_known_identity()
    }

    pub fn has_complete_accounting(&self) -> bool {
        self.usage.as_ref().is_some_and(|usage| {
            usage.tokens.is_some_and(|tokens| tokens > 0)
                && usage.cost_usd_micros.is_some()
                && usage.latency_ms.is_some_and(|latency_ms| latency_ms > 0)
        })
    }

    pub fn is_fully_attested(&self) -> bool {
        self.has_known_identity() && self.has_complete_accounting()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotCallRoleV1 {
    Producer,
    ColdRun,
    Adjudicator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotCallArmV1 {
    None,
    Treated,
    Baseline,
    Blinded,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PilotCallKeyV1 {
    pub contract_digest: String,
    pub source_route: PilotSourceRouteV1,
    pub source_id: String,
    pub source_revision: i64,
    pub role: PilotCallRoleV1,
    pub arm: PilotCallArmV1,
    pub ordinal: u8,
}

impl PilotCallKeyV1 {
    pub fn new(
        contract_digest: &str,
        binding: &PilotRowV1,
        role: PilotCallRoleV1,
        arm: PilotCallArmV1,
        ordinal: u8,
    ) -> Self {
        Self {
            contract_digest: contract_digest.to_string(),
            source_route: binding.source_route,
            source_id: binding.source_id.clone(),
            source_revision: binding.source_revision,
            role,
            arm,
            ordinal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PilotCallStateV1 {
    Started,
    Completed {
        receipt: PilotEngineReceiptV1,
        output_sha256: String,
    },
    Failed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt: Option<PilotEngineReceiptV1>,
        failure_code: String,
        retry_safe: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotCallRecordV1 {
    pub key: PilotCallKeyV1,
    pub state: PilotCallStateV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PilotBlindingKeyV1 {
    contract_digest: String,
    source_route: PilotSourceRouteV1,
    source_id: String,
    source_revision: i64,
}

impl PilotBlindingKeyV1 {
    fn new(contract_digest: &str, binding: &PilotRowV1) -> Self {
        Self {
            contract_digest: contract_digest.to_string(),
            source_route: binding.source_route,
            source_id: binding.source_id.clone(),
            source_revision: binding.source_revision,
        }
    }

    fn matches_call(&self, call: &PilotCallKeyV1) -> bool {
        self.contract_digest == call.contract_digest
            && self.source_route == call.source_route
            && self.source_id == call.source_id
            && self.source_revision == call.source_revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PilotBlindingRecordV1 {
    key: PilotBlindingKeyV1,
    material_hex: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedProgressV1 {
    format: String,
    contract_digest: String,
    #[serde(default)]
    blindings: Vec<PilotBlindingRecordV1>,
    records: Vec<PilotCallRecordV1>,
}

#[derive(Debug)]
pub enum PilotProgressErrorV1 {
    Read(std::io::Error),
    Write(std::io::Error),
    Parse(serde_json::Error),
    Serialize(serde_json::Error),
    InvalidFormat,
    ContractMismatch,
    DuplicateCallKey,
    DuplicateBlindingKey,
    InvalidBlindingMaterial,
    MissingBlindingAfterSpend,
    SourceDatabasePath,
    InsecurePermissions,
}

impl std::fmt::Display for PilotProgressErrorV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(error) => write!(f, "could not read pilot progress ledger: {error}"),
            Self::Write(error) => write!(f, "could not write pilot progress ledger: {error}"),
            Self::Parse(error) => write!(f, "could not parse pilot progress ledger: {error}"),
            Self::Serialize(error) => {
                write!(f, "could not serialize pilot progress ledger: {error}")
            }
            Self::InvalidFormat => write!(f, "unsupported pilot progress ledger format"),
            Self::ContractMismatch => {
                write!(
                    f,
                    "pilot progress ledger does not match the durable manifest"
                )
            }
            Self::DuplicateCallKey => write!(f, "pilot progress ledger has a duplicate call key"),
            Self::DuplicateBlindingKey => {
                write!(f, "pilot progress ledger has a duplicate blinding key")
            }
            Self::InvalidBlindingMaterial => {
                write!(f, "pilot progress ledger has invalid private blinding material")
            }
            Self::MissingBlindingAfterSpend => write!(
                f,
                "pilot progress ledger is missing private blinding material after a call was recorded"
            ),
            Self::SourceDatabasePath => {
                write!(
                    f,
                    "pilot progress ledger path must not be a source database"
                )
            }
            Self::InsecurePermissions => {
                write!(f, "pilot progress ledger must be private to its owner")
            }
        }
    }
}

impl std::error::Error for PilotProgressErrorV1 {}

pub struct PilotProgressLedgerV1 {
    path: PathBuf,
    contract_digest: String,
    blindings: Vec<PilotBlindingRecordV1>,
    records: Vec<PilotCallRecordV1>,
}

impl PilotProgressLedgerV1 {
    pub fn open(
        path: impl AsRef<Path>,
        contract_digest: &str,
    ) -> Result<Self, PilotProgressErrorV1> {
        let path = path.as_ref().to_path_buf();
        if is_source_database_path(&path) {
            return Err(PilotProgressErrorV1::SourceDatabasePath);
        }
        if path.exists() {
            require_private_recovery_state(&path)?;
            let bytes = std::fs::read(&path).map_err(PilotProgressErrorV1::Read)?;
            let persisted: PersistedProgressV1 =
                serde_json::from_slice(&bytes).map_err(PilotProgressErrorV1::Parse)?;
            if persisted.format != PROGRESS_FORMAT_V2 {
                return Err(PilotProgressErrorV1::InvalidFormat);
            }
            if persisted.contract_digest != contract_digest
                || persisted
                    .records
                    .iter()
                    .any(|record| record.key.contract_digest != contract_digest)
                || persisted
                    .blindings
                    .iter()
                    .any(|record| record.key.contract_digest != contract_digest)
            {
                return Err(PilotProgressErrorV1::ContractMismatch);
            }
            for (index, record) in persisted.records.iter().enumerate() {
                if persisted.records[..index]
                    .iter()
                    .any(|existing| existing.key == record.key)
                {
                    return Err(PilotProgressErrorV1::DuplicateCallKey);
                }
            }
            for (index, record) in persisted.blindings.iter().enumerate() {
                if persisted.blindings[..index]
                    .iter()
                    .any(|existing| existing.key == record.key)
                {
                    return Err(PilotProgressErrorV1::DuplicateBlindingKey);
                }
                if decode_blinding_material(&record.material_hex).is_none() {
                    return Err(PilotProgressErrorV1::InvalidBlindingMaterial);
                }
            }
            Ok(Self {
                path,
                contract_digest: contract_digest.to_string(),
                blindings: persisted.blindings,
                records: persisted.records,
            })
        } else {
            let ledger = Self {
                path,
                contract_digest: contract_digest.to_string(),
                blindings: Vec::new(),
                records: Vec::new(),
            };
            ledger.persist()?;
            Ok(ledger)
        }
    }

    pub fn get(&self, key: &PilotCallKeyV1) -> Option<&PilotCallStateV1> {
        self.records
            .iter()
            .find(|record| &record.key == key)
            .map(|record| &record.state)
    }

    pub fn record(
        &mut self,
        key: PilotCallKeyV1,
        state: PilotCallStateV1,
    ) -> Result<(), PilotProgressErrorV1> {
        if let Some(existing) = self.records.iter_mut().find(|record| record.key == key) {
            existing.state = state;
        } else {
            self.records.push(PilotCallRecordV1 { key, state });
        }
        self.persist()
    }

    /// Freeze an unpredictable seed before any call for this case. The raw
    /// OS-random material remains private to this restart ledger; the seed is
    /// deterministically re-derived from that material plus the complete
    /// contract/case binding on restart.
    pub(crate) fn case_blind_seed(
        &mut self,
        binding: &PilotRowV1,
    ) -> Result<[u8; 32], PilotProgressErrorV1> {
        let key = PilotBlindingKeyV1::new(&self.contract_digest, binding);
        if let Some(existing) = self.blindings.iter().find(|record| record.key == key) {
            let material = decode_blinding_material(&existing.material_hex)
                .ok_or(PilotProgressErrorV1::InvalidBlindingMaterial)?;
            return Ok(derive_blind_seed(&key, &material));
        }
        if self
            .records
            .iter()
            .any(|record| key.matches_call(&record.key))
        {
            return Err(PilotProgressErrorV1::MissingBlindingAfterSpend);
        }

        let mut material = [0u8; 32];
        OsRng.fill_bytes(&mut material);
        let seed = derive_blind_seed(&key, &material);
        self.blindings.push(PilotBlindingRecordV1 {
            key,
            material_hex: encode_blinding_material(&material),
        });
        self.persist()?;
        Ok(seed)
    }

    fn persist(&self) -> Result<(), PilotProgressErrorV1> {
        if is_source_database_path(&self.path) {
            return Err(PilotProgressErrorV1::SourceDatabasePath);
        }
        if self.path.exists() {
            require_private_recovery_state(&self.path)?;
        }
        let persisted = PersistedProgressV1 {
            format: PROGRESS_FORMAT_V2.to_string(),
            contract_digest: self.contract_digest.clone(),
            blindings: self.blindings.clone(),
            records: self.records.clone(),
        };
        let mut bytes =
            serde_json::to_vec_pretty(&persisted).map_err(PilotProgressErrorV1::Serialize)?;
        bytes.push(b'\n');
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("pilot-progress.json");
        let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(PilotProgressErrorV1::Write)?;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            let _ = std::fs::remove_file(&temporary);
            return Err(PilotProgressErrorV1::Write(error));
        }
        if let Err(error) = std::fs::rename(&temporary, &self.path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(PilotProgressErrorV1::Write(error));
        }
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(PilotProgressErrorV1::Write)
    }
}

fn encode_blinding_material(material: &[u8; 32]) -> String {
    material.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_blinding_material(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut material = [0u8; 32];
    for (index, byte) in material.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(material)
}

fn derive_blind_seed(key: &PilotBlindingKeyV1, material: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for field in [
        "lesson_forge_pilot_blinding_v1",
        key.contract_digest.as_str(),
        key.source_route.as_str(),
        key.source_id.as_str(),
    ] {
        hasher.update(field.len().to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hasher.update(key.source_revision.to_be_bytes());
    hasher.update(material);
    hasher.finalize().into()
}

fn is_source_database_path(path: &Path) -> bool {
    source_database_paths()
        .iter()
        .any(|source_path| paths_alias(path, source_path))
}

fn source_database_paths() -> Vec<PathBuf> {
    [
        PathBuf::from(DEFAULT_ANTIGRAVITY_SOURCE_DB),
        PathBuf::from(DEFAULT_HAPI_PROJECT_DB),
    ]
    .into_iter()
    .chain(
        ["ANTIGRAVITY_SOURCE_DB", "HAPI_PROJECT_DB"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from),
    )
    .collect()
}

fn paths_alias(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    if std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .is_some_and(|(left, right)| left == right)
    {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        return std::fs::metadata(left)
            .ok()
            .zip(std::fs::metadata(right).ok())
            .is_some_and(|(left, right)| left.dev() == right.dev() && left.ino() == right.ino());
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(unix)]
fn require_private_recovery_state(path: &Path) -> Result<(), PilotProgressErrorV1> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .map_err(PilotProgressErrorV1::Read)?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(PilotProgressErrorV1::InsecurePermissions)
    }
}

#[cfg(not(unix))]
fn require_private_recovery_state(_path: &Path) -> Result<(), PilotProgressErrorV1> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_ledger_refuses_default_source_database_paths_without_opening_them() {
        for path in [DEFAULT_ANTIGRAVITY_SOURCE_DB, DEFAULT_HAPI_PROJECT_DB] {
            assert!(matches!(
                PilotProgressLedgerV1::open(path, &"0".repeat(64)),
                Err(PilotProgressErrorV1::SourceDatabasePath)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn progress_ledger_refuses_source_database_path_aliases_without_opening_them() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let source = directory.path().join("source.db");
        std::fs::write(&source, []).expect("source placeholder");
        let alias = directory.path().join("source-database-alias");
        symlink(&source, &alias).expect("source alias");
        let hard_link = directory.path().join("source-database-hard-link");
        std::fs::hard_link(&source, &hard_link).expect("source hard link");
        let prior = std::env::var_os("ANTIGRAVITY_SOURCE_DB");
        std::env::set_var("ANTIGRAVITY_SOURCE_DB", &source);

        let results = [&alias, &hard_link]
            .into_iter()
            .map(|path| PilotProgressLedgerV1::open(path, &"0".repeat(64)))
            .collect::<Vec<_>>();

        match prior {
            Some(value) => std::env::set_var("ANTIGRAVITY_SOURCE_DB", value),
            None => std::env::remove_var("ANTIGRAVITY_SOURCE_DB"),
        }
        assert!(results
            .iter()
            .all(|result| matches!(result, Err(PilotProgressErrorV1::SourceDatabasePath))));
    }

    #[cfg(unix)]
    #[test]
    fn progress_ledger_creates_private_recovery_state() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("pilot-progress.json");
        let _ledger =
            PilotProgressLedgerV1::open(&path, &"0".repeat(64)).expect("new progress ledger");
        let mode = std::fs::metadata(&path)
            .expect("progress metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn progress_ledger_refuses_existing_nonprivate_recovery_state() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("pilot-progress.json");
        let mut ledger =
            PilotProgressLedgerV1::open(&path, &"0".repeat(64)).expect("new progress ledger");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("make state nonprivate");

        let key = PilotCallKeyV1 {
            contract_digest: "0".repeat(64),
            source_route: PilotSourceRouteV1::Antigravity,
            source_id: "row-1".to_string(),
            source_revision: 1,
            role: PilotCallRoleV1::Producer,
            arm: PilotCallArmV1::None,
            ordinal: 0,
        };
        assert!(matches!(
            ledger.record(key, PilotCallStateV1::Started),
            Err(PilotProgressErrorV1::InsecurePermissions)
        ));
        assert!(matches!(
            PilotProgressLedgerV1::open(&path, &"0".repeat(64)),
            Err(PilotProgressErrorV1::InsecurePermissions)
        ));
    }
}

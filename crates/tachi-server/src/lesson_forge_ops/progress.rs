//! Durable, privacy-safe progress ledger for the 400-call pilot.
//!
//! The ledger stores only call keys, engine receipts, output digests, and
//! categorical failure state. It never stores source text or model output.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tachi_params::LessonEngineReceiptV1;

use super::pilot::{PilotRowV1, PilotSourceRouteV1};
use super::source::{DEFAULT_ANTIGRAVITY_SOURCE_DB, DEFAULT_HAPI_PROJECT_DB};

const PROGRESS_FORMAT_V1: &str = "lesson_forge_pilot_progress_v1";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotEngineUsageV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotCallRoleV1 {
    Producer,
    ColdRun,
    Adjudicator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotCallArmV1 {
    None,
    Treated,
    Baseline,
    Blinded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
struct PersistedProgressV1 {
    format: String,
    contract_digest: String,
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
    SourceDatabasePath,
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
            Self::SourceDatabasePath => {
                write!(
                    f,
                    "pilot progress ledger path must not be a source database"
                )
            }
        }
    }
}

impl std::error::Error for PilotProgressErrorV1 {}

pub struct PilotProgressLedgerV1 {
    path: PathBuf,
    contract_digest: String,
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
            let bytes = std::fs::read(&path).map_err(PilotProgressErrorV1::Read)?;
            let persisted: PersistedProgressV1 =
                serde_json::from_slice(&bytes).map_err(PilotProgressErrorV1::Parse)?;
            if persisted.format != PROGRESS_FORMAT_V1 {
                return Err(PilotProgressErrorV1::InvalidFormat);
            }
            if persisted.contract_digest != contract_digest
                || persisted
                    .records
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
            Ok(Self {
                path,
                contract_digest: contract_digest.to_string(),
                records: persisted.records,
            })
        } else {
            let ledger = Self {
                path,
                contract_digest: contract_digest.to_string(),
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

    fn persist(&self) -> Result<(), PilotProgressErrorV1> {
        let persisted = PersistedProgressV1 {
            format: PROGRESS_FORMAT_V1.to_string(),
            contract_digest: self.contract_digest.clone(),
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
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
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

fn is_source_database_path(path: &Path) -> bool {
    if path == Path::new(DEFAULT_ANTIGRAVITY_SOURCE_DB)
        || path == Path::new(DEFAULT_HAPI_PROJECT_DB)
    {
        return true;
    }
    ["ANTIGRAVITY_SOURCE_DB", "HAPI_PROJECT_DB"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .any(|source_path| source_path == path)
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
}

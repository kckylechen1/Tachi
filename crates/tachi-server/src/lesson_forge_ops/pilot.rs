//! Durable, privacy-safe 50-row pilot manifest for #1073.
//!
//! The manifest is the spend gate.  It stores public-safe bindings only:
//! source route, source identity/revision, a complete-content SHA-256, capture
//! time, classification, and predeclared evaluation decisions.  It never
//! stores source text or model output.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tachi_params::LessonCandidateKindV1;

pub const PILOT_SIZE: usize = 50;
pub const PILOT_MANIFEST_FORMAT_V1: &str = "lesson_forge_pilot_manifest_v1";

/// The two source stores explicitly authorised for this pilot.  The route is
/// part of the source identity: equal ids in different stores never match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotSourceRouteV1 {
    Antigravity,
    Hapi,
}

impl PilotSourceRouteV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Antigravity => "antigravity",
            Self::Hapi => "hapi",
        }
    }
}

/// Narrative cases compete with structured controls in an exact 25/25 split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotRowKindV1 {
    Narrative,
    StructuredControl,
}

/// The three frozen exact strata (17 correction/alignment, 17
/// verification/recovery, and 16 routing/store/provenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotStratumV1 {
    CorrectionAlignment,
    VerificationRecovery,
    RoutingStoreProvenance,
}

impl PilotStratumV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CorrectionAlignment => "correction_alignment",
            Self::VerificationRecovery => "verification_recovery",
            Self::RoutingStoreProvenance => "routing_store_provenance",
        }
    }
}

/// One immutable, public-safe source binding selected before model spend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotRowV1 {
    pub source_route: PilotSourceRouteV1,
    pub source_id: String,
    pub source_revision: i64,
    /// Lowercase or uppercase hexadecimal SHA-256 of full UTF-8 source text.
    pub content_sha256: String,
    /// RFC3339 capture time recorded as part of the freeze record.
    pub capture_timestamp: String,
    pub kind: PilotRowKindV1,
    pub stratum: PilotStratumV1,
    /// Public-safe reason for sampling this row, never a source-text excerpt.
    pub selection_reason: String,
    /// Public-safe decision/ruling declared before the run.
    pub reference_decision: String,
    pub target_kind: LessonCandidateKindV1,
}

impl PilotRowV1 {
    pub fn binding_key(&self) -> (PilotSourceRouteV1, &str, i64) {
        (self.source_route, &self.source_id, self.source_revision)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PilotFreezeError {
    WrongRowCount {
        expected: usize,
        actual: usize,
    },
    DuplicateRow {
        source_route: PilotSourceRouteV1,
        source_id: String,
        revision: i64,
    },
    MissingSourceId {
        source_route: PilotSourceRouteV1,
    },
    InvalidSourceRevision {
        source_id: String,
        revision: i64,
    },
    InvalidContentDigest {
        source_id: String,
    },
    MissingCaptureTimestamp {
        source_id: String,
    },
    MissingSelectionReason {
        source_id: String,
    },
    MissingReferenceDecision {
        source_id: String,
    },
    WrongSourceCount {
        source_route: PilotSourceRouteV1,
        expected: usize,
        actual: usize,
    },
    WrongKindCount {
        kind: PilotRowKindV1,
        expected: usize,
        actual: usize,
    },
    WrongStratumCount {
        stratum: PilotStratumV1,
        expected: usize,
        actual: usize,
    },
}

impl std::fmt::Display for PilotFreezeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongRowCount { expected, actual } => {
                write!(
                    f,
                    "frozen pilot has {actual} rows; expected exactly {expected}"
                )
            }
            Self::DuplicateRow {
                source_route,
                source_id,
                revision,
            } => write!(
                f,
                "duplicate frozen pilot row {}:{source_id}@{revision}",
                source_route.as_str()
            ),
            Self::MissingSourceId { source_route } => write!(
                f,
                "frozen pilot row for {} has an empty stable id",
                source_route.as_str()
            ),
            Self::InvalidSourceRevision {
                source_id,
                revision,
            } => {
                write!(
                    f,
                    "frozen pilot row {source_id} has invalid revision {revision}"
                )
            }
            Self::InvalidContentDigest { source_id } => {
                write!(
                    f,
                    "frozen pilot row {source_id} lacks a SHA-256 content digest"
                )
            }
            Self::MissingCaptureTimestamp { source_id } => {
                write!(f, "frozen pilot row {source_id} lacks a capture timestamp")
            }
            Self::MissingSelectionReason { source_id } => {
                write!(f, "frozen pilot row {source_id} has no selection reason")
            }
            Self::MissingReferenceDecision { source_id } => {
                write!(f, "frozen pilot row {source_id} has no reference decision")
            }
            Self::WrongSourceCount {
                source_route,
                expected,
                actual,
            } => write!(
                f,
                "frozen pilot has {actual} {} rows; expected {expected}",
                source_route.as_str()
            ),
            Self::WrongKindCount {
                kind,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "frozen pilot has {actual} {kind:?} rows; expected {expected}"
                )
            }
            Self::WrongStratumCount {
                stratum,
                expected,
                actual,
            } => write!(
                f,
                "frozen pilot has {actual} {} rows; expected {expected}",
                stratum.as_str()
            ),
        }
    }
}

/// A validated, canonicalized manifest.  It is constructible only through
/// `freeze_pilot_manifest` or `load_from_path`, both of which enforce the
/// exact source/kind/stratum matrix and complete binding fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotManifestV1 {
    rows: Vec<PilotRowV1>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedPilotManifestV1 {
    format: String,
    contract_digest: String,
    rows: Vec<PilotRowV1>,
}

#[derive(Debug, Serialize)]
struct CanonicalPilotManifestV1<'a> {
    format: &'static str,
    rows: &'a [PilotRowV1],
}

#[derive(Debug)]
pub enum PilotManifestIoError {
    Read(std::io::Error),
    Write(std::io::Error),
    Parse(serde_json::Error),
    Serialize(serde_json::Error),
    InvalidFormat(String),
    Validation(Vec<PilotFreezeError>),
    DigestMismatch { expected: String, actual: String },
}

impl std::fmt::Display for PilotManifestIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(err) => write!(f, "could not read pilot manifest: {err}"),
            Self::Write(err) => write!(f, "could not write pilot manifest: {err}"),
            Self::Parse(err) => write!(f, "could not parse pilot manifest: {err}"),
            Self::Serialize(err) => write!(f, "could not serialize pilot manifest: {err}"),
            Self::InvalidFormat(format) => write!(f, "unsupported pilot manifest format {format}"),
            Self::Validation(errors) => write!(
                f,
                "invalid pilot manifest: {} validation error(s)",
                errors.len()
            ),
            Self::DigestMismatch { expected, actual } => write!(
                f,
                "pilot manifest digest mismatch: expected {expected}, recomputed {actual}"
            ),
        }
    }
}

impl std::error::Error for PilotManifestIoError {}

impl PilotManifestV1 {
    pub fn rows(&self) -> &[PilotRowV1] {
        &self.rows
    }

    pub fn contains(
        &self,
        source_route: PilotSourceRouteV1,
        source_id: &str,
        source_revision: i64,
    ) -> bool {
        self.find_binding(source_route, source_id, source_revision)
            .is_some()
    }

    pub fn find_binding(
        &self,
        source_route: PilotSourceRouteV1,
        source_id: &str,
        source_revision: i64,
    ) -> Option<&PilotRowV1> {
        self.rows.iter().find(|row| {
            row.source_route == source_route
                && row.source_id == source_id
                && row.source_revision == source_revision
        })
    }

    /// Stable bytes used for the contract digest.  Rows are canonicalized at
    /// freeze time, so equivalent input orderings have identical bytes.
    pub fn canonical_json(&self) -> Result<String, PilotManifestIoError> {
        serde_json::to_string(&CanonicalPilotManifestV1 {
            format: PILOT_MANIFEST_FORMAT_V1,
            rows: &self.rows,
        })
        .map_err(PilotManifestIoError::Serialize)
    }

    pub fn contract_digest(&self) -> Result<String, PilotManifestIoError> {
        let canonical = self.canonical_json()?;
        Ok(format!("{:x}", Sha256::digest(canonical.as_bytes())))
    }

    /// Save a public-safe canonical manifest.  No source text and no model
    /// text flow through this serializer.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), PilotManifestIoError> {
        let persisted = PersistedPilotManifestV1 {
            format: PILOT_MANIFEST_FORMAT_V1.to_string(),
            contract_digest: self.contract_digest()?,
            rows: self.rows.clone(),
        };
        let mut bytes =
            serde_json::to_vec_pretty(&persisted).map_err(PilotManifestIoError::Serialize)?;
        bytes.push(b'\n');
        std::fs::write(path, bytes).map_err(PilotManifestIoError::Write)
    }

    /// Load a manifest only after revalidating its rows and checking its saved
    /// digest.  A hand-edited binding therefore fails before it can buy a
    /// producer call.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, PilotManifestIoError> {
        let bytes = std::fs::read(path).map_err(PilotManifestIoError::Read)?;
        let persisted: PersistedPilotManifestV1 =
            serde_json::from_slice(&bytes).map_err(PilotManifestIoError::Parse)?;
        if persisted.format != PILOT_MANIFEST_FORMAT_V1 {
            return Err(PilotManifestIoError::InvalidFormat(persisted.format));
        }
        let manifest =
            freeze_pilot_manifest(persisted.rows).map_err(PilotManifestIoError::Validation)?;
        let actual = manifest.contract_digest()?;
        if persisted.contract_digest != actual {
            return Err(PilotManifestIoError::DigestMismatch {
                expected: persisted.contract_digest,
                actual,
            });
        }
        Ok(manifest)
    }
}

/// Validate and canonicalize the exact pilot contract, reporting every
/// mismatch rather than silently accepting an approximate sample.
pub fn freeze_pilot_manifest(
    mut rows: Vec<PilotRowV1>,
) -> Result<PilotManifestV1, Vec<PilotFreezeError>> {
    let mut errors = Vec::new();
    if rows.len() != PILOT_SIZE {
        errors.push(PilotFreezeError::WrongRowCount {
            expected: PILOT_SIZE,
            actual: rows.len(),
        });
    }

    let mut seen = HashSet::new();
    let mut sources = BTreeMap::new();
    let mut kinds = BTreeMap::new();
    let mut strata = BTreeMap::new();
    for row in &rows {
        *sources.entry(row.source_route).or_insert(0usize) += 1;
        *kinds.entry(row.kind).or_insert(0usize) += 1;
        *strata.entry(row.stratum).or_insert(0usize) += 1;
        if !seen.insert((row.source_route, row.source_id.clone(), row.source_revision)) {
            errors.push(PilotFreezeError::DuplicateRow {
                source_route: row.source_route,
                source_id: row.source_id.clone(),
                revision: row.source_revision,
            });
        }
        if row.source_id.trim().is_empty() {
            errors.push(PilotFreezeError::MissingSourceId {
                source_route: row.source_route,
            });
        }
        if row.source_revision <= 0 {
            errors.push(PilotFreezeError::InvalidSourceRevision {
                source_id: row.source_id.clone(),
                revision: row.source_revision,
            });
        }
        if !is_sha256(&row.content_sha256) {
            errors.push(PilotFreezeError::InvalidContentDigest {
                source_id: row.source_id.clone(),
            });
        }
        if row.capture_timestamp.trim().is_empty() {
            errors.push(PilotFreezeError::MissingCaptureTimestamp {
                source_id: row.source_id.clone(),
            });
        }
        if row.selection_reason.trim().is_empty() {
            errors.push(PilotFreezeError::MissingSelectionReason {
                source_id: row.source_id.clone(),
            });
        }
        if row.reference_decision.trim().is_empty() {
            errors.push(PilotFreezeError::MissingReferenceDecision {
                source_id: row.source_id.clone(),
            });
        }
    }

    for source_route in [PilotSourceRouteV1::Antigravity, PilotSourceRouteV1::Hapi] {
        exact_count(
            &mut errors,
            sources.get(&source_route).copied().unwrap_or_default(),
            25,
            |actual| PilotFreezeError::WrongSourceCount {
                source_route,
                expected: 25,
                actual,
            },
        );
    }
    for kind in [PilotRowKindV1::Narrative, PilotRowKindV1::StructuredControl] {
        exact_count(
            &mut errors,
            kinds.get(&kind).copied().unwrap_or_default(),
            25,
            |actual| PilotFreezeError::WrongKindCount {
                kind,
                expected: 25,
                actual,
            },
        );
    }
    for (stratum, expected) in [
        (PilotStratumV1::CorrectionAlignment, 17usize),
        (PilotStratumV1::VerificationRecovery, 17usize),
        (PilotStratumV1::RoutingStoreProvenance, 16usize),
    ] {
        exact_count(
            &mut errors,
            strata.get(&stratum).copied().unwrap_or_default(),
            expected,
            |actual| PilotFreezeError::WrongStratumCount {
                stratum,
                expected,
                actual,
            },
        );
    }

    if errors.is_empty() {
        rows.sort_by(|left, right| left.binding_key().cmp(&right.binding_key()));
        Ok(PilotManifestV1 { rows })
    } else {
        Err(errors)
    }
}

fn exact_count(
    errors: &mut Vec<PilotFreezeError>,
    actual: usize,
    expected: usize,
    error: impl FnOnce(usize) -> PilotFreezeError,
) {
    if actual != expected {
        errors.push(error(actual));
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(index: usize) -> PilotRowV1 {
        PilotRowV1 {
            source_route: if index < 25 {
                PilotSourceRouteV1::Antigravity
            } else {
                PilotSourceRouteV1::Hapi
            },
            source_id: format!("synthetic-{index:02}"),
            source_revision: 1,
            content_sha256: format!("{index:064x}"),
            capture_timestamp: "2026-07-24T00:00:00Z".to_string(),
            kind: if index % 2 == 0 {
                PilotRowKindV1::Narrative
            } else {
                PilotRowKindV1::StructuredControl
            },
            stratum: match index {
                0..=16 => PilotStratumV1::CorrectionAlignment,
                17..=33 => PilotStratumV1::VerificationRecovery,
                _ => PilotStratumV1::RoutingStoreProvenance,
            },
            selection_reason: "synthetic public-safe selection rationale".to_string(),
            reference_decision: "synthetic public-safe reference decision".to_string(),
            target_kind: LessonCandidateKindV1::Precedent,
        }
    }

    fn valid_rows() -> Vec<PilotRowV1> {
        (0..PILOT_SIZE).map(row).collect()
    }

    #[test]
    fn exact_fifty_row_source_kind_and_stratum_matrix_freezes() {
        let manifest = freeze_pilot_manifest(valid_rows()).expect("exact matrix freezes");
        assert_eq!(manifest.rows().len(), 50);
        assert_eq!(
            manifest
                .rows()
                .iter()
                .filter(|row| row.source_route == PilotSourceRouteV1::Antigravity)
                .count(),
            25
        );
        assert_eq!(
            manifest
                .rows()
                .iter()
                .filter(|row| row.kind == PilotRowKindV1::Narrative)
                .count(),
            25
        );
        assert!(manifest.contains(PilotSourceRouteV1::Hapi, "synthetic-49", 1));
        assert!(!manifest.contains(PilotSourceRouteV1::Antigravity, "synthetic-49", 1));
    }

    #[test]
    fn wrong_source_kind_and_stratum_allocations_are_red() {
        let mut rows = valid_rows();
        rows[25].source_route = PilotSourceRouteV1::Antigravity;
        rows[1].kind = PilotRowKindV1::Narrative;
        rows[34].stratum = PilotStratumV1::CorrectionAlignment;
        let errors = freeze_pilot_manifest(rows).expect_err("all exact dimensions must gate");
        assert!(errors.iter().any(|error| matches!(
            error,
            PilotFreezeError::WrongSourceCount {
                source_route: PilotSourceRouteV1::Antigravity,
                actual: 26,
                ..
            }
        )));
        assert!(errors.iter().any(|error| matches!(
            error,
            PilotFreezeError::WrongKindCount {
                kind: PilotRowKindV1::Narrative,
                actual: 26,
                ..
            }
        )));
        assert!(errors.iter().any(|error| matches!(
            error,
            PilotFreezeError::WrongStratumCount {
                stratum: PilotStratumV1::CorrectionAlignment,
                actual: 18,
                ..
            }
        )));
    }

    #[test]
    fn duplicate_and_incomplete_bindings_are_red() {
        let mut rows = valid_rows();
        rows[1] = rows[0].clone();
        rows[2].content_sha256.clear();
        rows[3].capture_timestamp.clear();
        rows[4].source_id.clear();
        rows[5].source_revision = 0;
        let errors = freeze_pilot_manifest(rows).expect_err("binding fields are mandatory");
        assert!(errors
            .iter()
            .any(|error| matches!(error, PilotFreezeError::DuplicateRow { .. })));
        assert!(errors
            .iter()
            .any(|error| matches!(error, PilotFreezeError::InvalidContentDigest { .. })));
        assert!(errors
            .iter()
            .any(|error| matches!(error, PilotFreezeError::MissingCaptureTimestamp { .. })));
        assert!(errors
            .iter()
            .any(|error| matches!(error, PilotFreezeError::MissingSourceId { .. })));
        assert!(errors
            .iter()
            .any(|error| matches!(error, PilotFreezeError::InvalidSourceRevision { .. })));
    }

    #[test]
    fn canonical_digest_does_not_depend_on_input_order() {
        let forward = freeze_pilot_manifest(valid_rows()).expect("forward manifest");
        let mut reversed = valid_rows();
        reversed.reverse();
        let reverse = freeze_pilot_manifest(reversed).expect("reverse manifest");
        assert_eq!(
            forward.contract_digest().unwrap(),
            reverse.contract_digest().unwrap()
        );
        assert_eq!(
            forward.canonical_json().unwrap(),
            reverse.canonical_json().unwrap()
        );
    }

    #[test]
    fn load_rejects_a_tampered_public_binding_before_spend() {
        let manifest = freeze_pilot_manifest(valid_rows()).expect("manifest");
        let path =
            std::env::temp_dir().join(format!("sigil-1073-pilot-{}.json", uuid::Uuid::new_v4()));
        manifest.save_to_path(&path).expect("save manifest");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["rows"][0]["source_id"] = serde_json::Value::String("tampered-id".to_string());
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let error =
            PilotManifestV1::load_from_path(&path).expect_err("digest must reject tampering");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(error, PilotManifestIoError::DigestMismatch { .. }));
    }
}

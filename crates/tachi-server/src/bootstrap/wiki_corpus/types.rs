use super::apply::*;
use super::classify::*;
use super::fs::*;

use crate::physical_db_identity::{
    classify_paths, InventoryFailureKind, OpenPathBasis, PhysicalDbStore,
};
use crate::tool_params::EffectiveKnowledgeArtifactV1;
use memcore::db::migrations::read_schema_version;
use memcore::MemoryStore;
use rusqlite::OpenFlags;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

pub(crate) const WIKI_CORPUS_CONFIRMATION_TOKEN: &str = "MIGRATE_WIKI_CORPUS_V1";
/// Separate token from `WIKI_CORPUS_CONFIRMATION_TOKEN` on purpose: a
/// copy-pasted apply command must never be able to trigger a repair write.
pub(crate) const WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN: &str =
    "REPAIR_WIKI_CORPUS_SIBLING_DAMAGE_V1";
pub(crate) const REPORT_VERSION: &str = "wiki_corpus_migration_v1";
pub(crate) const REPAIR_REPORT_VERSION: &str = "wiki_corpus_sibling_repair_v1";
pub(crate) const RECEIPT_KEY: &str = "wiki_corpus_migration";
pub(crate) const RECEIPT_VERSION: u32 = 2;
/// History key holding every sibling-damage repair applied to a row. The
/// migration receipt itself is restored to the canonical `target_copied`
/// terminal state so that every existing completion predicate keeps its exact
/// meaning; the replaced receipt is preserved here instead of being erased.
pub(crate) const REPAIR_RECEIPT_KEY: &str = "wiki_corpus_sibling_repair";
pub(crate) const REPAIR_RECEIPT_VERSION: u32 = 1;
pub(crate) const REPAIR_PHASE: &str = "sibling_damage_repaired";
/// Third token, distinct from both of the above for the same reason they are
/// distinct from each other: a copy-pasted apply or repair command must not be
/// able to bootstrap a new store.
pub(crate) const WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN: &str = "ADOPT_WIKI_LEGACY_V1";
/// Additive, nested provenance key stamped on every adopted row.
///
/// Deliberately **not** `RECEIPT_KEY`: attaching a `wiki_corpus_migration`
/// receipt to a row the classifier does not call a `SharedCandidate` is an
/// explicit error in the reconciler, so reusing that key here would poison a
/// later `--apply`. Equally deliberately, `review_status` lives *inside* this
/// key and never at metadata top level, where a `"pending"` value would demote
/// the row's derived lifecycle.
pub(crate) const WIKI_LEGACY_ADOPTION_MARKER_KEY: &str = "wiki_legacy_adoption_v1";
pub(crate) const WIKI_LEGACY_ADOPTION_REPORT_VERSION: &str = "wiki-legacy-adoption/v1";
pub(crate) static PREVIEW_SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A read-only SQLite handle backed by a private filesystem snapshot.
///
/// SQLite's ordinary read-only WAL open can create the `-shm` file when the
/// source has no shared-memory sidecar yet.  Preview therefore copies the
/// main database and any existing WAL/SHM sidecars into a private temporary
/// directory before opening the copy.  The source is never opened by SQLite;
/// metadata is checked before and after the copy so a concurrent source change
/// fails closed instead of producing an unqualified inventory.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CorpusClassification {
    SharedCandidate,
    ProjectBound,
    OperationalSnapshot,
    TestEphemeral,
    AuthorityRecord,
    ManualReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogicalStore {
    BoundProject,
    SharedWiki,
    LegacyGlobal,
}

impl LogicalStore {
    pub(crate) fn reference(self) -> &'static str {
        match self {
            Self::BoundProject => "bound_project",
            Self::SharedWiki => "named:wiki",
            Self::LegacyGlobal => "legacy_global",
        }
    }

    pub(crate) fn semantic_role(self) -> &'static str {
        match self {
            Self::BoundProject => "current_bound_project_store",
            Self::SharedWiki => "logical_shared_wiki_store",
            Self::LegacyGlobal => "legacy_global_store",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StoreSpec {
    pub(crate) logical_store: LogicalStore,
    pub(crate) addressed_path: Option<PathBuf>,
    pub(crate) resolution_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReadFailure {
    pub(crate) kind: InventoryFailureKind,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ClassificationCounts {
    pub(crate) shared_candidate: usize,
    pub(crate) project_bound: usize,
    pub(crate) operational_snapshot: usize,
    pub(crate) test_ephemeral: usize,
    pub(crate) authority_record: usize,
    pub(crate) manual_review: usize,
}

impl ClassificationCounts {
    pub(crate) fn increment(&mut self, classification: CorpusClassification) {
        match classification {
            CorpusClassification::SharedCandidate => self.shared_candidate += 1,
            CorpusClassification::ProjectBound => self.project_bound += 1,
            CorpusClassification::OperationalSnapshot => self.operational_snapshot += 1,
            CorpusClassification::TestEphemeral => self.test_ephemeral += 1,
            CorpusClassification::AuthorityRecord => self.authority_record += 1,
            CorpusClassification::ManualReview => self.manual_review += 1,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct RowCounts {
    pub(crate) total_memory_rows: usize,
    pub(crate) wiki_related_rows: usize,
    pub(crate) classifications: ClassificationCounts,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TypedMetadataEvidence {
    pub(crate) raw_metadata_sha256: String,
    pub(crate) artifact_kind: Option<Value>,
    pub(crate) knowledge_scope: Option<Value>,
    pub(crate) origin_projects: Option<Value>,
    pub(crate) applies_to: Option<Value>,
    pub(crate) applicability_status: Option<Value>,
    pub(crate) lifecycle: Option<Value>,
    pub(crate) authority: Option<Value>,
    pub(crate) review_receipt: Option<Value>,
    pub(crate) operational_markers: Vec<String>,
    pub(crate) test_markers: Vec<String>,
    pub(crate) authority_markers: Vec<String>,
    pub(crate) migration_receipt: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WikiCorpusRow {
    pub(crate) logical_store_ref: String,
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) normalized_path: String,
    pub(crate) summary: String,
    pub(crate) source: String,
    pub(crate) category: String,
    pub(crate) domain: Option<String>,
    pub(crate) legacy_scope: String,
    pub(crate) revision: i64,
    pub(crate) archived: bool,
    pub(crate) superseded_by: Option<String>,
    pub(crate) content_sha256: String,
    pub(crate) classification: CorpusClassification,
    pub(crate) reasons: Vec<String>,
    pub(crate) effective_typed_metadata: EffectiveKnowledgeArtifactV1,
    pub(crate) typed_metadata_evidence: TypedMetadataEvidence,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StoreReport {
    pub(crate) logical_store_ref: String,
    pub(crate) semantic_role: String,
    pub(crate) addressed_path: Option<String>,
    pub(crate) existence: bool,
    pub(crate) resolved_path: Option<String>,
    pub(crate) canonical_path: Option<String>,
    pub(crate) open_path: Option<String>,
    pub(crate) open_path_basis: Option<OpenPathBasis>,
    pub(crate) physical_identity: Option<PhysicalDbStore>,
    pub(crate) read_failure: Option<ReadFailure>,
    pub(crate) stored_schema: Option<u32>,
    pub(crate) expected_schema: u32,
    pub(crate) counts: RowCounts,
    pub(crate) rows: Vec<WikiCorpusRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PlanRowFingerprint {
    pub(crate) id: String,
    pub(crate) revision: i64,
    pub(crate) content_sha256: String,
    pub(crate) superseded_by: Option<String>,
    pub(crate) vector: VectorFingerprint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PlanStoreFingerprint {
    pub(crate) logical_store_ref: String,
    pub(crate) physical_id: String,
    pub(crate) canonical_path: String,
    pub(crate) schema: u32,
    pub(crate) total_memory_rows: usize,
    pub(crate) row_digest: String,
    pub(crate) vector_table_present: bool,
    pub(crate) rows: Vec<PlanRowFingerprint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct VectorFingerprint {
    pub(crate) table_present: bool,
    pub(crate) row_present: bool,
    pub(crate) dimensions: Option<usize>,
    pub(crate) sha256: Option<String>,
}

impl VectorFingerprint {
    pub(crate) fn absent(table_present: bool) -> Self {
        Self {
            table_present,
            row_present: false,
            dimensions: None,
            sha256: None,
        }
    }

    pub(crate) fn from_vector(table_present: bool, vector: Option<&[f32]>) -> Self {
        let Some(vector) = vector else {
            return Self::absent(table_present);
        };
        let bytes = memcore::db::serialize_f32(vector);
        Self {
            table_present,
            row_present: true,
            dimensions: Some(vector.len()),
            sha256: Some(format!("{:x}", Sha256::digest(&bytes))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PlanItem {
    pub(crate) action: String,
    pub(crate) source_store_ref: String,
    pub(crate) source_physical_id: String,
    pub(crate) source_id: String,
    pub(crate) source_path: String,
    pub(crate) normalized_path: String,
    pub(crate) source_revision: i64,
    pub(crate) source_content_sha256: String,
    pub(crate) source_copy_identity_sha256: String,
    pub(crate) source_valid_until: Option<String>,
    pub(crate) source_superseded_by: Option<String>,
    pub(crate) source_vector: VectorFingerprint,
    pub(crate) replay_identity: String,
    pub(crate) target_store_ref: String,
    pub(crate) target_physical_id: String,
    pub(crate) target_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WikiCorpusPlan {
    pub(crate) version: String,
    pub(crate) plan_id: String,
    pub(crate) store_fingerprints: Vec<PlanStoreFingerprint>,
    pub(crate) items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BackupReceipt {
    pub(crate) logical_store_refs: Vec<String>,
    pub(crate) source_physical_id: String,
    pub(crate) source_canonical_path: String,
    pub(crate) source_open_path: String,
    pub(crate) backup_path: String,
    pub(crate) backup_physical_id: String,
    pub(crate) schema: u32,
    pub(crate) total_memory_rows: usize,
    pub(crate) source_row_digest: String,
    pub(crate) backup_row_digest: String,
    pub(crate) vector_table_present: bool,
    pub(crate) source_identity_verified: bool,
    pub(crate) backup_identity_verified: bool,
    pub(crate) quick_check: String,
    pub(crate) verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BackupManifest {
    pub(crate) version: String,
    pub(crate) status: String,
    pub(crate) plan_id: String,
    pub(crate) backup_directory: String,
    pub(crate) receipts: Vec<BackupReceipt>,
    pub(crate) migration_receipts: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MigrationOutcome {
    pub(crate) source_store_ref: String,
    pub(crate) source_id: String,
    pub(crate) target_store_ref: String,
    pub(crate) target_id: Option<String>,
    pub(crate) action: String,
    pub(crate) outcome: String,
    pub(crate) phases: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MigrationPhase {
    Planned,
    TargetCopied,
    TargetNoncanonical,
    SourceReceipted,
    SourceSuperseded,
    Reclassified,
}

impl MigrationPhase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::TargetCopied => "target_copied",
            Self::TargetNoncanonical => "target_noncanonical",
            Self::SourceReceipted => "source_receipted",
            Self::SourceSuperseded => "source_superseded",
            Self::Reclassified => "reclassified",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationBoundary {
    TargetCopied,
    #[cfg(any(test, feature = "bootstrap-test-api"))]
    SourceSuperseded,
}

/// Deterministic fault-injection points for the apply safety tests.
///
/// These hooks model a local process replacing a protected directory entry at
/// the exact boundary that the production code must defend.  They are kept
/// typed and internal so production callers can only run with `None`.
#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum CorpusRaceHook {
    ReplaceBackupTempWithNormalFile,
    ReplaceManifestTempWithNormalFile,
    ReplaceExistingBackupAfterRetain {
        replacement_path: PathBuf,
    },
    SwapOpenedStorePath {
        store: LogicalStore,
        replacement_path: PathBuf,
    },
    SwapLogicalPathAfterInventory {
        store: LogicalStore,
        replacement_path: PathBuf,
    },
    SwapReclassificationPathAfterReceiptRead {
        source_id: String,
        replacement_path: PathBuf,
    },
    ReplaceRetainedBackupBeforeSourceMutation {
        replacement_path: PathBuf,
    },
    SwapLogicalPathBeforeCompletedReturn {
        store: LogicalStore,
        replacement_path: PathBuf,
    },
    ArchiveTargetBeforeCompletedReturn {
        source_id: String,
    },
    MutateReclassificationAfterPlanValidation {
        source_id: String,
    },
    MutateValidUntilAfterPlanValidation {
        source_id: String,
    },
    MutateCopySourceAfterPrecheck {
        source_id: String,
    },
    MutateSourceAfterReceiptPrepared {
        source_id: String,
    },
    MutateTargetAfterVerification {
        source_id: String,
    },
    ForeignSupersedeSourceBeforeAtomicTransition {
        source_id: String,
    },
    /// A sibling worker applying the *same* deterministic plan item runs it to
    /// completion (canonical target copy plus receipted supersession) while
    /// this worker is mid-item.
    SiblingWorkerCompletesItem {
        source_id: String,
        seam: SiblingCompletionSeam,
    },
}

/// Which existing race seam a [`CorpusRaceHook::SiblingWorkerCompletesItem`]
/// injection fires at. Both seams sit inside `apply_copy_and_supersede`, on
/// either side of the deterministic target inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum SiblingCompletionSeam {
    /// After this worker's source precheck, before it inspects the target.
    AfterSourcePrecheck,
    /// After this worker prepared its final receipt and expected transition
    /// state, before it decides whether the atomic transition still applies.
    AfterReceiptPrepared,
}

#[derive(Debug, Clone)]
pub(crate) struct MigrationInterruption {
    pub(crate) source_id: Option<String>,
    pub(crate) boundary: MigrationBoundary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MigrationReceipt {
    pub(crate) kind: String,
    pub(crate) version: u32,
    pub(crate) plan_id: String,
    pub(crate) phase: MigrationPhase,
    pub(crate) item: PlanItem,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WikiCorpusReport {
    pub(crate) version: String,
    pub(crate) mode: String,
    pub(crate) apply: bool,
    pub(crate) stores: Vec<StoreReport>,
    pub(crate) plan: Option<WikiCorpusPlan>,
    pub(crate) backup_manifest: Option<BackupManifest>,
    pub(crate) migration_outcomes: Vec<MigrationOutcome>,
    pub(crate) warnings: Vec<String>,
    /// Present only in the sibling-damage repair modes, so the JSON shape of
    /// preview and apply is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sibling_repair: Option<SiblingRepairReport>,
    /// Present only in the legacy-adoption modes, so the JSON shape of
    /// preview, apply, and sibling repair is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) legacy_adoption: Option<LegacyAdoptionReport>,
}

impl WikiCorpusReport {
    /// True when a sibling-repair run recorded one or more failures.
    pub(crate) fn sibling_repair_had_failures(&self) -> bool {
        self.sibling_repair
            .as_ref()
            .is_some_and(|report| report.had_failures)
    }

    /// True when a legacy-adoption run reached a failure it recorded in the
    /// receipt instead of throwing the receipt away with an `Err`.
    ///
    /// The CLI layer uses this to fail the process **after** printing the
    /// report: an adoption that created a store and then could not finish must
    /// not exit 0, and it must not lose the only evidence of what it did.
    pub(crate) fn legacy_adoption_had_failures(&self) -> bool {
        self.legacy_adoption
            .as_ref()
            .is_some_and(|report| report.had_failures)
    }
}

/// One inspected row that could carry the sibling-race damage signature.
///
/// `outcome` is `repairable` in the dry run, and `repaired`/`skipped`/`failed`
/// in the confirmed run: `failed` is a row whose signature matched (it was
/// `repairable`) but whose compensation transaction did not land, with
/// `failure_reason` carrying why. `skipped_reasons` is empty exactly when the
/// full signature matched; every unmatched clause is reported verbatim so an
/// operator can see why a damaged-looking row was deliberately left alone.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SiblingRepairRow {
    pub(crate) store_ref: String,
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) outcome: String,
    pub(crate) observed_receipt_phase: Option<String>,
    pub(crate) observed_archived: bool,
    pub(crate) observed_superseded_by: Option<String>,
    pub(crate) observed_copy_identity_sha256: String,
    pub(crate) expected_receipt_phase: String,
    pub(crate) expected_archived: bool,
    pub(crate) expected_copy_identity_sha256: Option<String>,
    pub(crate) plan_id: Option<String>,
    pub(crate) source_store_ref: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) skipped_reasons: Vec<String>,
    pub(crate) failure_reason: Option<String>,
}

/// A confirmed run never discards a partially completed report: each row's
/// repair is its own atomic, idempotent transaction (see
/// `repair_sibling_damaged_target`), so a row that fails to compensate is
/// recorded as `failed` in `rows`/`errors` instead of unwinding the rows
/// already repaired. `had_failures` is the caller-facing summary bit; the CLI
/// prints the complete report, then converts this bit into a non-zero exit so
/// automation cannot accept a partial repair as success.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SiblingRepairReport {
    pub(crate) version: String,
    pub(crate) confirmed: bool,
    pub(crate) backup_directory: Option<String>,
    pub(crate) inspected_rows: usize,
    pub(crate) repairable: usize,
    pub(crate) repaired: usize,
    pub(crate) skipped: usize,
    pub(crate) failed: usize,
    pub(crate) had_failures: bool,
    pub(crate) backups: Vec<BackupReceipt>,
    pub(crate) rows: Vec<SiblingRepairRow>,
    pub(crate) errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawRow {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) summary: String,
    pub(crate) text: String,
    pub(crate) importance: f64,
    pub(crate) timestamp: String,
    pub(crate) valid_from: String,
    pub(crate) valid_until: Option<String>,
    pub(crate) category: String,
    pub(crate) topic: String,
    pub(crate) keywords: Vec<String>,
    pub(crate) entities: Vec<String>,
    pub(crate) source: String,
    pub(crate) scope: String,
    pub(crate) archived: bool,
    pub(crate) revision: i64,
    // Round-2 bug C: usage counters, loaded verbatim so adoption can carry
    // them through. `import_snapshot_batch` writes these four fields from
    // `MemoryEntry` (memcore's `snapshot_import.rs`), but they are outside
    // the lifecycle checksum's coverage (archived/created_at/id/revision/
    // superseded_by/updated_at/valid_until, per #1607's scope) -- same
    // category as `path`, which gets its own `verify_adopted_paths` readback
    // for the same reason. Preservation here is asserted by
    // `adopt_legacy_preserves_usage_counters_verbatim`, not by the checksum;
    // deliberately not widening the checksum's scope to cover them.
    pub(crate) access_count: i64,
    pub(crate) scored_count: i64,
    pub(crate) last_access: Option<String>,
    pub(crate) last_use_at: Option<String>,
    pub(crate) retention_policy: Option<String>,
    pub(crate) domain: Option<String>,
    pub(crate) metadata: Value,
    pub(crate) vector: Option<Vec<f32>>,
    pub(crate) recall_count: i64,
    pub(crate) query_diversity: i64,
    pub(crate) tier: String,
    pub(crate) superseded_by: Option<String>,
    pub(crate) metadata_parse_error: Option<String>,
    pub(crate) classification: Option<CorpusClassification>,
    pub(crate) reasons: Vec<String>,
    pub(crate) effective: Option<EffectiveKnowledgeArtifactV1>,
}

pub(crate) struct StoreScan {
    pub(crate) spec: StoreSpec,
    pub(crate) physical: Option<PhysicalDbStore>,
    pub(crate) rows: Vec<RawRow>,
    pub(crate) report: StoreReport,
    pub(crate) row_digest: String,
    pub(crate) vector_table_present: bool,
}

impl RawRow {
    pub(crate) fn normalized_path(&self) -> String {
        memcore::path_router::normalize_path(&self.path)
    }

    pub(crate) fn content_sha256(&self) -> String {
        let mut metadata = self.metadata.clone();
        if let Value::Object(object) = &mut metadata {
            object.remove(RECEIPT_KEY);
        }
        let content = json!({
            "path": self.path,
            "summary": self.summary,
            "text": self.text,
            "importance": self.importance,
            "timestamp": self.timestamp,
            "valid_from": self.valid_from,
            "valid_until": self.valid_until,
            "category": self.category,
            "topic": self.topic,
            "keywords": self.keywords,
            "entities": self.entities,
            "source": self.source,
            "scope": self.scope,
            "archived": self.archived,
            "retention_policy": self.retention_policy,
            "domain": self.domain,
            "metadata": canonicalize_value(&metadata),
            "recall_count": self.recall_count,
            "query_diversity": self.query_diversity,
            "tier": self.tier,
        });
        digest_string(&serde_json::to_string(&canonicalize_value(&content)).unwrap_or_default())
    }

    pub(crate) fn copy_identity_sha256(&self) -> String {
        let identity = json!({
            "path": self.path,
            "text": self.text,
            "importance": self.importance,
            "timestamp": self.timestamp,
            "valid_from": self.valid_from,
            "valid_until": self.valid_until,
            "category": self.category,
            "topic": self.topic,
            "source": self.source,
            "scope": self.scope,
            "retention_policy": self.retention_policy,
            "domain": self.domain,
        });
        digest_string(&serde_json::to_string(&canonicalize_value(&identity)).unwrap_or_default())
    }

    pub(crate) fn vector_fingerprint(&self, table_present: bool) -> VectorFingerprint {
        VectorFingerprint::from_vector(table_present, self.vector.as_deref())
    }

    pub(crate) fn is_superseded_by(&self, id: &str) -> bool {
        self.superseded_by.as_deref() == Some(id)
    }
}

impl StoreScan {
    pub(crate) fn fingerprint(&self) -> Option<PlanStoreFingerprint> {
        let physical = self.physical.as_ref()?;
        let schema = self.report.stored_schema?;
        let mut rows = self
            .rows
            .iter()
            .map(|row| plan_row_fingerprint(row, self.vector_table_present))
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        Some(PlanStoreFingerprint {
            logical_store_ref: self.spec.logical_store.reference().to_string(),
            physical_id: physical.physical_id.clone(),
            canonical_path: physical.canonical_path.clone(),
            schema,
            total_memory_rows: self.report.counts.total_memory_rows,
            row_digest: self.row_digest.clone(),
            vector_table_present: self.vector_table_present,
            rows,
        })
    }

    pub(crate) fn raw_row(&self, id: &str) -> Option<&RawRow> {
        self.rows.iter().find(|row| row.id == id)
    }
}

/// Hold a writer reservation on every distinct physical store while the
/// completed no-op proof is refreshed. SQLite has no transaction spanning the
/// separate corpus databases, so the stores are acquired in deterministic
/// physical-id order; the final inventory is taken only after all reservations
/// are held. A failed acquisition therefore refuses the no-op rather than
/// returning from an unstable snapshot.
pub(crate) struct CompletedNoOpLocks {
    pub(crate) stores: Vec<MemoryStore>,
}

impl CompletedNoOpLocks {
    pub(crate) fn acquire(scans: &[StoreScan]) -> Result<Self, String> {
        let mut physical_stores = BTreeMap::<String, (PathBuf, Vec<PathBuf>)>::new();
        for scan in scans {
            check_authority(scan, None)?;
            let physical = scan.physical.as_ref().ok_or_else(|| {
                format!(
                    "completed no-op store {} has no physical identity",
                    scan.spec.logical_store.reference()
                )
            })?;
            let addressed_path = scan.spec.addressed_path.clone().ok_or_else(|| {
                format!(
                    "completed no-op store {} has no addressed path",
                    scan.spec.logical_store.reference()
                )
            })?;
            let entry = physical_stores
                .entry(physical.physical_id.clone())
                .or_insert_with(|| (PathBuf::from(&physical.primary_path), Vec::new()));
            entry.1.push(addressed_path);
        }

        let mut stores = Vec::with_capacity(physical_stores.len());
        for (physical_id, (primary_path, addressed_paths)) in physical_stores {
            let store = MemoryStore::open_existing_read_write(&primary_path.display().to_string())
                .map_err(|error| {
                    format!("completed no-op cannot open physical store {physical_id}: {error}")
                })?;
            for addressed_path in addressed_paths {
                verify_opened_apply_store(&store, &addressed_path, "completed no-op")?;
            }
            store
                .connection()
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(|error| {
                    format!("completed no-op cannot acquire writer lock for {physical_id}: {error}")
                })?;
            stores.push(store);
        }
        Ok(Self { stores })
    }
}

impl Drop for CompletedNoOpLocks {
    fn drop(&mut self) {
        for store in &self.stores {
            let _ = store.connection().execute_batch("ROLLBACK");
        }
    }
}

/// The proven sibling-race damage signature for one deterministic target row.
///
/// It is derived entirely from the row's own migration receipt plus its source
/// row, so the repair needs no plan file: the receipt carries the frozen
/// `PlanItem` and `plan_id` that produced the damage.
#[derive(Debug, Clone)]
pub(crate) struct SiblingDamageSignature {
    pub(crate) item: PlanItem,
    pub(crate) plan_id: String,
    pub(crate) replaced_receipt: Value,
}

pub(crate) enum SiblingDamageAssessment {
    /// Every clause of the signature matched; the row can be compensated.
    Repairable(Box<SiblingDamageSignature>),
    /// At least one clause did not match. The row is reported, never touched.
    Skipped(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainedFileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows { volume: u32, index: u64 },
    #[cfg(not(any(unix, windows)))]
    Unsupported,
}

pub(crate) fn retained_file_identity(
    metadata: &std::fs::Metadata,
) -> Result<RetainedFileIdentity, String> {
    if !metadata.file_type().is_file() {
        return Err("protected Wiki corpus object is not a regular file".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let device = metadata.dev();
        let inode = metadata.ino();
        if device == 0 || inode == 0 {
            return Err("protected Wiki corpus object has no stable Unix identity".to_string());
        }
        Ok(RetainedFileIdentity::Unix { device, inode })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let volume = metadata.volume_serial_number().ok_or_else(|| {
            "protected Wiki corpus object has no Windows volume identity".to_string()
        })?;
        let index = metadata.file_index().ok_or_else(|| {
            "protected Wiki corpus object has no Windows file identity".to_string()
        })?;
        Ok(RetainedFileIdentity::Windows { volume, index })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        Err("protected Wiki corpus writes require stable filesystem identity".to_string())
    }
}

pub(crate) struct RetainedPathFile {
    pub(crate) file: File,
    pub(crate) identity: RetainedFileIdentity,
}

impl RetainedPathFile {
    pub(crate) fn from_file(path: &Path, file: File) -> Result<Self, String> {
        let identity = retained_file_identity(&file.metadata().map_err(|error| {
            format!(
                "cannot inspect retained protected path {}: {error}",
                path.display()
            )
        })?)?;
        Ok(Self { file, identity })
    }

    pub(crate) fn open(
        path: &Path,
        read: bool,
        write: bool,
        create_new: bool,
    ) -> Result<Self, String> {
        let file = open_file_no_follow(path, read, write, create_new)
            .map_err(|error| format!("cannot open protected path {}: {error}", path.display()))?;
        Self::from_file(path, file)
    }

    pub(crate) fn verify_path(&self, path: &Path) -> Result<(), String> {
        let metadata = regular_non_symlink_metadata(path)?;
        let current = retained_file_identity(&metadata)?;
        if current != self.identity {
            return Err(format!(
                "protected Wiki corpus path identity changed after reservation: {}",
                path.display()
            ));
        }
        Ok(())
    }
}

pub(crate) struct RetainedBackup {
    pub(crate) receipt: BackupReceipt,
    pub(crate) path: PathBuf,
    pub(crate) retained: RetainedPathFile,
}

impl RetainedBackup {
    pub(crate) fn verify(&self) -> Result<(), String> {
        self.retained.verify_path(&self.path)?;
        let inventory = classify_paths([self.path.clone()]);
        if let Some(unresolved) = inventory.unresolved_paths.first() {
            return Err(format!(
                "retained backup {} has no readable physical identity: {}",
                self.path.display(),
                unresolved.error
            ));
        }
        let physical = inventory
            .stores
            .into_iter()
            .next()
            .ok_or_else(|| "retained backup has no physical identity".to_string())?;
        if physical.physical_id != self.receipt.backup_physical_id {
            return Err(format!(
                "retained backup {} physical identity changed",
                self.path.display()
            ));
        }
        self.retained.verify_path(&self.path)?;
        let open_path = canonical_parent_open_path(Path::new(&physical.open_path))?;
        let connection = open_sqlite_no_follow(&open_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| error.to_string())?;
        verify_sqlite_connection_retained_identity(&connection, &self.retained, &self.path)?;
        let schema = read_schema_version(&connection).map_err(|error| error.to_string())?;
        let quick_check = quick_check(&connection)?;
        let (rows, _, vector_table_present) = load_rows(&connection)?;
        let digest = row_digest(&rows, vector_table_present);
        if schema != self.receipt.schema
            || rows.len() != self.receipt.total_memory_rows
            || digest != self.receipt.backup_row_digest
            || vector_table_present != self.receipt.vector_table_present
            || quick_check != self.receipt.quick_check
        {
            return Err(format!(
                "retained backup {} evidence changed after verification",
                self.path.display()
            ));
        }
        verify_sqlite_connection_retained_identity(&connection, &self.retained, &self.path)?;
        self.retained.verify_path(&self.path)
    }
}

/// One legacy row that was not adopted, with the exact rule that excluded it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AdoptionSkip {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) reason: String,
}

/// A preserved supersession edge whose target is absent from the new store.
/// Reported, never fatal: repair is tachi#1350's operation.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AdoptionDanglingEdge {
    pub(crate) id: String,
    pub(crate) superseded_by: String,
}

/// A row whose stored path will differ from its source path because the
/// shared write path normalizes it.
///
/// The lifecycle checksum does **not** cover `path`, so a silent rewrite would
/// otherwise pass verification unnoticed. Every rewrite is disclosed here
/// before the run is confirmed, and the confirmed run reads the destination's
/// `path` column back and refuses to call itself clean if any stored path
/// differs from the value predicted here.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AdoptionPathRewrite {
    pub(crate) id: String,
    pub(crate) source_path: String,
    pub(crate) stored_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LegacyAdoptionReport {
    pub(crate) version: String,
    pub(crate) confirmed: bool,
    pub(crate) confirm_token_required: String,
    pub(crate) target_path: String,
    pub(crate) target_existed_before: bool,
    /// True once this run has created the store's directory, i.e. from the
    /// first side effect onward — not only after a successful import. Read it
    /// together with `target_removed_after_failure`: `true`/`Some(true)` means
    /// the run created something and then cleaned it up again.
    pub(crate) target_created: bool,
    /// `Some(false)` is a hard failure, not a warning: an unstamped store is
    /// not the wiki corpus and must not be left holding adopted rows.
    pub(crate) target_store_role_stamped: Option<bool>,
    /// Set when a post-creation failure was compensated by removing what this
    /// run wrote: the whole `target_dir` if this run created it, or just the
    /// db file (and its WAL/SHM sidecars) if `target_dir` preexisted this
    /// run. `Some(false)` means the removal itself failed and the operator
    /// must clean up by hand; `remediation` states exactly what to remove.
    pub(crate) target_removed_after_failure: Option<bool>,
    pub(crate) legacy_open_path: Option<String>,
    pub(crate) legacy_physical_id: Option<String>,
    pub(crate) legacy_row_digest_before: String,
    pub(crate) legacy_row_digest_after: Option<String>,
    /// Rows the classifier calls wiki-related, i.e. rows carrying a
    /// classification. Rows the legacy store holds that are not wiki-related
    /// are neither counted here nor listed in `skipped`: they are not part of
    /// this corpus and a `classification_missing` entry per non-wiki row would
    /// bury the skip histogram an operator has to read.
    pub(crate) wiki_related_rows: usize,
    pub(crate) eligible_rows: usize,
    pub(crate) skipped: Vec<AdoptionSkip>,
    pub(crate) adopted_ids: Vec<String>,
    pub(crate) adoption_run_id: String,
    pub(crate) provenance_marker_key: String,
    /// Derived lifecycle of every adopted row, keyed by lifecycle name. A run
    /// can import every row, match every checksum, and still leave search
    /// empty if nothing derives a default-retrievable lifecycle — so the
    /// receipt states this rather than leaving it to be inferred.
    pub(crate) derived_lifecycle_counts: BTreeMap<String, usize>,
    pub(crate) default_retrievable_rows: usize,
    pub(crate) path_rewrites: Vec<AdoptionPathRewrite>,
    pub(crate) rows_imported: usize,
    pub(crate) vectors_imported: usize,
    pub(crate) vectors_absent: usize,
    pub(crate) dangling_supersessions: Vec<AdoptionDanglingEdge>,
    pub(crate) expected_lifecycle_checksum: String,
    pub(crate) expected_vector_checksum: String,
    pub(crate) observed_lifecycle_checksum: Option<String>,
    pub(crate) observed_vector_checksum: Option<String>,
    pub(crate) checksums_match: Option<bool>,
    /// The standing consequence of copy-without-supersede; see the module
    /// section above.
    pub(crate) reconciler_impact: String,
    pub(crate) legacy_rows_forced_to_manual_review_by_duplicate_path: Option<usize>,
    /// True when the run recorded a failure instead of returning `Err`. The
    /// CLI turns this into a non-zero exit after printing the receipt.
    pub(crate) had_failures: bool,
    /// Populated only when the operator still has cleanup to do.
    pub(crate) remediation: Option<String>,
    pub(crate) errors: Vec<String>,
}

pub(crate) const WIKI_LEGACY_ADOPTION_RECONCILER_IMPACT: &str =
    "adoption copies rows without superseding the legacy source, so every adopted normalized \
     path now exists in two logical stores; finalize_classifications forces both copies to \
     manual_review and build_plan only emits shared_candidate items, so `tachi wiki corpus \
     --apply` is inert for these paths until one side is deleted (tachi#1611 phase 5 must \
     delete a side first)";

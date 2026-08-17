//! Read-only Wiki corpus inventory and backup-gated migration.
//!
//! Preview deliberately avoids MemoryServer/MemoryStore opens. Legacy Wiki
//! stores are part of the inventory, and ordinary opens can reject or migrate
//! them. Preview uses raw read-only SQLite plus the existing physical identity
//! inventory. Writes are reached only after schema, identity, plan, and backup
//! gates pass.

use crate::physical_db_identity::{
    classify_memory_failure, classify_open_failure, classify_paths, open_read_only_connection,
    physical_db_bindings_for_paths, InventoryFailureKind, OpenPathBasis, PhysicalDbStore,
    PhysicalStoreMutationState,
};
use crate::server_state::MemoryServer;
use crate::tool_params::{
    derive_effective_knowledge_artifact, EffectiveKnowledgeArtifactV1, WikiApplicabilityStatusV1,
    WikiKnowledgeScopeV1,
};
use memcore::db::migrations::{read_schema_version, EXPECTED_SCHEMA_VERSION};
use memcore::{ExpectedMemoryState, InsertMemoryResult, MemoryEntry, MemoryStore};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub(crate) const WIKI_CORPUS_CONFIRMATION_TOKEN: &str = "MIGRATE_WIKI_CORPUS_V1";
/// Separate token from `WIKI_CORPUS_CONFIRMATION_TOKEN` on purpose: a
/// copy-pasted apply command must never be able to trigger a repair write.
pub(crate) const WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN: &str =
    "REPAIR_WIKI_CORPUS_SIBLING_DAMAGE_V1";
const REPORT_VERSION: &str = "wiki_corpus_migration_v1";
const REPAIR_REPORT_VERSION: &str = "wiki_corpus_sibling_repair_v1";
const RECEIPT_KEY: &str = "wiki_corpus_migration";
const RECEIPT_VERSION: u32 = 2;
/// History key holding every sibling-damage repair applied to a row. The
/// migration receipt itself is restored to the canonical `target_copied`
/// terminal state so that every existing completion predicate keeps its exact
/// meaning; the replaced receipt is preserved here instead of being erased.
const REPAIR_RECEIPT_KEY: &str = "wiki_corpus_sibling_repair";
const REPAIR_RECEIPT_VERSION: u32 = 1;
const REPAIR_PHASE: &str = "sibling_damage_repaired";
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
const WIKI_LEGACY_ADOPTION_MARKER_KEY: &str = "wiki_legacy_adoption_v1";
const WIKI_LEGACY_ADOPTION_REPORT_VERSION: &str = "wiki-legacy-adoption/v1";
static PREVIEW_SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A read-only SQLite handle backed by a private filesystem snapshot.
///
/// SQLite's ordinary read-only WAL open can create the `-shm` file when the
/// source has no shared-memory sidecar yet.  Preview therefore copies the
/// main database and any existing WAL/SHM sidecars into a private temporary
/// directory before opening the copy.  The source is never opened by SQLite;
/// metadata is checked before and after the copy so a concurrent source change
/// fails closed instead of producing an unqualified inventory.
struct PreviewConnection {
    connection: Option<rusqlite::Connection>,
    staging_dir: Option<PathBuf>,
}

impl Deref for PreviewConnection {
    type Target = rusqlite::Connection;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("preview connection is live while borrowed")
    }
}

impl Drop for PreviewConnection {
    fn drop(&mut self) {
        self.connection.take();
        if let Some(staging_dir) = self.staging_dir.take() {
            let _ = fs::remove_dir_all(staging_dir);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewFileFingerprint {
    length: u64,
    modified: Option<std::time::SystemTime>,
}

fn preview_file_fingerprint(path: &Path) -> Result<Option<PreviewFileFingerprint>, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(PreviewFileFingerprint {
            length: metadata.len(),
            modified: metadata.modified().ok(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot stat preview source {}: {error}",
            path.display()
        )),
    }
}

fn preview_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn preview_source_fingerprints(path: &Path) -> Result<[Option<PreviewFileFingerprint>; 3], String> {
    Ok([
        preview_file_fingerprint(path)?,
        preview_file_fingerprint(&preview_sidecar_path(path, "-wal"))?,
        preview_file_fingerprint(&preview_sidecar_path(path, "-shm"))?,
    ])
}

fn create_private_preview_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        let metadata = fs::symlink_metadata(path)?;
        let owner = unsafe { libc::geteuid() };
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_dir()
            || metadata.uid() != owner
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "preview staging directory is not an owner-only 0700 directory",
            ));
        }
    }
    Ok(())
}

fn preview_staging_dir_in(root: &Path) -> Result<PathBuf, String> {
    let process_id = std::process::id();
    for attempt in 0..32u64 {
        let sequence = PREVIEW_SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(
            "sigil-wiki-corpus-preview-{process_id}-{sequence}-{attempt}"
        ));
        match create_private_preview_directory(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "cannot create private preview staging directory {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Err("cannot allocate a private preview staging directory".to_string())
}

fn preview_staging_dir() -> Result<PathBuf, String> {
    preview_staging_dir_in(&std::env::temp_dir())
}

fn copy_preview_file(source: &Path, destination: &Path, required: bool) -> Result<(), String> {
    match fs::copy(source, destination) {
        Ok(_) => Ok(()),
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot snapshot preview source {}: {error}",
            source.display()
        )),
    }
}

fn open_preview_connection(source: &Path) -> Result<PreviewConnection, String> {
    let before = preview_source_fingerprints(source)?;
    if before[0].is_none() {
        return Err(format!(
            "preview source {} does not exist",
            source.display()
        ));
    }
    let staging_dir = preview_staging_dir()?;
    let staged_path = staging_dir.join("memory.db");
    let result = (|| {
        copy_preview_file(source, &staged_path, true)?;
        copy_preview_file(
            &preview_sidecar_path(source, "-wal"),
            &preview_sidecar_path(&staged_path, "-wal"),
            false,
        )?;
        copy_preview_file(
            &preview_sidecar_path(source, "-shm"),
            &preview_sidecar_path(&staged_path, "-shm"),
            false,
        )?;
        let after = preview_source_fingerprints(source)?;
        if before != after {
            return Err(format!(
                "preview source {} changed while staging an immutable snapshot",
                source.display()
            ));
        }
        let connection = open_read_only_connection(&staged_path)
            .map_err(|error| format!("cannot open immutable preview snapshot: {error}"))?;
        Ok(connection)
    })();
    match result {
        Ok(connection) => Ok(PreviewConnection {
            connection: Some(connection),
            staging_dir: Some(staging_dir),
        }),
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_dir);
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CorpusClassification {
    SharedCandidate,
    ProjectBound,
    OperationalSnapshot,
    TestEphemeral,
    AuthorityRecord,
    ManualReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogicalStore {
    BoundProject,
    SharedWiki,
    LegacyGlobal,
}

impl LogicalStore {
    fn reference(self) -> &'static str {
        match self {
            Self::BoundProject => "bound_project",
            Self::SharedWiki => "named:wiki",
            Self::LegacyGlobal => "legacy_global",
        }
    }

    fn semantic_role(self) -> &'static str {
        match self {
            Self::BoundProject => "current_bound_project_store",
            Self::SharedWiki => "logical_shared_wiki_store",
            Self::LegacyGlobal => "legacy_global_store",
        }
    }
}

#[derive(Debug, Clone)]
struct StoreSpec {
    logical_store: LogicalStore,
    addressed_path: Option<PathBuf>,
    resolution_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ReadFailure {
    kind: InventoryFailureKind,
    message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ClassificationCounts {
    shared_candidate: usize,
    project_bound: usize,
    operational_snapshot: usize,
    test_ephemeral: usize,
    authority_record: usize,
    manual_review: usize,
}

impl ClassificationCounts {
    fn increment(&mut self, classification: CorpusClassification) {
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
struct RowCounts {
    total_memory_rows: usize,
    wiki_related_rows: usize,
    classifications: ClassificationCounts,
}

#[derive(Debug, Clone, Serialize)]
struct TypedMetadataEvidence {
    raw_metadata_sha256: String,
    artifact_kind: Option<Value>,
    knowledge_scope: Option<Value>,
    origin_projects: Option<Value>,
    applies_to: Option<Value>,
    applicability_status: Option<Value>,
    lifecycle: Option<Value>,
    authority: Option<Value>,
    review_receipt: Option<Value>,
    operational_markers: Vec<String>,
    test_markers: Vec<String>,
    authority_markers: Vec<String>,
    migration_receipt: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct WikiCorpusRow {
    logical_store_ref: String,
    id: String,
    path: String,
    normalized_path: String,
    summary: String,
    source: String,
    category: String,
    domain: Option<String>,
    legacy_scope: String,
    revision: i64,
    archived: bool,
    superseded_by: Option<String>,
    content_sha256: String,
    classification: CorpusClassification,
    reasons: Vec<String>,
    effective_typed_metadata: EffectiveKnowledgeArtifactV1,
    typed_metadata_evidence: TypedMetadataEvidence,
}

#[derive(Debug, Clone, Serialize)]
struct StoreReport {
    logical_store_ref: String,
    semantic_role: String,
    addressed_path: Option<String>,
    existence: bool,
    resolved_path: Option<String>,
    canonical_path: Option<String>,
    open_path: Option<String>,
    open_path_basis: Option<OpenPathBasis>,
    physical_identity: Option<PhysicalDbStore>,
    read_failure: Option<ReadFailure>,
    stored_schema: Option<u32>,
    expected_schema: u32,
    counts: RowCounts,
    rows: Vec<WikiCorpusRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PlanRowFingerprint {
    id: String,
    revision: i64,
    content_sha256: String,
    superseded_by: Option<String>,
    vector: VectorFingerprint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PlanStoreFingerprint {
    logical_store_ref: String,
    physical_id: String,
    canonical_path: String,
    schema: u32,
    total_memory_rows: usize,
    row_digest: String,
    vector_table_present: bool,
    rows: Vec<PlanRowFingerprint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct VectorFingerprint {
    table_present: bool,
    row_present: bool,
    dimensions: Option<usize>,
    sha256: Option<String>,
}

impl VectorFingerprint {
    fn absent(table_present: bool) -> Self {
        Self {
            table_present,
            row_present: false,
            dimensions: None,
            sha256: None,
        }
    }

    fn from_vector(table_present: bool, vector: Option<&[f32]>) -> Self {
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
struct PlanItem {
    action: String,
    source_store_ref: String,
    source_physical_id: String,
    source_id: String,
    source_path: String,
    normalized_path: String,
    source_revision: i64,
    source_content_sha256: String,
    source_copy_identity_sha256: String,
    source_valid_until: Option<String>,
    source_superseded_by: Option<String>,
    source_vector: VectorFingerprint,
    replay_identity: String,
    target_store_ref: String,
    target_physical_id: String,
    target_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WikiCorpusPlan {
    version: String,
    plan_id: String,
    store_fingerprints: Vec<PlanStoreFingerprint>,
    items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BackupReceipt {
    logical_store_refs: Vec<String>,
    source_physical_id: String,
    source_canonical_path: String,
    source_open_path: String,
    backup_path: String,
    backup_physical_id: String,
    schema: u32,
    total_memory_rows: usize,
    source_row_digest: String,
    backup_row_digest: String,
    vector_table_present: bool,
    source_identity_verified: bool,
    backup_identity_verified: bool,
    quick_check: String,
    verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupManifest {
    version: String,
    status: String,
    plan_id: String,
    backup_directory: String,
    receipts: Vec<BackupReceipt>,
    migration_receipts: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct MigrationOutcome {
    source_store_ref: String,
    source_id: String,
    target_store_ref: String,
    target_id: Option<String>,
    action: String,
    outcome: String,
    phases: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MigrationPhase {
    Planned,
    TargetCopied,
    TargetNoncanonical,
    SourceReceipted,
    SourceSuperseded,
    Reclassified,
}

impl MigrationPhase {
    fn as_str(self) -> &'static str {
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
enum MigrationBoundary {
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
enum CorpusRaceHook {
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
enum SiblingCompletionSeam {
    /// After this worker's source precheck, before it inspects the target.
    AfterSourcePrecheck,
    /// After this worker prepared its final receipt and expected transition
    /// state, before it decides whether the atomic transition still applies.
    AfterReceiptPrepared,
}

#[derive(Debug, Clone)]
struct MigrationInterruption {
    source_id: Option<String>,
    boundary: MigrationBoundary,
}

fn maybe_interrupt(
    interruption: &mut Option<MigrationInterruption>,
    item: &PlanItem,
    boundary: MigrationBoundary,
) -> Result<(), String> {
    let matches = interruption.as_ref().is_some_and(|pending| {
        pending.boundary == boundary
            && pending
                .source_id
                .as_deref()
                .is_none_or(|source_id| source_id == item.source_id)
    });
    if matches {
        interruption.take();
        return Err(format!(
            "simulated interruption after {:?} for {}:{}",
            boundary, item.source_store_ref, item.source_id
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MigrationReceipt {
    kind: String,
    version: u32,
    plan_id: String,
    phase: MigrationPhase,
    item: PlanItem,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WikiCorpusReport {
    version: String,
    mode: String,
    apply: bool,
    stores: Vec<StoreReport>,
    plan: Option<WikiCorpusPlan>,
    backup_manifest: Option<BackupManifest>,
    migration_outcomes: Vec<MigrationOutcome>,
    warnings: Vec<String>,
    /// Present only in the sibling-damage repair modes, so the JSON shape of
    /// preview and apply is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    sibling_repair: Option<SiblingRepairReport>,
    /// Present only in the legacy-adoption modes, so the JSON shape of
    /// preview, apply, and sibling repair is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    legacy_adoption: Option<LegacyAdoptionReport>,
}

impl WikiCorpusReport {
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
struct SiblingRepairRow {
    store_ref: String,
    id: String,
    path: String,
    outcome: String,
    observed_receipt_phase: Option<String>,
    observed_archived: bool,
    observed_superseded_by: Option<String>,
    observed_copy_identity_sha256: String,
    expected_receipt_phase: String,
    expected_archived: bool,
    expected_copy_identity_sha256: Option<String>,
    plan_id: Option<String>,
    source_store_ref: Option<String>,
    source_id: Option<String>,
    skipped_reasons: Vec<String>,
    failure_reason: Option<String>,
}

/// A confirmed run never discards a partially completed report: each row's
/// repair is its own atomic, idempotent transaction (see
/// `repair_sibling_damaged_target`), so a row that fails to compensate is
/// recorded as `failed` in `rows`/`errors` instead of unwinding the rows
/// already repaired. `had_failures` is the caller-facing summary bit; the CLI
/// layer has no exit-code convention for a partially failed report today, so
/// this field -- not the process exit code -- is the operator-visible signal
/// that a re-run is needed.
#[derive(Debug, Clone, Serialize)]
struct SiblingRepairReport {
    version: String,
    confirmed: bool,
    backup_directory: Option<String>,
    inspected_rows: usize,
    repairable: usize,
    repaired: usize,
    skipped: usize,
    failed: usize,
    had_failures: bool,
    backups: Vec<BackupReceipt>,
    rows: Vec<SiblingRepairRow>,
    errors: Vec<String>,
}

#[derive(Debug, Clone)]
struct RawRow {
    id: String,
    path: String,
    summary: String,
    text: String,
    importance: f64,
    timestamp: String,
    valid_from: String,
    valid_until: Option<String>,
    category: String,
    topic: String,
    keywords: Vec<String>,
    entities: Vec<String>,
    source: String,
    scope: String,
    archived: bool,
    revision: i64,
    // Round-2 bug C: usage counters, loaded verbatim so adoption can carry
    // them through. `import_snapshot_batch` writes these four fields from
    // `MemoryEntry` (memcore's `snapshot_import.rs`), but they are outside
    // the lifecycle checksum's coverage (archived/created_at/id/revision/
    // superseded_by/updated_at/valid_until, per #1607's scope) -- same
    // category as `path`, which gets its own `verify_adopted_paths` readback
    // for the same reason. Preservation here is asserted by
    // `adopt_legacy_preserves_usage_counters_verbatim`, not by the checksum;
    // deliberately not widening the checksum's scope to cover them.
    access_count: i64,
    scored_count: i64,
    last_access: Option<String>,
    last_use_at: Option<String>,
    retention_policy: Option<String>,
    domain: Option<String>,
    metadata: Value,
    vector: Option<Vec<f32>>,
    recall_count: i64,
    query_diversity: i64,
    tier: String,
    superseded_by: Option<String>,
    metadata_parse_error: Option<String>,
    classification: Option<CorpusClassification>,
    reasons: Vec<String>,
    effective: Option<EffectiveKnowledgeArtifactV1>,
}

struct StoreScan {
    spec: StoreSpec,
    physical: Option<PhysicalDbStore>,
    rows: Vec<RawRow>,
    report: StoreReport,
    row_digest: String,
    vector_table_present: bool,
}

impl RawRow {
    fn normalized_path(&self) -> String {
        memcore::path_router::normalize_path(&self.path)
    }

    fn content_sha256(&self) -> String {
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

    fn copy_identity_sha256(&self) -> String {
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

    fn vector_fingerprint(&self, table_present: bool) -> VectorFingerprint {
        VectorFingerprint::from_vector(table_present, self.vector.as_deref())
    }

    fn is_superseded_by(&self, id: &str) -> bool {
        self.superseded_by.as_deref() == Some(id)
    }
}

impl StoreScan {
    fn fingerprint(&self) -> Option<PlanStoreFingerprint> {
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

    fn raw_row(&self, id: &str) -> Option<&RawRow> {
        self.rows.iter().find(|row| row.id == id)
    }
}

fn digest_string(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn canonicalize_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_value).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let mut result = Map::new();
            for key in keys {
                if let Some(value) = object.get(&key) {
                    result.insert(key, canonicalize_value(value));
                }
            }
            Value::Object(result)
        }
        _ => value.clone(),
    }
}

fn json_array(raw: Option<String>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
        .unwrap_or_default()
}

fn read_optional_string(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<String>> {
    row.get(index)
}

fn load_rows(conn: &Connection) -> Result<(Vec<RawRow>, usize, bool), String> {
    if !memcore::db::table_exists(conn, "memories").map_err(|e| e.to_string())? {
        return Err("memories table is missing".to_string());
    }

    let mut statement = conn
        .prepare("PRAGMA table_info(memories)")
        .map_err(|e| e.to_string())?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|e| e.to_string())?;
    if !columns.contains("id") {
        return Err("memories table has no id column".to_string());
    }

    let expression = |name: &str, fallback: &str| -> String {
        if columns.contains(name) {
            format!("\"{name}\"")
        } else {
            fallback.to_string()
        }
    };
    let select = [
        expression("id", "NULL"),
        expression("path", "'/'"),
        expression("summary", "''"),
        expression("text", "''"),
        expression("importance", "0.0"),
        expression("timestamp", "''"),
        expression("valid_from", "''"),
        expression("valid_until", "NULL"),
        expression("category", "''"),
        expression("topic", "''"),
        expression("keywords", "'[]'"),
        expression("entities", "'[]'"),
        expression("source", "''"),
        expression("scope", "'general'"),
        expression("archived", "0"),
        expression("revision", "0"),
        expression("retention_policy", "NULL"),
        expression("domain", "NULL"),
        expression("metadata", "'{}'"),
        expression("recall_count", "0"),
        expression("query_diversity", "0"),
        expression("tier", "'raw'"),
        expression("superseded_by", "NULL"),
        // Round-2 bug C: appended rather than interleaved so every existing
        // positional `row.get(N)` above keeps its index unchanged.
        expression("access_count", "0"),
        expression("scored_count", "0"),
        expression("last_access", "NULL"),
        expression("last_use_at", "NULL"),
    ]
    .join(", ");
    let sql = format!("SELECT {select} FROM memories ORDER BY id ASC");
    let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    let mapped = statement
        .query_map([], |row| {
            let raw_metadata = read_optional_string(row, 18)?;
            let (metadata, metadata_parse_error) = match raw_metadata {
                None => (Value::Object(Map::new()), None),
                Some(raw) => match serde_json::from_str::<Value>(&raw) {
                    Ok(value) => (value, None),
                    Err(error) => (
                        Value::Null,
                        Some(format!("metadata JSON is invalid: {error}")),
                    ),
                },
            };
            Ok(RawRow {
                id: row.get(0)?,
                path: row.get(1)?,
                summary: row.get(2)?,
                text: row.get(3)?,
                importance: row.get(4)?,
                timestamp: row.get(5)?,
                valid_from: row.get(6)?,
                valid_until: row.get(7)?,
                category: row.get(8)?,
                topic: row.get(9)?,
                keywords: json_array(row.get(10)?),
                entities: json_array(row.get(11)?),
                source: row.get(12)?,
                scope: row.get(13)?,
                archived: row.get::<_, i64>(14)? != 0,
                revision: row.get(15)?,
                retention_policy: row.get(16)?,
                domain: row.get(17)?,
                metadata,
                vector: None,
                recall_count: row.get(19)?,
                query_diversity: row.get(20)?,
                tier: row.get(21)?,
                superseded_by: row.get(22)?,
                access_count: row.get(23)?,
                scored_count: row.get(24)?,
                last_access: row.get(25)?,
                last_use_at: row.get(26)?,
                metadata_parse_error,
                classification: None,
                reasons: Vec::new(),
                effective: None,
            })
        })
        .map_err(|e| e.to_string())?;
    for row in mapped {
        rows.push(row.map_err(|e| e.to_string())?);
    }
    let vector_table_present =
        memcore::db::table_exists(conn, "memories_vec").map_err(|error| error.to_string())?;
    if vector_table_present {
        for row in &mut rows {
            let blob = conn
                .query_row(
                    "SELECT embedding FROM memories_vec WHERE id = ?1",
                    rusqlite::params![row.id],
                    |result| result.get::<_, Vec<u8>>(0),
                )
                .optional()
                .map_err(|error| format!("cannot read vector for {}: {error}", row.id))?;
            if let Some(blob) = blob {
                if blob.len() % 4 != 0 {
                    return Err(format!(
                        "vector for {} has invalid blob length {}",
                        row.id,
                        blob.len()
                    ));
                }
                row.vector = Some(
                    blob.chunks_exact(4)
                        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                        .collect(),
                );
            }
        }
    }
    let count = rows.len();
    Ok((rows, count, vector_table_present))
}

fn plan_row_fingerprint(row: &RawRow, vector_table_present: bool) -> PlanRowFingerprint {
    PlanRowFingerprint {
        id: row.id.clone(),
        revision: row.revision,
        content_sha256: row.content_sha256(),
        superseded_by: row.superseded_by.clone(),
        vector: row.vector_fingerprint(vector_table_present),
    }
}

fn row_digest_values(mut values: Vec<Value>) -> String {
    values.sort_by_key(|value| {
        value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    });
    digest_string(
        &serde_json::to_string(&canonicalize_value(&Value::Array(values))).unwrap_or_default(),
    )
}

fn row_digest(rows: &[RawRow], vector_table_present: bool) -> String {
    row_digest_values(
        rows.iter()
            .map(|row| {
                serde_json::to_value(plan_row_fingerprint(row, vector_table_present))
                    .unwrap_or(Value::Null)
            })
            .collect(),
    )
}

fn metadata_value(metadata: &Value, key: &str) -> Option<Value> {
    metadata.get(key).cloned()
}

fn marker_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

fn marker_bool(metadata: &Value, key: &str) -> bool {
    metadata.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn exact_marker(metadata: &Value, keys: &[&str], values: &[&str]) -> bool {
    keys.iter()
        .filter_map(|key| marker_string(metadata, key))
        .any(|value| values.iter().any(|candidate| value == *candidate))
}

fn marker_list(metadata: &Value, keys: &[&str], values: &[&str]) -> Vec<String> {
    let mut markers = Vec::new();
    for key in keys {
        if marker_bool(metadata, key) {
            markers.push((*key).to_string());
        }
        if let Some(value) = marker_string(metadata, key) {
            if values.iter().any(|candidate| value == *candidate) {
                markers.push(format!("{key}={value}"));
            }
        }
    }
    markers.sort();
    markers.dedup();
    markers
}

fn memory_entry_from_raw(row: &RawRow) -> MemoryEntry {
    MemoryEntry {
        id: row.id.clone(),
        path: row.path.clone(),
        summary: row.summary.clone(),
        text: row.text.clone(),
        importance: row.importance,
        timestamp: row.timestamp.clone(),
        valid_from: row.valid_from.clone(),
        valid_until: row.valid_until.clone(),
        category: row.category.clone(),
        topic: row.topic.clone(),
        keywords: row.keywords.clone(),
        persons: Vec::new(),
        entities: row.entities.clone(),
        location: String::new(),
        source: row.source.clone(),
        scope: row.scope.clone(),
        archived: row.archived,
        // Round-2 bug C: loaded verbatim from the legacy row instead of
        // hardcoded to zero/None -- `import_snapshot_batch` writes these
        // straight from this `MemoryEntry`, so hardcoding them here silently
        // dropped every adopted row's usage history.
        access_count: row.access_count,
        scored_count: row.scored_count,
        last_access: row.last_access.clone(),
        last_use_at: row.last_use_at.clone(),
        revision: row.revision,
        vector: row.vector.clone(),
        retention_policy: row.retention_policy.clone(),
        domain: row.domain.clone(),
        metadata: row.metadata.clone(),
        recall_count: row.recall_count,
        query_diversity: row.query_diversity,
        tier: row.tier.clone(),
    }
}

fn authority_markers(row: &RawRow) -> Vec<String> {
    let mut markers = marker_list(
        &row.metadata,
        &[
            "authority_record",
            "is_authority_record",
            "authority_kind",
            "authority",
            "record_kind",
            "kind",
            "memory_kind",
            "artifact_kind",
        ],
        &[
            "lane",
            "lane_card",
            "lane-card",
            "card",
            "soul",
            "agent_soul",
            "identity",
            "agent_identity",
            "governance",
            "governance_record",
            "governance_registry",
            "authority_record",
            "authoritative",
            "dispatch",
            "task_card",
        ],
    );
    let category = row.category.trim().to_ascii_lowercase();
    if [
        "lane",
        "lane_card",
        "lane-card",
        "card",
        "soul",
        "identity",
        "governance",
        "governance_registry",
    ]
    .contains(&category.as_str())
    {
        markers.push(format!("category={category}"));
    }
    let source = row.source.trim().to_ascii_lowercase();
    if [
        "lane",
        "lane_card",
        "lane-card",
        "card",
        "soul",
        "agent_soul",
        "governance",
        "identity",
        "governance_registry",
    ]
    .contains(&source.as_str())
    {
        markers.push(format!("source={source}"));
    }
    markers.sort();
    markers.dedup();
    markers
}

fn operational_markers(row: &RawRow) -> Vec<String> {
    let entry = memory_entry_from_raw(row);
    let mut markers = marker_list(
        &row.metadata,
        &[
            "operational_snapshot",
            "operational",
            "wiki_log",
            "recall_cache",
            "recall_rerank_cache",
            "cache_key",
            "rem_operation",
            "snapshot_kind",
            "record_kind",
            "kind",
        ],
        &[
            "operational_snapshot",
            "operational",
            "wiki_log",
            "rem",
            "rem_operation",
            "recall_cache",
            "recall_rerank_cache",
            "foundry_recall_rerank_cache",
            "wiki_snapshot",
        ],
    );
    let source = row.source.trim().to_ascii_lowercase();
    if [
        "wiki_log",
        "rem",
        "rem_operation",
        "recall_cache",
        "wiki_recall_cache",
        "foundry_recall_rerank_cache",
    ]
    .contains(&source.as_str())
    {
        markers.push(format!("source={source}"));
    }
    if memcore::namespace::is_wiki_log_entry(&entry) {
        markers.push("canonical_wiki_log_signal".to_string());
    }
    if memcore::namespace::is_recall_cache_entry(&entry) {
        markers.push("canonical_recall_cache_signal".to_string());
    }
    if memcore::namespace::is_reserved_wiki_rem_id(&row.id) || row.metadata.get("rem").is_some() {
        markers.push("canonical_rem_operation_signal".to_string());
    }
    let path = row.normalized_path();
    if [
        "/wiki/log",
        "/wiki/_log",
        "/wiki/operations",
        "/wiki/operational",
        "/wiki/recall-cache",
        "/wiki/rem",
        "/recall-cache",
    ]
    .iter()
    .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
    {
        markers.push(format!("path={path}"));
    }
    markers.sort();
    markers.dedup();
    markers
}

fn test_markers(row: &RawRow) -> Vec<String> {
    let mut markers = marker_list(
        &row.metadata,
        &[
            "test",
            "fixture",
            "ephemeral",
            "test_ephemeral",
            "record_kind",
            "kind",
        ],
        &["test", "fixture", "ephemeral", "test_ephemeral"],
    );
    let source = row.source.trim().to_ascii_lowercase();
    if ["test", "fixture", "ephemeral"].contains(&source.as_str()) {
        markers.push(format!("source={source}"));
    }
    let category = row.category.trim().to_ascii_lowercase();
    if ["test", "fixture", "ephemeral"].contains(&category.as_str()) {
        markers.push(format!("category={category}"));
    }
    markers.sort();
    markers.dedup();
    markers
}

fn is_wiki_related(row: &RawRow) -> bool {
    let entry = memory_entry_from_raw(row);
    let typed_artifact_signal = exact_marker(
        &row.metadata,
        &["artifact_kind", "record_kind", "kind"],
        &["wiki", "guide"],
    );
    memcore::namespace::is_wiki_entry(&entry)
        || memcore::namespace::is_wiki_log_entry(&entry)
        || memcore::namespace::is_recall_cache_entry(&entry)
        || memcore::namespace::is_reserved_wiki_rem_id(&row.id)
        || row.metadata.get("rem").is_some()
        || entry.is_guide()
        || typed_artifact_signal
}

fn valid_typed_shared(row: &RawRow, effective: &EffectiveKnowledgeArtifactV1) -> bool {
    let explicit_shared =
        marker_string(&row.metadata, "knowledge_scope").is_some_and(|scope| scope == "shared");
    let typed_origin = row
        .metadata
        .get("origin_projects")
        .and_then(Value::as_array)
        .is_some_and(|values| {
            !values.is_empty()
                && values
                    .iter()
                    .all(|value| value.as_str().is_some_and(|text| !text.trim().is_empty()))
        });
    explicit_shared
        && typed_origin
        && effective.knowledge_scope == WikiKnowledgeScopeV1::Shared
        && effective.applicability_status == WikiApplicabilityStatusV1::Bounded
        && !effective.validation_issues.iter().any(|issue| {
            issue.starts_with("malformed_")
                || issue == "shared_scope_missing_typed_origin"
                || issue == "shared_scope_not_bounded"
                || issue == "shared_active_without_bounded_applicability"
                || issue == "shared_active_without_review"
        })
}

fn classify_row(
    store: LogicalStore,
    row: &RawRow,
) -> (
    CorpusClassification,
    Vec<String>,
    EffectiveKnowledgeArtifactV1,
    TypedMetadataEvidence,
) {
    let effective = derive_effective_knowledge_artifact(&row.metadata, &row.path, &row.scope);
    let authority = authority_markers(row);
    let operational = operational_markers(row);
    let test = test_markers(row);
    let valid_shared = valid_typed_shared(row, &effective);
    let mut reasons = Vec::new();
    let classification = if !authority.is_empty() {
        reasons.push("typed_authority_record_evidence".to_string());
        reasons.extend(authority.iter().map(|marker| format!("authority:{marker}")));
        CorpusClassification::AuthorityRecord
    } else if !operational.is_empty() {
        reasons.push("typed_operational_snapshot_evidence".to_string());
        reasons.extend(
            operational
                .iter()
                .map(|marker| format!("operational:{marker}")),
        );
        CorpusClassification::OperationalSnapshot
    } else if !test.is_empty() {
        reasons.push("explicit_typed_test_fixture_or_ephemeral_evidence".to_string());
        reasons.extend(test.iter().map(|marker| format!("test:{marker}")));
        CorpusClassification::TestEphemeral
    } else if valid_shared {
        reasons.push("valid_typed_shared_scope_and_bounded_applicability".to_string());
        CorpusClassification::SharedCandidate
    } else if store == LogicalStore::BoundProject {
        reasons.push(
            "bound_project_store_row_remains_project_bound_without_valid_typed_shared_semantics"
                .to_string(),
        );
        if effective.knowledge_scope == WikiKnowledgeScopeV1::Project {
            reasons.push("typed_project_scope".to_string());
        } else if row.scope.eq_ignore_ascii_case("general") {
            reasons.push("legacy_scope_general_does_not_prove_shared_semantics".to_string());
        } else {
            reasons.push("shared_scope_missing_or_underspecified".to_string());
        }
        CorpusClassification::ProjectBound
    } else if effective.knowledge_scope == WikiKnowledgeScopeV1::Project {
        reasons.push("typed_project_scope".to_string());
        CorpusClassification::ProjectBound
    } else {
        reasons.push("ambiguous_or_underspecified_typed_metadata".to_string());
        reasons.push("path_or_category_alone_never_promotes_shared_knowledge".to_string());
        CorpusClassification::ManualReview
    };
    if row.metadata_parse_error.is_some() {
        reasons.push("invalid_metadata_json".to_string());
    }
    reasons.extend(
        effective
            .validation_issues
            .iter()
            .map(|issue| format!("effective_metadata:{issue}")),
    );
    reasons.sort();
    reasons.dedup();
    let evidence = typed_metadata_evidence(row, authority, operational, test);
    (classification, reasons, effective, evidence)
}

fn typed_metadata_evidence(
    row: &RawRow,
    authority_markers: Vec<String>,
    operational_markers: Vec<String>,
    test_markers: Vec<String>,
) -> TypedMetadataEvidence {
    TypedMetadataEvidence {
        raw_metadata_sha256: digest_string(
            &serde_json::to_string(&canonicalize_value(&row.metadata)).unwrap_or_default(),
        ),
        artifact_kind: metadata_value(&row.metadata, "artifact_kind"),
        knowledge_scope: metadata_value(&row.metadata, "knowledge_scope"),
        origin_projects: metadata_value(&row.metadata, "origin_projects"),
        applies_to: metadata_value(&row.metadata, "applies_to"),
        applicability_status: metadata_value(&row.metadata, "applicability_status"),
        lifecycle: metadata_value(&row.metadata, "lifecycle"),
        authority: metadata_value(&row.metadata, "authority"),
        review_receipt: metadata_value(&row.metadata, "review_receipt"),
        operational_markers,
        test_markers,
        authority_markers,
        migration_receipt: metadata_value(&row.metadata, RECEIPT_KEY),
    }
}

fn finalize_classifications(scans: &mut [StoreScan]) {
    for scan in scans.iter_mut() {
        for row in scan.rows.iter_mut() {
            if is_wiki_related(row) {
                let (classification, reasons, effective, _) =
                    classify_row(scan.spec.logical_store, row);
                row.classification = Some(classification);
                row.reasons = reasons;
                row.effective = Some(effective);
            }
        }
    }

    let mut paths = BTreeMap::<String, BTreeSet<String>>::new();
    for scan in scans.iter() {
        for row in scan.rows.iter().filter(|row| row.classification.is_some()) {
            paths
                .entry(row.normalized_path())
                .or_default()
                .insert(scan.spec.logical_store.reference().to_string());
        }
    }
    let duplicate_paths = paths
        .into_iter()
        .filter_map(|(path, stores)| (stores.len() > 1).then_some(path))
        .collect::<BTreeSet<_>>();
    for scan in scans.iter_mut() {
        for row in scan
            .rows
            .iter_mut()
            .filter(|row| row.classification.is_some())
        {
            let path = row.normalized_path();
            if duplicate_paths.contains(&path) {
                row.classification = Some(CorpusClassification::ManualReview);
                row.reasons
                    .push("duplicate_normalized_path_across_logical_stores".to_string());
                row.reasons
                    .push("safe_canonical_target_not_proven".to_string());
                row.reasons.sort();
                row.reasons.dedup();
            }
        }
    }
}

fn row_report(scan: &StoreScan, row: &RawRow) -> WikiCorpusRow {
    let effective = row.effective.clone().unwrap_or_else(|| {
        derive_effective_knowledge_artifact(&row.metadata, &row.path, &row.scope)
    });
    let evidence = typed_metadata_evidence(
        row,
        authority_markers(row),
        operational_markers(row),
        test_markers(row),
    );
    WikiCorpusRow {
        logical_store_ref: scan.spec.logical_store.reference().to_string(),
        id: row.id.clone(),
        path: row.path.clone(),
        normalized_path: row.normalized_path(),
        summary: row.summary.clone(),
        source: row.source.clone(),
        category: row.category.clone(),
        domain: row.domain.clone(),
        legacy_scope: row.scope.clone(),
        revision: row.revision,
        archived: row.archived,
        superseded_by: row.superseded_by.clone(),
        content_sha256: row.content_sha256(),
        classification: row
            .classification
            .unwrap_or(CorpusClassification::ManualReview),
        reasons: row.reasons.clone(),
        effective_typed_metadata: effective,
        typed_metadata_evidence: evidence,
    }
}

fn refresh_report(scan: &mut StoreScan) {
    let mut counts = RowCounts {
        total_memory_rows: scan.rows.len(),
        ..RowCounts::default()
    };
    let mut rows = scan
        .rows
        .iter()
        .filter(|row| row.classification.is_some())
        .map(|row| {
            let classification = row
                .classification
                .unwrap_or(CorpusClassification::ManualReview);
            counts.wiki_related_rows += 1;
            counts.classifications.increment(classification);
            row_report(scan, row)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.normalized_path.cmp(&right.normalized_path))
            .then_with(|| left.path.cmp(&right.path))
    });
    scan.report.counts = counts;
    scan.report.rows = rows;
}

fn inventory_store(spec: StoreSpec) -> StoreScan {
    let logical_store_ref = spec.logical_store.reference().to_string();
    let existence = spec
        .addressed_path
        .as_ref()
        .is_some_and(|path| fs::metadata(path).is_ok());
    let report = StoreReport {
        logical_store_ref,
        semantic_role: spec.logical_store.semantic_role().to_string(),
        addressed_path: spec
            .addressed_path
            .as_ref()
            .map(|path| path.display().to_string()),
        existence,
        resolved_path: None,
        canonical_path: None,
        open_path: None,
        open_path_basis: None,
        physical_identity: None,
        read_failure: None,
        stored_schema: None,
        expected_schema: EXPECTED_SCHEMA_VERSION,
        counts: RowCounts::default(),
        rows: Vec::new(),
    };
    let mut scan = StoreScan {
        spec,
        physical: None,
        rows: Vec::new(),
        report,
        row_digest: String::new(),
        vector_table_present: false,
    };
    let Some(addressed_path) = scan.spec.addressed_path.clone() else {
        scan.report.read_failure = Some(ReadFailure {
            kind: InventoryFailureKind::NotFound,
            message: scan
                .spec
                .resolution_error
                .clone()
                .unwrap_or_else(|| "logical store has no resolved path".to_string()),
        });
        return scan;
    };

    let physical_inventory = classify_paths([addressed_path]);
    if let Some(unresolved) = physical_inventory.unresolved_paths.first() {
        scan.report.read_failure = Some(ReadFailure {
            kind: unresolved.failure_kind,
            message: unresolved.error.clone(),
        });
        return scan;
    }
    let Some(mut physical) = physical_inventory.stores.into_iter().next() else {
        scan.report.read_failure = Some(ReadFailure {
            kind: InventoryFailureKind::Other,
            message: "physical identity inventory returned no store".to_string(),
        });
        return scan;
    };
    scan.report.resolved_path = Some(physical.canonical_path.clone());
    scan.report.canonical_path = Some(physical.canonical_path.clone());
    scan.report.open_path = Some(physical.open_path.clone());
    scan.report.open_path_basis = Some(physical.open_path_basis);
    scan.report.physical_identity = Some(physical.clone());
    let open_path = PathBuf::from(&physical.open_path);
    let conn = match open_preview_connection(&open_path) {
        Ok(conn) => conn,
        Err(error) => {
            let kind = classify_open_failure(&error);
            physical.open_failure_kind = Some(kind);
            scan.report.physical_identity = Some(physical.clone());
            scan.report.read_failure = Some(ReadFailure {
                kind,
                message: error.to_string(),
            });
            scan.physical = Some(physical);
            return scan;
        }
    };
    let stored_schema = match read_schema_version(&conn) {
        Ok(version) => version,
        Err(error) => {
            let kind = classify_memory_failure(&error);
            physical.open_failure_kind = Some(kind);
            scan.report.physical_identity = Some(physical.clone());
            scan.report.read_failure = Some(ReadFailure {
                kind,
                message: error.to_string(),
            });
            scan.physical = Some(physical);
            return scan;
        }
    };
    scan.report.stored_schema = Some(stored_schema);
    let (rows, _, vector_table_present) = match load_rows(&conn) {
        Ok(result) => result,
        Err(error) => {
            let kind = classify_open_failure(&error);
            physical.open_failure_kind = Some(kind);
            scan.report.physical_identity = Some(physical.clone());
            scan.report.read_failure = Some(ReadFailure {
                kind,
                message: error,
            });
            scan.physical = Some(physical);
            return scan;
        }
    };
    scan.rows = rows;
    scan.vector_table_present = vector_table_present;
    scan.row_digest = row_digest(&scan.rows, vector_table_present);
    scan.physical = Some(physical);
    scan
}

fn store_specs(global_db: &Path, project_db: Option<&Path>, app_home: &Path) -> Vec<StoreSpec> {
    let shared =
        match MemoryServer::resolve_existing_named_project_db_path_in_home("wiki", app_home) {
            Ok(Some(path)) => StoreSpec {
                logical_store: LogicalStore::SharedWiki,
                addressed_path: Some(path),
                resolution_error: None,
            },
            Ok(None) => StoreSpec {
                logical_store: LogicalStore::SharedWiki,
                addressed_path: None,
                resolution_error: Some("logical named store 'wiki' is absent".to_string()),
            },
            Err(error) => StoreSpec {
                logical_store: LogicalStore::SharedWiki,
                addressed_path: None,
                resolution_error: Some(error),
            },
        };
    vec![
        StoreSpec {
            logical_store: LogicalStore::BoundProject,
            addressed_path: project_db.map(Path::to_path_buf),
            resolution_error: project_db
                .is_none()
                .then(|| "no bound project store was supplied".to_string()),
        },
        shared,
        StoreSpec {
            logical_store: LogicalStore::LegacyGlobal,
            addressed_path: Some(global_db.to_path_buf()),
            resolution_error: None,
        },
    ]
}

fn find_scan<'a>(scans: &'a [StoreScan], reference: &str) -> Result<&'a StoreScan, String> {
    scans
        .iter()
        .find(|scan| scan.spec.logical_store.reference() == reference)
        .ok_or_else(|| format!("plan references unknown logical store '{reference}'"))
}

fn plan_item_material(item: &PlanItem) -> String {
    serde_json::to_string(&canonicalize_value(
        &serde_json::to_value(item).unwrap_or(Value::Null),
    ))
    .unwrap_or_default()
}

fn replay_identity_for(
    source_store_ref: &str,
    source_physical_id: &str,
    row: &RawRow,
    vector_table_present: bool,
) -> String {
    let material = json!({
        "version": "wiki-corpus-replay-v2",
        "source_store_ref": source_store_ref,
        "source_physical_id": source_physical_id,
        "source_id": row.id,
        "source_path": row.path,
        "normalized_path": row.normalized_path(),
        "source_revision": row.revision,
        "source_content_sha256": row.content_sha256(),
        "source_copy_identity_sha256": row.copy_identity_sha256(),
        "source_superseded_by": row.superseded_by,
        "source_vector": row.vector_fingerprint(vector_table_present),
    });
    digest_string(&serde_json::to_string(&canonicalize_value(&material)).unwrap_or_default())
}

fn build_plan_item(source: &StoreScan, row: &RawRow, target: &StoreScan) -> PlanItem {
    let source_store_ref = source.spec.logical_store.reference().to_string();
    let source_physical_id = source
        .physical
        .as_ref()
        .map(|physical| physical.physical_id.clone())
        .unwrap_or_default();
    let replay_identity = replay_identity_for(
        &source_store_ref,
        &source_physical_id,
        row,
        source.vector_table_present,
    );
    let is_shared_source = source.spec.logical_store == LogicalStore::SharedWiki;
    PlanItem {
        action: if is_shared_source {
            "reclassify_in_place".to_string()
        } else {
            "copy_to_shared_and_supersede".to_string()
        },
        source_store_ref,
        source_physical_id,
        source_id: row.id.clone(),
        source_path: row.path.clone(),
        normalized_path: row.normalized_path(),
        source_revision: row.revision,
        source_content_sha256: row.content_sha256(),
        source_copy_identity_sha256: row.copy_identity_sha256(),
        source_valid_until: row.valid_until.clone(),
        source_superseded_by: row.superseded_by.clone(),
        source_vector: row.vector_fingerprint(source.vector_table_present),
        replay_identity: replay_identity.clone(),
        target_store_ref: LogicalStore::SharedWiki.reference().to_string(),
        target_physical_id: target
            .physical
            .as_ref()
            .map(|physical| physical.physical_id.clone())
            .unwrap_or_default(),
        target_id: (!is_shared_source).then(|| format!("wiki-corpus:{replay_identity}")),
    }
}

fn plan_id_for(
    version: &str,
    store_fingerprints: &[PlanStoreFingerprint],
    items: &[PlanItem],
) -> String {
    let mut fingerprints = store_fingerprints.to_vec();
    fingerprints.sort_by(|left, right| left.logical_store_ref.cmp(&right.logical_store_ref));
    let mut items = items.to_vec();
    items.sort_by(|left, right| {
        left.source_store_ref
            .cmp(&right.source_store_ref)
            .then_with(|| left.source_id.cmp(&right.source_id))
            .then_with(|| left.replay_identity.cmp(&right.replay_identity))
    });
    let material = json!({
        "version": version,
        "store_fingerprints": fingerprints,
        "items": items,
    });
    digest_string(&serde_json::to_string(&canonicalize_value(&material)).unwrap_or_default())
}

fn build_plan(scans: &[StoreScan]) -> Result<WikiCorpusPlan, String> {
    let target = find_scan(scans, LogicalStore::SharedWiki.reference())?;
    let mut items = Vec::new();
    for scan in scans {
        if scan.physical.is_none() {
            continue;
        }
        for row in scan
            .rows
            .iter()
            .filter(|row| row.classification == Some(CorpusClassification::SharedCandidate))
        {
            items.push(build_plan_item(scan, row, target));
        }
    }
    items.sort_by(|left, right| {
        left.source_store_ref
            .cmp(&right.source_store_ref)
            .then_with(|| left.source_id.cmp(&right.source_id))
            .then_with(|| left.replay_identity.cmp(&right.replay_identity))
    });
    let store_fingerprints = scans
        .iter()
        .filter_map(StoreScan::fingerprint)
        .collect::<Vec<_>>();
    let plan_id = plan_id_for(REPORT_VERSION, &store_fingerprints, &items);
    Ok(WikiCorpusPlan {
        version: REPORT_VERSION.to_string(),
        plan_id,
        store_fingerprints,
        items,
    })
}

fn receipt_value(item: &PlanItem, plan_id: &str, phase: &str) -> Value {
    let phase = match phase {
        "planned" => MigrationPhase::Planned,
        "target_copied" => MigrationPhase::TargetCopied,
        "target_noncanonical" => MigrationPhase::TargetNoncanonical,
        "source_receipted" => MigrationPhase::SourceReceipted,
        "source_superseded" => MigrationPhase::SourceSuperseded,
        "reclassified" => MigrationPhase::Reclassified,
        other => panic!("unsupported Wiki corpus migration phase {other}"),
    };
    serde_json::to_value(MigrationReceipt {
        kind: RECEIPT_KEY.to_string(),
        version: RECEIPT_VERSION,
        plan_id: plan_id.to_string(),
        phase,
        item: item.clone(),
    })
    .expect("Wiki corpus migration receipt is serializable")
}

fn parse_migration_receipt(row: &RawRow) -> Result<Option<MigrationReceipt>, String> {
    let Some(receipt) = row.metadata.get(RECEIPT_KEY) else {
        return Ok(None);
    };
    let receipt: MigrationReceipt = serde_json::from_value(receipt.clone()).map_err(|error| {
        format!(
            "invalid Wiki corpus migration receipt on {}: {error}",
            row.id
        )
    })?;
    if receipt.kind != RECEIPT_KEY || receipt.version != RECEIPT_VERSION {
        return Err(format!(
            "unsupported Wiki corpus migration receipt on {}",
            row.id
        ));
    }
    Ok(Some(receipt))
}

fn receipt_matches(row: &RawRow, item: &PlanItem, plan_id: &str, phases: &[&str]) -> bool {
    let Ok(Some(receipt)) = parse_migration_receipt(row) else {
        return false;
    };
    receipt.plan_id == plan_id && receipt.item == *item && phases.contains(&receipt.phase.as_str())
}

fn metadata_with_receipt(row: &RawRow, receipt: Value) -> Result<Value, String> {
    let mut metadata = row
        .metadata
        .as_object()
        .cloned()
        .ok_or_else(|| "migration requires object-shaped metadata".to_string())?;
    metadata.insert(RECEIPT_KEY.to_string(), receipt);
    Ok(Value::Object(metadata))
}

#[cfg(test)]
fn validate_target_occupant(
    item: &PlanItem,
    plan_id: &str,
    _source: &RawRow,
    target: &StoreScan,
) -> Result<(), String> {
    let Some(target_id) = item.target_id.as_deref() else {
        return Ok(());
    };
    let Some(occupant) = target.raw_row(target_id) else {
        return Ok(());
    };
    if occupant.copy_identity_sha256() != item.source_copy_identity_sha256
        || occupant.archived
        || occupant.superseded_by.is_some()
        || !receipt_matches(occupant, item, plan_id, &["target_copied"])
    {
        return Err(format!(
            "deterministic target occupant collision: {target_id} already exists in {} but its canonical content or migration receipt does not match",
            target.spec.logical_store.reference()
        ));
    }
    Ok(())
}

fn plan_row_fingerprint_from_item(item: &PlanItem) -> PlanRowFingerprint {
    PlanRowFingerprint {
        id: item.source_id.clone(),
        revision: item.source_revision,
        content_sha256: item.source_content_sha256.clone(),
        superseded_by: item.source_superseded_by.clone(),
        vector: item.source_vector.clone(),
    }
}

fn expected_item_for_live_source(
    source: &StoreScan,
    row: &RawRow,
    target: &StoreScan,
    source_revision: i64,
    source_valid_until: Option<String>,
    source_superseded_by: Option<String>,
) -> PlanItem {
    let mut identity_row = row.clone();
    identity_row.revision = source_revision;
    identity_row.valid_until = source_valid_until;
    identity_row.superseded_by = source_superseded_by;
    let mut expected = build_plan_item(source, &identity_row, target);
    expected.replay_identity = replay_identity_for(
        &expected.source_store_ref,
        &expected.source_physical_id,
        &identity_row,
        source.vector_table_present,
    );
    if expected.action == "copy_to_shared_and_supersede" {
        expected.target_id = Some(format!("wiki-corpus:{}", expected.replay_identity));
    }
    expected
}

fn superseded_validity_matches_plan(row: &RawRow, item: &PlanItem) -> bool {
    match item.source_valid_until.as_ref() {
        Some(expected) => row.valid_until.as_ref() == Some(expected),
        None => row.valid_until.is_some(),
    }
}

fn source_content_matches_plan(
    row: &RawRow,
    item: &PlanItem,
    supersession_is_proven: bool,
) -> bool {
    if !supersession_is_proven {
        return row.content_sha256() == item.source_content_sha256;
    }
    if !superseded_validity_matches_plan(row, item) {
        return false;
    }
    let mut original = row.clone();
    original.valid_until.clone_from(&item.source_valid_until);
    original.content_sha256() == item.source_content_sha256
}

fn validate_live_source_item(
    item: &PlanItem,
    plan_id: &str,
    source: &StoreScan,
    row: &RawRow,
    target: &StoreScan,
    receipt: Option<&MigrationReceipt>,
) -> Result<(), String> {
    let source_physical = source.physical.as_ref().ok_or_else(|| {
        format!(
            "source store {} has no physical identity",
            item.source_store_ref
        )
    })?;
    let target_physical = target
        .physical
        .as_ref()
        .ok_or_else(|| "logical shared target has no physical identity".to_string())?;
    let expected = expected_item_for_live_source(
        source,
        row,
        target,
        item.source_revision,
        item.source_valid_until.clone(),
        item.source_superseded_by.clone(),
    );
    if item != &expected {
        return Err(format!(
            "Wiki corpus plan item for {}:{} does not match the canonical item rederived from the current source",
            item.source_store_ref, item.source_id
        ));
    }
    if item.source_physical_id != source_physical.physical_id
        || item.target_physical_id != target_physical.physical_id
    {
        return Err(format!(
            "Wiki corpus plan item for {}:{} has stale physical identity",
            item.source_store_ref, item.source_id
        ));
    }
    if row.vector_fingerprint(source.vector_table_present) != item.source_vector {
        return Err(format!(
            "Wiki corpus source vector state changed for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    if item.source_vector.row_present && !target.vector_table_present {
        return Err(format!(
            "target {} cannot preserve the source vector for {}:{}",
            item.target_store_ref, item.source_store_ref, item.source_id
        ));
    }

    match receipt {
        None => {
            if row.revision != item.source_revision
                || row.valid_until != item.source_valid_until
                || row.superseded_by != item.source_superseded_by
            {
                return Err(format!(
                    "source revision/lifecycle changed after preview for {}:{}",
                    item.source_store_ref, item.source_id
                ));
            }
        }
        Some(receipt) => {
            if receipt.plan_id != plan_id || receipt.item != *item {
                return Err(format!(
                    "Wiki corpus migration receipt does not match supplied plan for {}:{}",
                    item.source_store_ref, item.source_id
                ));
            }
            match receipt.phase {
                MigrationPhase::Reclassified => {
                    if row.revision != item.source_revision + 1
                        || row.valid_until != item.source_valid_until
                        || row.superseded_by != item.source_superseded_by
                    {
                        return Err(format!(
                            "Wiki corpus migration state is inconsistent for {}:{}",
                            item.source_store_ref, item.source_id
                        ));
                    }
                }
                MigrationPhase::SourceReceipted => {
                    let receipt_only = row.revision == item.source_revision + 1
                        && row.valid_until == item.source_valid_until
                        && row.superseded_by == item.source_superseded_by;
                    let supersession_observed = row.revision == item.source_revision + 2
                        && superseded_validity_matches_plan(row, item)
                        && row.superseded_by == item.target_id;
                    if !receipt_only && !supersession_observed {
                        return Err(format!(
                            "Wiki corpus migration state is inconsistent for {}:{}",
                            item.source_store_ref, item.source_id
                        ));
                    }
                }
                MigrationPhase::SourceSuperseded => {
                    if item.action != "copy_to_shared_and_supersede"
                        || !(item.source_revision + 1..=item.source_revision + 3)
                            .contains(&row.revision)
                        || !superseded_validity_matches_plan(row, item)
                        || row.superseded_by != item.target_id
                    {
                        return Err(format!(
                            "Wiki corpus source_superseded state is not proven for {}:{}",
                            item.source_store_ref, item.source_id
                        ));
                    }
                }
                MigrationPhase::Planned
                | MigrationPhase::TargetCopied
                | MigrationPhase::TargetNoncanonical => {
                    return Err(format!(
                        "invalid source migration phase for {}:{}",
                        item.source_store_ref, item.source_id
                    ));
                }
            }
        }
    }

    if item.action == "copy_to_shared_and_supersede"
        && row.superseded_by.is_some()
        && row.superseded_by != item.target_id
    {
        return Err(format!(
            "source row {}:{} is already superseded by another target",
            item.source_store_ref, item.source_id
        ));
    }
    Ok(())
}

fn validate_target_receipt(
    item: &PlanItem,
    plan_id: &str,
    target: &StoreScan,
    row: &RawRow,
    source: &StoreScan,
    source_row: &RawRow,
) -> Result<(), String> {
    let receipt = parse_migration_receipt(row)?
        .ok_or_else(|| format!("target {} has no migration receipt", row.id))?;
    if receipt.plan_id != plan_id
        || receipt.phase != MigrationPhase::TargetCopied
        || receipt.item != *item
        || item.target_store_ref != target.spec.logical_store.reference()
        || item.target_id.as_deref() != Some(row.id.as_str())
    {
        return Err(format!(
            "target migration receipt does not match supplied plan for {}",
            row.id
        ));
    }
    validate_live_source_item(
        item,
        plan_id,
        source,
        source_row,
        target,
        parse_migration_receipt(source_row)?.as_ref(),
    )?;
    if row.copy_identity_sha256() != item.source_copy_identity_sha256
        || row.archived
        || row.superseded_by.is_some()
    {
        return Err(format!(
            "target {} immutable copy identity or canonical lifecycle changed",
            row.id
        ));
    }
    Ok(())
}

fn validate_noncanonical_target_receipt(row: &RawRow, store: LogicalStore) -> Result<(), String> {
    let receipt = parse_migration_receipt(row)?
        .ok_or_else(|| format!("noncanonical target {} has no receipt", row.id))?;
    let item = &receipt.item;
    if receipt.phase != MigrationPhase::TargetNoncanonical
        || item.target_store_ref != store.reference()
        || item.target_id.as_deref() != Some(row.id.as_str())
        || row.copy_identity_sha256() != item.source_copy_identity_sha256
        || !row.archived
    {
        return Err(format!(
            "noncanonical target {} has invalid provenance or lifecycle",
            row.id
        ));
    }
    Ok(())
}

fn rederive_items(scans: &[StoreScan], plan_id: &str) -> Result<Vec<PlanItem>, String> {
    let target = find_scan(scans, LogicalStore::SharedWiki.reference())?;
    let mut items = BTreeMap::<String, PlanItem>::new();
    for scan in scans {
        for row in &scan.rows {
            let receipt = parse_migration_receipt(row)?;
            if receipt
                .as_ref()
                .is_some_and(|receipt| receipt.phase == MigrationPhase::TargetNoncanonical)
            {
                validate_noncanonical_target_receipt(row, scan.spec.logical_store)?;
                continue;
            }
            let is_target_receipt = receipt.as_ref().is_some_and(|receipt| {
                receipt.phase == MigrationPhase::TargetCopied
                    && receipt.item.target_store_ref == scan.spec.logical_store.reference()
                    && receipt.item.target_id.as_deref() == Some(row.id.as_str())
                    && receipt.item.source_store_ref != scan.spec.logical_store.reference()
            });
            let is_source_receipt = receipt.as_ref().is_some_and(|receipt| {
                receipt.item.source_store_ref == scan.spec.logical_store.reference()
                    && receipt.item.source_id == row.id
            });
            if receipt.is_none() && row.classification.is_none() {
                continue;
            }
            if receipt.is_some()
                && !is_target_receipt
                && !is_source_receipt
                && row.classification != Some(CorpusClassification::SharedCandidate)
            {
                return Err(format!(
                    "migration receipt is attached to a non-candidate row {}",
                    row.id
                ));
            }
            if receipt.is_none()
                && row.classification != Some(CorpusClassification::SharedCandidate)
            {
                continue;
            }
            let item = if let Some(receipt) = receipt.as_ref() {
                if receipt.plan_id != plan_id {
                    return Err(format!(
                        "migration receipt on {} belongs to a different plan",
                        row.id
                    ));
                }
                let item = &receipt.item;
                if item.source_store_ref == scan.spec.logical_store.reference()
                    && item.source_id == row.id
                {
                    let source = scan;
                    validate_live_source_item(item, plan_id, source, row, target, Some(receipt))?;
                    item.clone()
                } else if receipt.phase == MigrationPhase::TargetCopied
                    && item.target_store_ref == scan.spec.logical_store.reference()
                    && item.target_id.as_deref() == Some(row.id.as_str())
                {
                    let source = find_scan(scans, &item.source_store_ref)?;
                    let source_row = source.raw_row(&item.source_id).ok_or_else(|| {
                        format!(
                            "target receipt {} references missing source {}:{}",
                            row.id, item.source_store_ref, item.source_id
                        )
                    })?;
                    validate_target_receipt(item, plan_id, scan, row, source, source_row)?;
                    item.clone()
                } else {
                    return Err(format!(
                        "migration receipt on {} is not bound to its physical row",
                        row.id
                    ));
                }
            } else {
                let item = build_plan_item(scan, row, target);
                validate_live_source_item(&item, plan_id, scan, row, target, None)?;
                item
            };
            let key = plan_item_material(&item);
            if let Some(existing) = items.insert(key, item.clone()) {
                if existing != item {
                    return Err(
                        "current Wiki corpus state rederives conflicting plan items".to_string()
                    );
                }
            }
        }
    }
    Ok(items.into_values().collect())
}

fn rederive_original_store_fingerprint(
    scans: &[StoreScan],
    scan: &StoreScan,
    plan_id: &str,
) -> Result<PlanStoreFingerprint, String> {
    let target = find_scan(scans, LogicalStore::SharedWiki.reference())?;
    let mut rows = Vec::new();
    for row in &scan.rows {
        if let Some(receipt) = parse_migration_receipt(row)? {
            if receipt.phase == MigrationPhase::TargetNoncanonical {
                validate_noncanonical_target_receipt(row, scan.spec.logical_store)?;
                if receipt.plan_id != plan_id {
                    rows.push(plan_row_fingerprint(row, scan.vector_table_present));
                }
                continue;
            }
            if receipt.plan_id != plan_id {
                return Err(format!(
                    "migration receipt on {} belongs to another plan",
                    row.id
                ));
            }
            let item = &receipt.item;
            if receipt.phase == MigrationPhase::TargetCopied
                && item.target_store_ref == scan.spec.logical_store.reference()
                && item.target_id.as_deref() == Some(row.id.as_str())
                && item.source_store_ref != scan.spec.logical_store.reference()
            {
                let source = find_scan(scans, &item.source_store_ref)?;
                let source_row = source.raw_row(&item.source_id).ok_or_else(|| {
                    format!("target receipt {} references missing source", row.id)
                })?;
                validate_target_receipt(item, plan_id, scan, row, source, source_row)?;
                continue;
            }
            if item.source_store_ref == scan.spec.logical_store.reference()
                && item.source_id == row.id
            {
                validate_live_source_item(item, plan_id, scan, row, target, Some(&receipt))?;
                rows.push(plan_row_fingerprint_from_item(item));
                continue;
            }
            return Err(format!(
                "migration receipt on {} is not in this store's source set",
                row.id
            ));
        }
        rows.push(plan_row_fingerprint(row, scan.vector_table_present));
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    let row_digest = row_digest_values(
        rows.iter()
            .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
            .collect(),
    );
    let physical = scan.physical.as_ref().ok_or_else(|| {
        format!(
            "store {} has no physical identity",
            scan.spec.logical_store.reference()
        )
    })?;
    let schema = scan.report.stored_schema.ok_or_else(|| {
        format!(
            "store {} has no schema",
            scan.spec.logical_store.reference()
        )
    })?;
    Ok(PlanStoreFingerprint {
        logical_store_ref: scan.spec.logical_store.reference().to_string(),
        physical_id: physical.physical_id.clone(),
        canonical_path: physical.canonical_path.clone(),
        schema,
        total_memory_rows: rows.len(),
        row_digest,
        vector_table_present: scan.vector_table_present,
        rows,
    })
}

fn validate_store_fingerprints(scans: &[StoreScan], plan: &WikiCorpusPlan) -> Result<(), String> {
    let mut expected = plan.store_fingerprints.clone();
    expected.sort_by(|left, right| left.logical_store_ref.cmp(&right.logical_store_ref));
    let mut current = Vec::new();
    for scan in scans {
        if scan.physical.is_some() {
            current.push(rederive_original_store_fingerprint(
                scans,
                scan,
                &plan.plan_id,
            )?);
        }
    }
    current.sort_by(|left, right| left.logical_store_ref.cmp(&right.logical_store_ref));
    if expected.len() != current.len() {
        return Err("Wiki corpus store fingerprint set changed since preview".to_string());
    }

    for (expected_store, current_store) in expected.iter().zip(current.iter()) {
        if expected_store.logical_store_ref != current_store.logical_store_ref
            || expected_store.physical_id != current_store.physical_id
            || expected_store.canonical_path != current_store.canonical_path
            || expected_store.schema != current_store.schema
            || expected_store.vector_table_present != current_store.vector_table_present
        {
            return Err(format!(
                "Wiki corpus store identity or schema changed since preview for {}",
                expected_store.logical_store_ref
            ));
        }

        if expected_store.total_memory_rows != expected_store.rows.len()
            || expected_store.row_digest
                != row_digest_values(
                    expected_store
                        .rows
                        .iter()
                        .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
                        .collect(),
                )
        {
            return Err(format!(
                "Wiki corpus preview fingerprint is internally inconsistent for {}",
                expected_store.logical_store_ref
            ));
        }
        if current_store != expected_store {
            return Err(format!(
                "Wiki corpus row or vector fingerprint changed since preview for {}",
                expected_store.logical_store_ref
            ));
        }
    }
    Ok(())
}

fn validate_plan(scans: &[StoreScan], plan: &WikiCorpusPlan) -> Result<(), String> {
    if plan.version != REPORT_VERSION {
        return Err(format!(
            "unsupported Wiki corpus plan version '{}'",
            plan.version
        ));
    }
    let expected_plan_id = plan_id_for(&plan.version, &plan.store_fingerprints, &plan.items);
    if expected_plan_id != plan.plan_id {
        return Err("Wiki corpus plan id does not match its canonical contents".to_string());
    }
    validate_store_fingerprints(scans, plan)?;
    let mut expected_items = rederive_items(scans, &plan.plan_id)?;
    let mut supplied_items = plan.items.clone();
    expected_items.sort_by_key(plan_item_material);
    supplied_items.sort_by_key(plan_item_material);
    if expected_items != supplied_items {
        return Err(
            "supplied Wiki corpus plan items do not match canonical rederivation from current stores"
                .to_string(),
        );
    }
    Ok(())
}

fn load_plan(path: &Path) -> Result<WikiCorpusPlan, String> {
    regular_non_symlink_metadata(path)?;
    let raw = read_file_no_follow(path)?;
    let value: Value = serde_json::from_slice(&raw)
        .map_err(|error| format!("Wiki corpus plan {} is not JSON: {error}", path.display()))?;
    if let Some(plan) = value.get("plan") {
        serde_json::from_value(plan.clone())
            .map_err(|error| format!("invalid embedded Wiki corpus plan: {error}"))
    } else {
        serde_json::from_value(value).map_err(|error| format!("invalid Wiki corpus plan: {error}"))
    }
}

fn validate_apply_inventory(scans: &[StoreScan]) -> Result<(), String> {
    for scan in scans {
        let report = &scan.report;
        if !report.existence {
            return Err(format!(
                "apply refuses missing involved store {} ({})",
                report.logical_store_ref,
                report
                    .read_failure
                    .as_ref()
                    .map(|failure| failure.message.as_str())
                    .unwrap_or("path is absent")
            ));
        }
        if let Some(failure) = &report.read_failure {
            return Err(format!(
                "apply refuses unreadable store {}: {}",
                report.logical_store_ref, failure.message
            ));
        }
        if report.stored_schema != Some(EXPECTED_SCHEMA_VERSION) {
            return Err(format!(
                "apply refuses schema mismatch in {}: stored {:?}, expected {}",
                report.logical_store_ref, report.stored_schema, EXPECTED_SCHEMA_VERSION
            ));
        }
        let physical = scan.physical.as_ref().ok_or_else(|| {
            format!(
                "apply refuses store {} without physical identity",
                report.logical_store_ref
            )
        })?;
        if physical.mutation_state != PhysicalStoreMutationState::Authorized
            || physical.mutation_authority.is_none()
        {
            return Err(format!(
                "apply refuses ambiguous or unavailable physical mutation identity for {} ({})",
                report.logical_store_ref,
                physical.mutation_state.as_str()
            ));
        }
    }
    let addressed_paths = scans
        .iter()
        .filter_map(|scan| scan.spec.addressed_path.clone())
        .collect::<Vec<_>>();
    let bindings = physical_db_bindings_for_paths(&addressed_paths)
        .map_err(|error| format!("apply refuses ambiguous physical mutation identity: {error}"))?;
    for (scan, binding) in scans
        .iter()
        .filter(|scan| scan.spec.addressed_path.is_some())
        .zip(bindings.iter())
    {
        let physical = scan
            .physical
            .as_ref()
            .ok_or_else(|| "apply inventory lost its physical identity".to_string())?;
        let addressed = scan
            .spec
            .addressed_path
            .as_ref()
            .expect("filtered to addressed stores");
        let expected_primary = physical.primary_path == addressed.display().to_string();
        if binding.physical_id != physical.physical_id
            || binding.is_primary_alias != expected_primary
        {
            return Err(format!(
                "apply refuses physical binding changed since inventory for {}",
                scan.spec.logical_store.reference()
            ));
        }
    }
    Ok(())
}

fn reinventory_apply_scans(scans: &[StoreScan]) -> Vec<StoreScan> {
    let mut current = scans
        .iter()
        .map(|scan| inventory_store(scan.spec.clone()))
        .collect::<Vec<_>>();
    finalize_classifications(&mut current);
    for scan in current.iter_mut() {
        refresh_report(scan);
    }
    current
}

/// Hold a writer reservation on every distinct physical store while the
/// completed no-op proof is refreshed. SQLite has no transaction spanning the
/// separate corpus databases, so the stores are acquired in deterministic
/// physical-id order; the final inventory is taken only after all reservations
/// are held. A failed acquisition therefore refuses the no-op rather than
/// returning from an unstable snapshot.
struct CompletedNoOpLocks {
    stores: Vec<MemoryStore>,
}

impl CompletedNoOpLocks {
    fn acquire(scans: &[StoreScan]) -> Result<Self, String> {
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

fn check_authority(scan: &StoreScan, target: Option<&Path>) -> Result<(), String> {
    let physical = scan
        .physical
        .as_ref()
        .ok_or_else(|| "physical identity disappeared before mutation".to_string())?;
    let authority = physical
        .mutation_authority
        .as_ref()
        .ok_or_else(|| "mutation authority disappeared before mutation".to_string())?;
    authority.revalidate_for_mutation(target)
}

fn open_apply_store(scan: &StoreScan) -> Result<MemoryStore, String> {
    let path = scan
        .physical
        .as_ref()
        .ok_or_else(|| "apply store has no physical identity".to_string())?
        .primary_path
        .clone();
    MemoryStore::open_existing_read_write(&path).map_err(|error| error.to_string())
}

fn verify_opened_apply_store(
    store: &MemoryStore,
    logical_path: &Path,
    role: &str,
) -> Result<(), String> {
    store
        .verify_opened_physical_db_identity(logical_path)
        .map_err(|error| format!("{role} store detached from logical path: {error}"))
}

fn verify_copy_store_identities(
    source_store: &MemoryStore,
    source_path: &Path,
    target_store: &MemoryStore,
    target_path: &Path,
) -> Result<(), String> {
    verify_opened_apply_store(source_store, source_path, "source")?;
    verify_opened_apply_store(target_store, target_path, "target")
}

fn maybe_swap_opened_store_path(
    race_hook: &mut Option<CorpusRaceHook>,
    store: LogicalStore,
    logical_path: &Path,
) -> Result<(), String> {
    let replacement_path = match race_hook.as_ref() {
        Some(CorpusRaceHook::SwapOpenedStorePath {
            store: hook_store,
            replacement_path,
        }) if *hook_store == store => replacement_path.clone(),
        _ => return Ok(()),
    };
    race_hook.take();
    atomic_exchange_paths(logical_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject opened-store path replacement for {}: {error}",
            logical_path.display()
        )
    })
}

fn maybe_swap_logical_path_after_inventory(
    scans: &[StoreScan],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let (store, replacement_path) = match race_hook.as_ref() {
        Some(CorpusRaceHook::SwapLogicalPathAfterInventory {
            store,
            replacement_path,
        }) => (*store, replacement_path.clone()),
        _ => return Ok(()),
    };
    let logical_path = scans
        .iter()
        .find(|scan| scan.spec.logical_store == store)
        .and_then(|scan| scan.spec.addressed_path.as_deref())
        .ok_or_else(|| format!("race hook cannot find logical store {}", store.reference()))?;
    race_hook.take();
    atomic_exchange_paths(logical_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject post-inventory path replacement for {}: {error}",
            logical_path.display()
        )
    })
}

fn maybe_swap_reclassification_after_receipt_read(
    item: &PlanItem,
    logical_path: &Path,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let replacement_path = match race_hook.as_ref() {
        Some(CorpusRaceHook::SwapReclassificationPathAfterReceiptRead {
            source_id,
            replacement_path,
        }) if source_id == &item.source_id => replacement_path.clone(),
        _ => return Ok(()),
    };
    race_hook.take();
    atomic_exchange_paths(logical_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject reclassification path replacement for {}: {error}",
            logical_path.display()
        )
    })
}

fn maybe_swap_logical_path_before_completed_return(
    scans: &[StoreScan],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let (store, replacement_path) = match race_hook.as_ref() {
        Some(CorpusRaceHook::SwapLogicalPathBeforeCompletedReturn {
            store,
            replacement_path,
        }) => (*store, replacement_path.clone()),
        _ => return Ok(()),
    };
    let logical_path = scans
        .iter()
        .find(|scan| scan.spec.logical_store == store)
        .and_then(|scan| scan.spec.addressed_path.as_deref())
        .ok_or_else(|| format!("race hook cannot find logical store {}", store.reference()))?;
    race_hook.take();
    atomic_exchange_paths(logical_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject completed-return path replacement for {}: {error}",
            logical_path.display()
        )
    })
}

fn maybe_archive_target_before_completed_return(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::ArchiveTargetBeforeCompletedReturn { source_id }) => source_id.clone(),
        _ => return Ok(()),
    };
    let item = plan
        .items
        .iter()
        .find(|item| item.source_id == source_id && item.action == "copy_to_shared_and_supersede")
        .ok_or_else(|| format!("race hook cannot find copy item for source {source_id}"))?;
    let target_id = item
        .target_id
        .as_deref()
        .ok_or_else(|| "race hook copy item has no target id".to_string())?;
    let target_scan = find_scan(scans, &item.target_store_ref)?;
    let target_path = target_scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook target has no addressed path".to_string())?;
    race_hook.take();
    let target_store = MemoryStore::open_existing_read_write(&target_path.display().to_string())
        .map_err(|error| error.to_string())?;
    let target_entry = target_store
        .get_with_options(target_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("race hook target {target_id} disappeared"))?;
    if !target_store
        .archive_memory_if_revision(target_id, target_entry.revision)
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "race hook could not archive target {target_id} at revision {}",
            target_entry.revision
        ));
    }
    Ok(())
}

fn maybe_mutate_reclassification_after_plan_validation(
    scans: &[StoreScan],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::MutateReclassificationAfterPlanValidation { source_id }) => {
            source_id.clone()
        }
        _ => return Ok(()),
    };
    let scan = scans
        .iter()
        .find(|scan| scan.raw_row(&source_id).is_some())
        .ok_or_else(|| format!("race hook cannot find reclassification row {source_id}"))?;
    let logical_path = scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook reclassification store has no addressed path".to_string())?;
    let revision = scan
        .raw_row(&source_id)
        .expect("row selected above")
        .revision;
    race_hook.take();
    let mut store = MemoryStore::open_existing_read_write(&logical_path.display().to_string())
        .map_err(|error| error.to_string())?;
    let keywords = vec!["post-validation".to_string(), "enrichment".to_string()];
    let vector = vec![0.91_f32; 1024];
    let changed = store
        .update_enrichment_fields(
            &source_id,
            Some("post-validation generated summary"),
            Some(&vector),
            Some(&keywords),
            None,
            revision,
        )
        .map_err(|error| error.to_string())?;
    if !changed {
        return Err(format!(
            "race hook could not mutate reclassification row {source_id}"
        ));
    }
    Ok(())
}

fn maybe_mutate_valid_until_after_plan_validation(
    scans: &[StoreScan],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::MutateValidUntilAfterPlanValidation { source_id }) => {
            source_id.clone()
        }
        _ => return Ok(()),
    };
    let scan = scans
        .iter()
        .find(|scan| scan.raw_row(&source_id).is_some())
        .ok_or_else(|| format!("race hook cannot find valid_until row {source_id}"))?;
    let logical_path = scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook valid_until store has no addressed path".to_string())?;
    race_hook.take();
    let conn = Connection::open(logical_path).map_err(|error| error.to_string())?;
    let changed = conn
        .execute(
            "UPDATE memories SET valid_until = ?1 WHERE id = ?2",
            rusqlite::params!["2026-12-31T23:59:59Z", source_id],
        )
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("race hook could not mutate valid_until".to_string());
    }
    Ok(())
}

fn maybe_mutate_copy_source_after_precheck(
    source_scan: &StoreScan,
    item: &PlanItem,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::MutateCopySourceAfterPrecheck { source_id })
            if source_id == &item.source_id =>
        {
            source_id.clone()
        }
        _ => return Ok(()),
    };
    let logical_path = source_scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook copy source has no addressed path".to_string())?;
    race_hook.take();
    let mut store = MemoryStore::open_existing_read_write(&logical_path.display().to_string())
        .map_err(|error| error.to_string())?;
    let vector = vec![0.83_f32; 1024];
    let changed = store
        .update_enrichment_fields(
            &source_id,
            Some("post-precheck generated summary"),
            Some(&vector),
            None,
            None,
            item.source_revision,
        )
        .map_err(|error| error.to_string())?;
    if !changed {
        return Err(format!(
            "race hook could not mutate copy source {source_id}"
        ));
    }
    Ok(())
}

/// Run the whole plan item to completion the way a sibling worker applying the
/// same deterministic plan would: claim the deterministic target with the
/// `target_copied` receipt, then supersede the source with the
/// `source_superseded` receipt. Both writes go through the same store seams the
/// production path uses, with this plan's real `target_id`, so the store is
/// left in exactly the state a concurrent operator run produces.
fn maybe_complete_item_as_sibling_worker(
    source_scan: &StoreScan,
    target_scan: &StoreScan,
    item: &PlanItem,
    plan_id: &str,
    target_id: &str,
    seam: SiblingCompletionSeam,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    match race_hook.as_ref() {
        Some(CorpusRaceHook::SiblingWorkerCompletesItem {
            source_id,
            seam: hook_seam,
        }) if source_id == &item.source_id && *hook_seam == seam => {}
        _ => return Ok(()),
    }
    race_hook.take();
    let source_path = source_scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook source has no addressed path".to_string())?;
    let target_path = target_scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook target has no addressed path".to_string())?;
    #[cfg(any(test, feature = "bootstrap-test-api"))]
    let mut source_store =
        MemoryStore::open_existing_read_write(&source_path.display().to_string())
            .map_err(|error| error.to_string())?;
    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    let source_store = MemoryStore::open_existing_read_write(&source_path.display().to_string())
        .map_err(|error| error.to_string())?;
    let mut target_store =
        MemoryStore::open_existing_read_write(&target_path.display().to_string())
            .map_err(|error| error.to_string())?;
    let source_entry = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "race hook sibling source row disappeared".to_string())?;
    let source_row = raw_from_entry(&source_entry);
    let mut target_entry = source_entry.clone();
    target_entry.id = target_id.to_string();
    target_entry.metadata =
        metadata_with_receipt(&source_row, receipt_value(item, plan_id, "target_copied"))?;
    match target_store
        .insert_if_absent(&target_entry)
        .map_err(|error| error.to_string())?
    {
        InsertMemoryResult::Inserted | InsertMemoryResult::Existing => {}
    }
    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    {
        let production_result = Err::<(), String>(
            "cross-store wiki race hook cannot perform semantic supersession in a production build"
                .to_string(),
        );
        production_result?;
    }
    #[cfg(any(test, feature = "bootstrap-test-api"))]
    {
        let final_metadata = metadata_with_receipt(
            &source_row,
            receipt_value(item, plan_id, "source_superseded"),
        )?;
        let expected = ExpectedMemoryState::from_entry(&source_entry, None);
        if !source_store
            .supersede_with_metadata_if_expected_state(
                &item.source_id,
                target_id,
                &final_metadata,
                &expected,
            )
            .map_err(|error| error.to_string())?
        {
            return Err("race hook sibling worker could not supersede the source".to_string());
        }
    }
    #[cfg(any(test, feature = "bootstrap-test-api"))]
    let final_result: Result<(), String> = Ok(());
    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    let final_result: Result<(), String> = Ok(());
    final_result
}

#[cfg(any(test, feature = "bootstrap-test-api"))]
fn maybe_inject_copy_after_receipt_prepared(
    source_scan: &StoreScan,
    target_scan: &StoreScan,
    item: &PlanItem,
    target_id: &str,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::MutateSourceAfterReceiptPrepared { source_id })
        | Some(CorpusRaceHook::MutateTargetAfterVerification { source_id })
        | Some(CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition { source_id })
            if source_id == &item.source_id =>
        {
            source_id.clone()
        }
        _ => return Ok(()),
    };
    let hook = race_hook.take().expect("matched hook exists");
    if matches!(
        hook,
        CorpusRaceHook::MutateSourceAfterReceiptPrepared { .. }
            | CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition { .. }
    ) {
        let target_path = target_scan
            .spec
            .addressed_path
            .as_deref()
            .ok_or_else(|| "race hook target has no addressed path".to_string())?;
        let conn = Connection::open(target_path).map_err(|error| error.to_string())?;
        conn.execute_batch(
            "CREATE TRIGGER wiki_corpus_no_hard_delete
             BEFORE DELETE ON memories
             BEGIN
               SELECT RAISE(ABORT, 'wiki corpus hard delete forbidden');
             END;",
        )
        .map_err(|error| error.to_string())?;
    }
    match hook {
        CorpusRaceHook::MutateSourceAfterReceiptPrepared { .. } => {
            let source_path = source_scan
                .spec
                .addressed_path
                .as_deref()
                .ok_or_else(|| "race hook source has no addressed path".to_string())?;
            let mut store =
                MemoryStore::open_existing_read_write(&source_path.display().to_string())
                    .map_err(|error| error.to_string())?;
            let vector = vec![0.77_f32; 1024];
            if !store
                .update_enrichment_fields(
                    &source_id,
                    Some("source enriched after receipt preparation"),
                    Some(&vector),
                    None,
                    None,
                    item.source_revision,
                )
                .map_err(|error| error.to_string())?
            {
                return Err("race hook could not enrich source".to_string());
            }
        }
        CorpusRaceHook::MutateTargetAfterVerification { .. } => {
            let target_path = target_scan
                .spec
                .addressed_path
                .as_deref()
                .ok_or_else(|| "race hook target has no addressed path".to_string())?;
            let mut store =
                MemoryStore::open_existing_read_write(&target_path.display().to_string())
                    .map_err(|error| error.to_string())?;
            let target = store
                .get_with_options(target_id, true)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "race hook target disappeared".to_string())?;
            let vector = vec![0.66_f32; 1024];
            let keywords = vec!["target".to_string(), "enriched".to_string()];
            if !store
                .update_enrichment_fields(
                    target_id,
                    Some("target enriched after verification"),
                    Some(&vector),
                    Some(&keywords),
                    None,
                    target.revision,
                )
                .map_err(|error| error.to_string())?
            {
                return Err("race hook could not enrich target".to_string());
            }
            let enriched = store
                .get_with_options(target_id, true)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "race hook enriched target disappeared".to_string())?;
            let mut metadata = enriched.metadata.clone();
            metadata["enrichment"] = json!({"status": "complete", "lane": "race-hook"});
            if !store
                .update_with_revision(
                    target_id,
                    &enriched.text,
                    &enriched.summary,
                    &enriched.source,
                    &metadata,
                    enriched.vector.as_deref(),
                    enriched.revision,
                )
                .map_err(|error| error.to_string())?
            {
                return Err("race hook could not persist target enrichment metadata".to_string());
            }
        }
        CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition { .. } => {
            #[cfg(any(test, feature = "bootstrap-test-api"))]
            {
                let source_path = source_scan
                    .spec
                    .addressed_path
                    .as_deref()
                    .ok_or_else(|| "race hook source has no addressed path".to_string())?;
                let store =
                    MemoryStore::open_existing_read_write(&source_path.display().to_string())
                        .map_err(|error| error.to_string())?;
                if !store
                    .supersede_memory_if_revision(
                        &source_id,
                        "foreign-wiki-target",
                        item.source_revision,
                    )
                    .map_err(|error| error.to_string())?
                {
                    return Err("race hook could not supersede source".to_string());
                }
            }
            #[cfg(not(any(test, feature = "bootstrap-test-api")))]
            {
                return Err(
                    "wiki corpus race hooks are unavailable in production builds".to_string(),
                );
            }
        }
        _ => unreachable!("matched one of the receipt-prepared hooks"),
    }
    Ok(())
}

fn apply_reclassification(
    scan: &StoreScan,
    item: &PlanItem,
    plan_id: &str,
    retained_backups: &[RetainedBackup],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<MigrationOutcome, String> {
    check_authority(scan, None)?;
    let mut store = open_apply_store(scan)?;
    let logical_path = scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "reclassification store has no addressed path".to_string())?;
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    let entry = store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("source row {} disappeared during apply", item.source_id))?;
    let source_row = raw_from_entry(&entry);
    if receipt_matches(&source_row, item, plan_id, &["reclassified"]) {
        maybe_swap_reclassification_after_receipt_read(item, logical_path, race_hook)?;
        verify_opened_apply_store(&store, logical_path, "reclassification")?;
        return Ok(MigrationOutcome {
            source_store_ref: item.source_store_ref.clone(),
            source_id: item.source_id.clone(),
            target_store_ref: item.target_store_ref.clone(),
            target_id: None,
            action: item.action.clone(),
            outcome: "existing_no_op".to_string(),
            phases: vec!["reclassified".to_string()],
        });
    }
    if entry.revision != item.source_revision {
        return Err(format!(
            "source revision changed during apply for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    let metadata =
        metadata_with_receipt(&source_row, receipt_value(item, plan_id, "reclassified"))?;
    let frozen_row = scan
        .raw_row(&item.source_id)
        .ok_or_else(|| format!("planned source row {} is missing", item.source_id))?;
    let expected = ExpectedMemoryState::from_entry(
        &memory_entry_from_raw(frozen_row),
        frozen_row.superseded_by.as_deref(),
    );
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    verify_backups_before_source_mutation(retained_backups, race_hook)?;
    let changed = store
        .update_with_revision_if_expected_state(
            &entry.id,
            &entry.text,
            &entry.summary,
            &entry.source,
            &metadata,
            entry.vector.as_deref(),
            &expected,
        )
        .map_err(|error| error.to_string())?;
    verify_retained_backups(retained_backups)?;
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    if !changed {
        return Err(format!(
            "source fingerprint changed during reclassification for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    let final_entry = store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "reclassification row disappeared after receipt write".to_string())?;
    let final_row = raw_from_entry(&final_entry);
    if final_entry.revision != item.source_revision + 1
        || !receipt_matches(&final_row, item, plan_id, &["reclassified"])
    {
        return Err(format!(
            "reclassification receipt was not durably recorded for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    maybe_swap_reclassification_after_receipt_read(item, logical_path, race_hook)?;
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    Ok(MigrationOutcome {
        source_store_ref: item.source_store_ref.clone(),
        source_id: item.source_id.clone(),
        target_store_ref: item.target_store_ref.clone(),
        target_id: None,
        action: item.action.clone(),
        outcome: "reclassified".to_string(),
        phases: vec!["reclassified".to_string()],
    })
}

fn canonical_target_matches_plan(row: &RawRow, item: &PlanItem, plan_id: &str) -> bool {
    receipt_matches(row, item, plan_id, &["target_copied"])
        && row.copy_identity_sha256() == item.source_copy_identity_sha256
        && !row.archived
        && row.superseded_by.is_none()
}

/// Describe why [`canonical_target_matches_plan`] rejected a deterministic
/// target: the observed and expected `receipt phase / lifecycle / copy
/// identity` triple. Pure formatting — the decision stays in the predicate.
fn target_mismatch_diagnosis(row: &RawRow, item: &PlanItem, plan_id: &str) -> String {
    let receipt = parse_migration_receipt(row);
    let observed_phase = match &receipt {
        Ok(Some(receipt)) if receipt.plan_id == plan_id && receipt.item == *item => {
            receipt.phase.as_str().to_string()
        }
        Ok(Some(receipt)) if receipt.plan_id == plan_id => {
            format!("{} bound to another plan item", receipt.phase.as_str())
        }
        Ok(Some(receipt)) => format!("{} of plan {}", receipt.phase.as_str(), receipt.plan_id),
        Ok(None) => "<absent>".to_string(),
        Err(error) => format!("<unreadable: {error}>"),
    };
    let sibling_damage_shape = matches!(
        &receipt,
        Ok(Some(receipt))
            if receipt.phase == MigrationPhase::TargetNoncanonical
                && receipt.plan_id == plan_id
                && receipt.item == *item
    ) && row.archived
        && row.copy_identity_sha256() == item.source_copy_identity_sha256;
    let hint = if sibling_damage_shape {
        "; this is the sibling-race damage shape — `tachi wiki corpus --repair-sibling-damage` reports whether it is repairable"
    } else {
        ""
    };
    format!(
        "observed receipt_phase={observed_phase} archived={} superseded_by={} copy_identity={}; \
         expected receipt_phase=target_copied archived=false superseded_by=<none> copy_identity={}{hint}",
        row.archived,
        row.superseded_by.as_deref().unwrap_or("<none>"),
        row.copy_identity_sha256(),
        item.source_copy_identity_sha256,
    )
}

/// Snapshot-independent proof that this copy item is already complete.
///
/// `apply_copy_and_supersede` reads the source row, its migration receipt and
/// its supersession edge once, before it inspects the deterministic target. A
/// sibling worker applying the same deterministic plan can complete the whole
/// item inside that window, which leaves the snapshot claiming "no source
/// receipt yet" while the store already holds the finished migration. Every
/// completion decision taken after that read therefore re-reads the source row,
/// its receipt and the target row here, and applies exactly the predicate
/// [`plan_item_completed`] uses for `copy_to_shared_and_supersede` items.
#[cfg(any(test, feature = "bootstrap-test-api"))]
fn copy_item_completed_from_fresh_state(
    source_store: &MemoryStore,
    target_store: &MemoryStore,
    item: &PlanItem,
    plan_id: &str,
    target_id: &str,
) -> Result<bool, String> {
    let Some(source_entry) = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let mut source_row = raw_from_entry(&source_entry);
    source_row.superseded_by = source_store
        .supersession_target(&item.source_id)
        .map_err(|error| error.to_string())?
        .flatten();
    if !receipt_matches(&source_row, item, plan_id, &["source_superseded"])
        || !source_row.is_superseded_by(target_id)
    {
        return Ok(false);
    }
    let Some(target_entry) = target_store
        .get_with_options(target_id, true)
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let mut target_row = raw_from_entry(&target_entry);
    target_row.superseded_by = target_store
        .supersession_target(target_id)
        .map_err(|error| error.to_string())?
        .flatten();
    Ok(canonical_target_matches_plan(&target_row, item, plan_id))
}

#[cfg(any(test, feature = "bootstrap-test-api"))]
fn copy_completed_no_op_outcome(
    item: &PlanItem,
    target_id: &str,
    phases: Vec<String>,
) -> MigrationOutcome {
    MigrationOutcome {
        source_store_ref: item.source_store_ref.clone(),
        source_id: item.source_id.clone(),
        target_store_ref: item.target_store_ref.clone(),
        target_id: Some(target_id.to_string()),
        action: item.action.clone(),
        outcome: "existing_no_op".to_string(),
        phases,
    }
}

fn copy_only_outcome_from_fresh_state(
    source_store: &MemoryStore,
    target_store: &MemoryStore,
    item: &PlanItem,
    plan_id: &str,
    target_id: &str,
) -> Result<MigrationOutcome, String> {
    let source_entry = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "source row {} disappeared after target copy",
                item.source_id
            )
        })?;
    let mut source_row = raw_from_entry(&source_entry);
    source_row.superseded_by = source_store
        .supersession_target(&item.source_id)
        .map_err(|error| error.to_string())?
        .flatten();
    if source_row.archived
        || source_row.superseded_by.is_some()
        || parse_migration_receipt(&source_row)?.is_some()
    {
        return Err(format!(
            "copy-only completion requires an active, unsuperseded, unreceipted source for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }

    let target_entry = target_store
        .get_with_options(target_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("deterministic target {target_id} disappeared after copy"))?;
    let mut target_row = raw_from_entry(&target_entry);
    target_row.superseded_by = target_store
        .supersession_target(target_id)
        .map_err(|error| error.to_string())?
        .flatten();
    if !canonical_target_matches_plan(&target_row, item, plan_id) {
        return Err(format!(
            "copy-only completion found a noncanonical target {target_id}"
        ));
    }

    Ok(MigrationOutcome {
        source_store_ref: item.source_store_ref.clone(),
        source_id: item.source_id.clone(),
        target_store_ref: item.target_store_ref.clone(),
        target_id: Some(target_id.to_string()),
        action: item.action.clone(),
        outcome: "copied_without_supersession".to_string(),
        phases: vec![MigrationPhase::TargetCopied.as_str().to_string()],
    })
}

#[cfg(any(test, feature = "bootstrap-test-api"))]
fn reconcile_target_noncanonical(
    target_store: &mut MemoryStore,
    target_id: &str,
    item: &PlanItem,
    plan_id: &str,
) -> Result<(), String> {
    for _ in 0..3 {
        let entry = target_store
            .get_with_options(target_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("deterministic target {target_id} disappeared"))?;
        let mut row = raw_from_entry(&entry);
        row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        if receipt_matches(&row, item, plan_id, &["target_noncanonical"])
            && row.archived
            && row.copy_identity_sha256() == item.source_copy_identity_sha256
        {
            return Ok(());
        }
        if !canonical_target_matches_plan(&row, item, plan_id) {
            return Err(format!(
                "deterministic target {target_id} cannot be reconciled: immutable identity or migration provenance changed"
            ));
        }
        let metadata =
            metadata_with_receipt(&row, receipt_value(item, plan_id, "target_noncanonical"))?;
        let expected = ExpectedMemoryState::from_entry(&entry, row.superseded_by.as_deref());
        if target_store
            .archive_with_metadata_if_expected_state(target_id, &metadata, &expected)
            .map_err(|error| error.to_string())?
        {
            continue;
        }
    }
    Err(format!(
        "deterministic target {target_id} kept changing during noncanonical reconciliation"
    ))
}

/// The proven sibling-race damage signature for one deterministic target row.
///
/// It is derived entirely from the row's own migration receipt plus its source
/// row, so the repair needs no plan file: the receipt carries the frozen
/// `PlanItem` and `plan_id` that produced the damage.
#[derive(Debug, Clone)]
struct SiblingDamageSignature {
    item: PlanItem,
    plan_id: String,
    replaced_receipt: Value,
}

enum SiblingDamageAssessment {
    /// Every clause of the signature matched; the row can be compensated.
    Repairable(Box<SiblingDamageSignature>),
    /// At least one clause did not match. The row is reported, never touched.
    Skipped(Vec<String>),
}

/// Rows worth assessing at all: a migration receipt plus either an archived
/// lifecycle or a `target_noncanonical` phase. Healthy canonical targets
/// (`archived = 0` + `target_copied`) and healthy superseded sources
/// (`archived = 0` + `source_superseded`) are never inspected.
fn is_sibling_damage_candidate(row: &RawRow) -> bool {
    if row.metadata.get(RECEIPT_KEY).is_none() {
        return false;
    }
    if row.archived {
        return true;
    }
    matches!(
        parse_migration_receipt(row),
        Ok(Some(receipt)) if receipt.phase == MigrationPhase::TargetNoncanonical
    )
}

/// Decide whether `target_row` carries the exact terminal damage a sibling
/// race used to leave behind:
///
/// * the row is archived, unsuperseded, and carries a `target_noncanonical`
///   receipt whose plan/item identity fields are complete and bind this row,
/// * the row's immutable copy identity still equals the receipt item's,
/// * the receipt item's source row exists, is superseded **by this row**, and
///   carries a `source_superseded` receipt for the same plan and item.
///
/// Anything else is reported with every failed clause and left untouched:
/// under-repairing is recoverable, resurrecting the wrong row is not.
///
/// Deliberately absent from this signature: `PlanItem.source_physical_id` /
/// `target_physical_id`. Those are `unix:{device}:{inode}` (see
/// `physical_db_identity.rs`), and restoring a store from a backup -- the
/// legitimate route back to this exact damage -- gives the restored file a
/// new inode. Binding the repair signature to physical identity would turn
/// every ordinary backup-restore into a false refusal; the row/plan/item
/// identity and content-binding clauses above are what actually distinguish
/// this damaged row from any other, so physical identity adds no
/// discriminating power here.
fn assess_sibling_damage(
    target_store_ref: &str,
    target_row: &RawRow,
    source_lookup: impl FnOnce(&PlanItem) -> Result<Option<RawRow>, String>,
) -> SiblingDamageAssessment {
    let mut reasons = Vec::new();
    let receipt = match parse_migration_receipt(target_row) {
        Ok(Some(receipt)) => receipt,
        Ok(None) => {
            return SiblingDamageAssessment::Skipped(vec![
                "row carries no Wiki corpus migration receipt".to_string(),
            ])
        }
        Err(error) => return SiblingDamageAssessment::Skipped(vec![error]),
    };
    if receipt.phase != MigrationPhase::TargetNoncanonical {
        reasons.push(format!(
            "receipt phase is {}, not target_noncanonical",
            receipt.phase.as_str()
        ));
    }
    if !target_row.archived {
        reasons.push("row is not archived".to_string());
    }
    if let Some(superseded_by) = target_row.superseded_by.as_deref() {
        reasons.push(format!("row is itself superseded by {superseded_by}"));
    }
    let item = &receipt.item;
    if receipt.plan_id.is_empty() {
        reasons.push("receipt has no plan id".to_string());
    }
    if item.action != "copy_to_shared_and_supersede" {
        reasons.push(format!(
            "receipt item action is {}, not copy_to_shared_and_supersede",
            item.action
        ));
    }
    if item.target_store_ref != target_store_ref
        || item.target_id.as_deref() != Some(target_row.id.as_str())
    {
        reasons.push(format!(
            "receipt item target identity {}:{} does not bind this row",
            item.target_store_ref,
            item.target_id.as_deref().unwrap_or("<none>")
        ));
    }
    if item.source_id.is_empty()
        || item.source_store_ref.is_empty()
        || item.source_copy_identity_sha256.is_empty()
    {
        reasons.push("receipt item source identity fields are incomplete".to_string());
    }
    if item.source_store_ref == target_store_ref {
        reasons.push("receipt item source and target are the same logical store".to_string());
    }
    if target_row.copy_identity_sha256() != item.source_copy_identity_sha256 {
        reasons.push(format!(
            "row copy identity {} does not match the receipt item's {}",
            target_row.copy_identity_sha256(),
            item.source_copy_identity_sha256
        ));
    }
    match source_lookup(item) {
        Err(error) => reasons.push(error),
        Ok(None) => reasons.push(format!(
            "receipt item source row {}:{} is absent",
            item.source_store_ref, item.source_id
        )),
        Ok(Some(source_row)) => {
            if !source_row.is_superseded_by(&target_row.id) {
                reasons.push(format!(
                    "source {}:{} is superseded by {}, not by this row",
                    item.source_store_ref,
                    item.source_id,
                    source_row.superseded_by.as_deref().unwrap_or("<nothing>")
                ));
            }
            if !receipt_matches(&source_row, item, &receipt.plan_id, &["source_superseded"]) {
                reasons.push(format!(
                    "source {}:{} does not carry a source_superseded receipt for the same plan item",
                    item.source_store_ref, item.source_id
                ));
            }
        }
    }
    if !reasons.is_empty() {
        return SiblingDamageAssessment::Skipped(reasons);
    }
    let replaced_receipt = match target_row.metadata.get(RECEIPT_KEY) {
        Some(value) => value.clone(),
        None => {
            return SiblingDamageAssessment::Skipped(vec![
                "row lost its migration receipt while it was being assessed".to_string(),
            ])
        }
    };
    SiblingDamageAssessment::Repairable(Box::new(SiblingDamageSignature {
        item: item.clone(),
        plan_id: receipt.plan_id.clone(),
        replaced_receipt,
    }))
}

/// Look a receipt item's source row up in the current inventory.
fn inventory_source_lookup(scans: &[StoreScan], item: &PlanItem) -> Result<Option<RawRow>, String> {
    let source = find_scan(scans, &item.source_store_ref).map_err(|_| {
        format!(
            "receipt item source store {} is not in this inventory",
            item.source_store_ref
        )
    })?;
    Ok(source.raw_row(&item.source_id).cloned())
}

/// Read a receipt item's source row directly from its opened store, including
/// its supersession edge, so the repair decision is taken on fresh state.
fn store_source_lookup(
    source_store: &MemoryStore,
    item: &PlanItem,
) -> Result<Option<RawRow>, String> {
    let Some(entry) = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut row = raw_from_entry(&entry);
    row.superseded_by = source_store
        .supersession_target(&item.source_id)
        .map_err(|error| error.to_string())?
        .flatten();
    Ok(Some(row))
}

fn sibling_repair_row(
    scan: &StoreScan,
    row: &RawRow,
    outcome: &str,
    signature: Option<&SiblingDamageSignature>,
    skipped_reasons: Vec<String>,
) -> SiblingRepairRow {
    let receipt = parse_migration_receipt(row).ok().flatten();
    let item = signature
        .map(|signature| &signature.item)
        .or_else(|| receipt.as_ref().map(|receipt| &receipt.item));
    SiblingRepairRow {
        store_ref: scan.spec.logical_store.reference().to_string(),
        id: row.id.clone(),
        path: row.path.clone(),
        outcome: outcome.to_string(),
        observed_receipt_phase: receipt
            .as_ref()
            .map(|receipt| receipt.phase.as_str().to_string()),
        observed_archived: row.archived,
        observed_superseded_by: row.superseded_by.clone(),
        observed_copy_identity_sha256: row.copy_identity_sha256(),
        expected_receipt_phase: MigrationPhase::TargetNoncanonical.as_str().to_string(),
        expected_archived: true,
        expected_copy_identity_sha256: item.map(|item| item.source_copy_identity_sha256.clone()),
        plan_id: signature
            .map(|signature| signature.plan_id.clone())
            .or_else(|| receipt.as_ref().map(|receipt| receipt.plan_id.clone())),
        source_store_ref: item.map(|item| item.source_store_ref.clone()),
        source_id: item.map(|item| item.source_id.clone()),
        skipped_reasons,
        failure_reason: None,
    }
}

/// Metadata for the compensated row.
///
/// The migration receipt is restored to the byte-identical `target_copied`
/// value a clean migration would have written, because every completion
/// predicate (`canonical_target_matches_plan`, `plan_item_completed`,
/// `validate_target_receipt`) is defined against exactly that phase. The
/// replaced `target_noncanonical` receipt is appended to a separate repair
/// history key instead of being erased.
fn metadata_with_repaired_receipt(
    row: &RawRow,
    signature: &SiblingDamageSignature,
) -> Result<Value, String> {
    let mut metadata = row
        .metadata
        .as_object()
        .cloned()
        .ok_or_else(|| "repair requires object-shaped metadata".to_string())?;
    let record = json!({
        "kind": REPAIR_RECEIPT_KEY,
        "version": REPAIR_RECEIPT_VERSION,
        "phase": REPAIR_PHASE,
        "plan_id": signature.plan_id,
        "target_store_ref": signature.item.target_store_ref,
        "target_id": signature.item.target_id,
        "source_store_ref": signature.item.source_store_ref,
        "source_id": signature.item.source_id,
        "restored_phase": MigrationPhase::TargetCopied.as_str(),
        "replaced_receipt": signature.replaced_receipt,
    });
    let mut history = match metadata.get(REPAIR_RECEIPT_KEY) {
        Some(Value::Array(existing)) => existing.clone(),
        Some(other) => vec![other.clone()],
        None => Vec::new(),
    };
    history.push(record);
    metadata.insert(REPAIR_RECEIPT_KEY.to_string(), Value::Array(history));
    metadata.insert(
        RECEIPT_KEY.to_string(),
        receipt_value(&signature.item, &signature.plan_id, "target_copied"),
    );
    Ok(Value::Object(metadata))
}

/// Compensate one damaged target under a complete-state CAS.
///
/// The signature is re-proven against a fresh read of the target row and its
/// source inside the retry loop, and the un-archive plus the receipt rewrite
/// land in a single transaction, so a concurrent writer either loses the CAS
/// (retry) or changes the state away from the signature (refusal). The row is
/// re-read afterwards and must satisfy `canonical_target_matches_plan`.
fn repair_sibling_damaged_target(
    target_scan: &StoreScan,
    source_scan: &StoreScan,
    signature: &SiblingDamageSignature,
    retained_backups: &[RetainedBackup],
) -> Result<(), String> {
    let target_id = signature
        .item
        .target_id
        .as_deref()
        .ok_or_else(|| "repairable signature without a deterministic target id".to_string())?;
    let target_path = target_scan
        .spec
        .addressed_path
        .as_ref()
        .ok_or_else(|| "repair target has no addressed path".to_string())?;
    let source_path = source_scan
        .spec
        .addressed_path
        .as_ref()
        .ok_or_else(|| "repair source has no addressed path".to_string())?;
    check_authority(target_scan, Some(source_path))?;
    check_authority(source_scan, Some(target_path))?;
    let mut target_store = open_apply_store(target_scan)?;
    let source_store = open_apply_store(source_scan)?;
    verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;

    for _ in 0..3 {
        verify_retained_backups(retained_backups)?;
        let entry = target_store
            .get_with_options(target_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("damaged target {target_id} disappeared before repair"))?;
        let mut row = raw_from_entry(&entry);
        row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        let fresh =
            assess_sibling_damage(target_scan.spec.logical_store.reference(), &row, |item| {
                store_source_lookup(&source_store, item)
            });
        let fresh = match fresh {
            SiblingDamageAssessment::Repairable(fresh) => fresh,
            SiblingDamageAssessment::Skipped(reasons) => {
                return Err(format!(
                    "damaged target {target_id} no longer matches the repair signature: {}",
                    reasons.join("; ")
                ))
            }
        };
        if fresh.item != signature.item || fresh.plan_id != signature.plan_id {
            return Err(format!(
                "damaged target {target_id} changed migration provenance before repair"
            ));
        }
        let metadata = metadata_with_repaired_receipt(&row, &fresh)?;
        let expected = ExpectedMemoryState::from_entry(&entry, row.superseded_by.as_deref());
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_retained_backups(retained_backups)?;
        if !target_store
            .restore_with_metadata_if_expected_state(target_id, &metadata, &expected)
            .map_err(|error| error.to_string())?
        {
            continue;
        }
        let restored = target_store
            .get_with_options(target_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("repaired target {target_id} disappeared after repair"))?;
        let mut restored_row = raw_from_entry(&restored);
        restored_row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        if !canonical_target_matches_plan(&restored_row, &signature.item, &signature.plan_id) {
            return Err(format!(
                "repair of {target_id} did not produce the canonical migrated target"
            ));
        }
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_retained_backups(retained_backups)?;
        return Ok(());
    }
    Err(format!(
        "damaged target {target_id} kept changing during sibling-damage repair"
    ))
}

/// Repair backups are named per physical store *and* per captured state: a
/// migration backup of the same store is a different file, and a later repair
/// run captures its own snapshot instead of failing to verify an older one
/// against the state it no longer describes.
fn repair_backup_file_name(physical_id: &str, row_digest: &str) -> String {
    format!(
        "wiki-corpus-repair-v1-{}-{}.db",
        digest_string(physical_id),
        row_digest
    )
}

/// Back up every physical store the repair is about to mutate.
///
/// The expected evidence is the store's *current* fingerprint (the repair has
/// no plan), and the file name is repair-specific so an existing migration
/// backup of a different state is never mistaken for this one.
fn retain_repair_backups(
    scans: &[StoreScan],
    mutated_store_refs: &BTreeSet<String>,
    backup_dir: &Path,
) -> Result<Vec<RetainedBackup>, String> {
    let mut groups = BTreeMap::<String, (Vec<String>, &StoreScan)>::new();
    for scan in scans
        .iter()
        .filter(|scan| mutated_store_refs.contains(scan.spec.logical_store.reference()))
    {
        let Some(physical) = scan.physical.as_ref() else {
            return Err(format!(
                "repair refuses store {} without physical identity",
                scan.spec.logical_store.reference()
            ));
        };
        let entry = groups
            .entry(physical.physical_id.clone())
            .or_insert_with(|| (Vec::new(), scan));
        entry
            .0
            .push(scan.spec.logical_store.reference().to_string());
    }
    let mut backups = Vec::new();
    for (physical_id, (mut logical_refs, source)) in groups {
        logical_refs.sort();
        logical_refs.dedup();
        check_authority(source, None)?;
        let expected = source.fingerprint().ok_or_else(|| {
            format!(
                "repair cannot fingerprint {} before backup",
                source.spec.logical_store.reference()
            )
        })?;
        let mut race_hook = None;
        backups.push(create_or_verify_backup_with_hook(
            source,
            backup_dir,
            &repair_backup_file_name(&physical_id, &expected.row_digest),
            logical_refs,
            &expected,
            &mut race_hook,
        )?);
    }
    backups.sort_by(|left, right| {
        left.receipt
            .source_physical_id
            .cmp(&right.receipt.source_physical_id)
    });
    verify_retained_backups(&backups)?;
    Ok(backups)
}

/// Assess (and, when confirmed, compensate) every sibling-damaged row.
///
/// A confirmed run never discards a partially completed report: each row's
/// repair is its own atomic, idempotent transaction (see
/// `repair_sibling_damaged_target`), so a row whose compensation fails is
/// recorded as `failed` -- with its reason in both that row and the
/// top-level `errors` -- instead of unwinding the rows already repaired
/// earlier in the same run. Setup failures that happen before any row is
/// touched (the backup directory itself cannot be created or verified) are
/// still returned as `Err`: at that point there is no partial repair to
/// report.
fn repair_sibling_damage(
    scans: &[StoreScan],
    backup_dir: Option<&Path>,
) -> Result<SiblingRepairReport, String> {
    let mut rows = Vec::new();
    let mut repairs = Vec::new();
    for (index, scan) in scans.iter().enumerate() {
        for row in &scan.rows {
            if !is_sibling_damage_candidate(row) {
                continue;
            }
            match assess_sibling_damage(scan.spec.logical_store.reference(), row, |item| {
                inventory_source_lookup(scans, item)
            }) {
                SiblingDamageAssessment::Repairable(signature) => {
                    rows.push(sibling_repair_row(
                        scan,
                        row,
                        "repairable",
                        Some(&*signature),
                        Vec::new(),
                    ));
                    repairs.push((index, rows.len() - 1, signature));
                }
                SiblingDamageAssessment::Skipped(reasons) => {
                    rows.push(sibling_repair_row(scan, row, "skipped", None, reasons));
                }
            }
        }
    }
    let repairable = repairs.len();
    let mut report = SiblingRepairReport {
        version: REPAIR_REPORT_VERSION.to_string(),
        confirmed: backup_dir.is_some(),
        backup_directory: backup_dir.map(|dir| dir.display().to_string()),
        inspected_rows: rows.len(),
        repairable,
        repaired: 0,
        skipped: rows.len() - repairable,
        failed: 0,
        had_failures: false,
        backups: Vec::new(),
        rows,
        errors: Vec::new(),
    };
    let Some(backup_dir) = backup_dir else {
        return Ok(report);
    };
    if repairs.is_empty() {
        return Ok(report);
    }

    let mutated_store_refs = repairs
        .iter()
        .map(|(index, _, _)| scans[*index].spec.logical_store.reference().to_string())
        .collect::<BTreeSet<_>>();
    let retained_backups = retain_repair_backups(scans, &mutated_store_refs, backup_dir)?;
    for (index, row_index, signature) in &repairs {
        let target_scan = &scans[*index];
        let store_ref = report.rows[*row_index].store_ref.clone();
        let id = report.rows[*row_index].id.clone();
        let outcome = find_scan(scans, &signature.item.source_store_ref).and_then(|source_scan| {
            repair_sibling_damaged_target(target_scan, source_scan, signature, &retained_backups)
        });
        match outcome {
            Ok(()) => {
                report.rows[*row_index].outcome = "repaired".to_string();
                report.repaired += 1;
            }
            Err(error) => {
                report.errors.push(format!("{store_ref}:{id}: {error}"));
                report.rows[*row_index].outcome = "failed".to_string();
                report.rows[*row_index].failure_reason = Some(error);
            }
        }
    }
    report.failed = report
        .rows
        .iter()
        .filter(|row| row.outcome == "failed")
        .count();
    report.had_failures = report.failed > 0;
    // The final integrity check is orthogonal to any individual row's
    // outcome (it detects a backup file tampered with mid-run, not a row
    // that failed to compensate), so it folds into `errors` too instead of
    // discarding a report whose row outcomes are otherwise trustworthy.
    match verify_retained_backups(&retained_backups) {
        Ok(()) => {
            report.backups = retained_backups
                .iter()
                .map(|backup| backup.receipt.clone())
                .collect();
        }
        Err(error) => {
            report.errors.push(error);
            report.had_failures = true;
        }
    }
    Ok(report)
}

fn apply_copy_and_supersede(
    source_scan: &StoreScan,
    target_scan: &StoreScan,
    item: &PlanItem,
    plan_id: &str,
    interruption: &mut Option<MigrationInterruption>,
    retained_backups: &[RetainedBackup],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<MigrationOutcome, String> {
    let target_path = target_scan
        .spec
        .addressed_path
        .as_ref()
        .ok_or_else(|| "logical shared target has no addressed path".to_string())?;
    let source_path = source_scan
        .spec
        .addressed_path
        .as_ref()
        .ok_or_else(|| "source path missing".to_string())?;
    check_authority(source_scan, Some(target_path))?;
    check_authority(target_scan, Some(source_path))?;
    let target_id = item
        .target_id
        .as_deref()
        .ok_or_else(|| "copy plan item has no deterministic target id".to_string())?;
    if item.source_vector.row_present && !target_scan.vector_table_present {
        return Err(format!(
            "target {} cannot preserve the source vector for {}:{}",
            item.target_store_ref, item.source_store_ref, item.source_id
        ));
    }
    #[cfg(any(test, feature = "bootstrap-test-api"))]
    let mut source_store = open_apply_store(source_scan)?;
    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    let source_store = open_apply_store(source_scan)?;
    let mut target_store = open_apply_store(target_scan)?;
    maybe_swap_opened_store_path(race_hook, source_scan.spec.logical_store, source_path)?;
    maybe_swap_opened_store_path(race_hook, target_scan.spec.logical_store, target_path)?;
    verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
    let source_entry = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("source row {} disappeared during apply", item.source_id))?;
    let source_row = raw_from_entry(&source_entry);
    let source_receipt = parse_migration_receipt(&source_row)?;
    if let Some(receipt) = source_receipt.as_ref() {
        if receipt.plan_id != plan_id || receipt.item != *item {
            return Err(format!(
                "source migration receipt does not match supplied plan for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
        if !matches!(
            receipt.phase,
            MigrationPhase::SourceReceipted | MigrationPhase::SourceSuperseded
        ) {
            return Err(format!(
                "invalid source migration phase for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
    }
    let source_receipted = source_receipt.is_some();
    let source_superseded_receipted = source_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.phase == MigrationPhase::SourceSuperseded);
    let initial_supersession = source_store
        .supersession_target(&source_entry.id)
        .map_err(|error| error.to_string())?
        .flatten();
    if initial_supersession
        .as_deref()
        .is_some_and(|id| id != target_id)
    {
        return Err(format!(
            "source row {}:{} is already superseded by another target (observed {}, expected {})",
            item.source_store_ref,
            item.source_id,
            initial_supersession.as_deref().unwrap_or("<none>"),
            target_id
        ));
    }
    if source_superseded_receipted && initial_supersession.is_none() {
        return Err(format!(
            "source {}:{} claims source_superseded without durable supersession",
            item.source_store_ref, item.source_id
        ));
    }
    let source_revision_matches = if source_superseded_receipted {
        (item.source_revision + 1..=item.source_revision + 3).contains(&source_entry.revision)
    } else {
        let expected = item.source_revision
            + if initial_supersession.is_some() {
                2
            } else if source_receipted {
                1
            } else {
                0
            };
        source_entry.revision == expected
    };
    let expected_supersession_is_proven =
        initial_supersession.as_deref() == Some(target_id) && source_receipt.is_some();
    if !source_revision_matches
        || !source_content_matches_plan(&source_row, item, expected_supersession_is_proven)
        || source_row.vector_fingerprint(source_scan.vector_table_present) != item.source_vector
    {
        return Err(format!(
            "source revision/content changed during apply for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    maybe_mutate_copy_source_after_precheck(source_scan, item, race_hook)?;
    maybe_complete_item_as_sibling_worker(
        source_scan,
        target_scan,
        item,
        plan_id,
        target_id,
        SiblingCompletionSeam::AfterSourcePrecheck,
        race_hook,
    )?;
    let mut phases = Vec::new();

    if let Some(target_entry) = target_store
        .get_with_options(target_id, true)
        .map_err(|error| error.to_string())?
    {
        let mut target_row = raw_from_entry(&target_entry);
        target_row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        if !canonical_target_matches_plan(&target_row, item, plan_id) {
            return Err(format!(
                "deterministic target occupant collision: {target_id} immutable identity, lifecycle, or receipt mismatch ({})",
                target_mismatch_diagnosis(&target_row, item, plan_id)
            ));
        }
        phases.push("target_copied".to_string());
    } else {
        if source_receipted {
            return Err(format!(
                "source receipt exists without deterministic target {} for {}:{}",
                target_id, item.source_store_ref, item.source_id
            ));
        }
        let mut target_entry = source_entry.clone();
        target_entry.id = target_id.to_string();
        target_entry.metadata =
            metadata_with_receipt(&source_row, receipt_value(item, plan_id, "target_copied"))?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_retained_backups(retained_backups)?;
        match target_store
            .insert_if_absent(&target_entry)
            .map_err(|error| error.to_string())?
        {
            InsertMemoryResult::Inserted | InsertMemoryResult::Existing => {}
        }
        verify_retained_backups(retained_backups)?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        let verified_target = target_store
            .get_with_options(target_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "target insert did not produce a readable row".to_string())?;
        let mut verified_target_row = raw_from_entry(&verified_target);
        verified_target_row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        if !canonical_target_matches_plan(&verified_target_row, item, plan_id) {
            return Err(format!(
                "target insert produced a mismatched immutable occupant or receipt ({})",
                target_mismatch_diagnosis(&verified_target_row, item, plan_id)
            ));
        }
        phases.push("target_copied".to_string());
    }
    verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
    maybe_interrupt(interruption, item, MigrationBoundary::TargetCopied)?;

    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    {
        verify_retained_backups(retained_backups)?;
        copy_only_outcome_from_fresh_state(&source_store, &target_store, item, plan_id, target_id)
    }

    #[cfg(any(test, feature = "bootstrap-test-api"))]
    {
        // Sibling-completion defence: `source_receipted` and
        // `source_superseded_receipted` above were derived from the source snapshot
        // taken before the target was inspected, so they report "not started" for
        // work a sibling worker on the same deterministic plan has already
        // finished. Decide completion from a fresh read of the source row, its
        // receipt and the target row instead.
        if copy_item_completed_from_fresh_state(
            &source_store,
            &target_store,
            item,
            plan_id,
            target_id,
        )? {
            verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
            verify_retained_backups(retained_backups)?;
            phases.push("source_superseded".to_string());
            return Ok(copy_completed_no_op_outcome(item, target_id, phases));
        }
        maybe_inject_copy_after_receipt_prepared(
            source_scan,
            target_scan,
            item,
            target_id,
            race_hook,
        )?;
        maybe_complete_item_as_sibling_worker(
            source_scan,
            target_scan,
            item,
            plan_id,
            target_id,
            SiblingCompletionSeam::AfterReceiptPrepared,
            race_hook,
        )?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_backups_before_source_mutation(retained_backups, race_hook)?;

        let transitioned = {
            let final_metadata = metadata_with_receipt(
                &source_row,
                receipt_value(item, plan_id, "source_superseded"),
            )?;
            let expected_transition =
                ExpectedMemoryState::from_entry(&source_entry, initial_supersession.as_deref());

            let observed_before_transition = source_store
                .supersession_target(&item.source_id)
                .map_err(|error| error.to_string())?
                .flatten();
            if observed_before_transition.as_deref() == Some(target_id) {
                if !source_receipted {
                    false
                } else {
                    source_store
                        .update_with_revision_if_expected_state(
                            &source_entry.id,
                            &source_entry.text,
                            &source_entry.summary,
                            &source_entry.source,
                            &final_metadata,
                            source_entry.vector.as_deref(),
                            &expected_transition,
                        )
                        .map_err(|error| error.to_string())?
                }
            } else if observed_before_transition.is_none() {
                source_store
                    .supersede_with_metadata_if_expected_state(
                        &source_entry.id,
                        target_id,
                        &final_metadata,
                        &expected_transition,
                    )
                    .map_err(|error| error.to_string())?
            } else {
                false
            }
        };

        if !transitioned {
            // Invariant, sibling-completion defence: the deterministic target may
            // only be reconciled to noncanonical once a *fresh* read of the source
            // proves it does not already carry a completed, receipted supersede
            // onto this exact target. The transition above loses that CAS both when
            // this worker's own snapshot went stale and when a sibling worker
            // applying the same deterministic plan finished the item first;
            // archiving the winner's canonical target here would remove the page
            // from every user-facing read surface (`archived = 0 AND superseded_by
            // IS NULL`) and dead-end every later apply on the occupant collision
            // check above.
            if copy_item_completed_from_fresh_state(
                &source_store,
                &target_store,
                item,
                plan_id,
                target_id,
            )? {
                verify_retained_backups(retained_backups)?;
                verify_copy_store_identities(
                    &source_store,
                    source_path,
                    &target_store,
                    target_path,
                )?;
                phases.push("source_superseded".to_string());
                return Ok(copy_completed_no_op_outcome(item, target_id, phases));
            }
            reconcile_target_noncanonical(&mut target_store, target_id, item, plan_id)?;
            verify_retained_backups(retained_backups)?;
            verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
            let observed = source_store
                .supersession_target(&item.source_id)
                .map_err(|error| error.to_string())?
                .flatten();
            if observed.as_deref().is_some_and(|id| id != target_id) {
                return Err(format!(
                "source {}:{} was superseded by foreign target {} before the atomic migration transition",
                item.source_store_ref,
                item.source_id,
                observed.as_deref().unwrap_or("<none>")
            ));
            }
            return Err(format!(
            "source fingerprint changed before the atomic receipt and supersession transition for {}:{}",
            item.source_store_ref, item.source_id
        ));
        }

        verify_retained_backups(retained_backups)?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        maybe_interrupt(interruption, item, MigrationBoundary::SourceSuperseded)?;

        let final_source = source_store
            .get_with_options(&item.source_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "source disappeared after final receipt".to_string())?;
        let final_row = raw_from_entry(&final_source);
        if !receipt_matches(&final_row, item, plan_id, &["source_superseded"])
            || !(item.source_revision + 1..=item.source_revision + 3)
                .contains(&final_source.revision)
            || source_store
                .supersession_target(&item.source_id)
                .map_err(|error| error.to_string())?
                .flatten()
                .as_deref()
                != Some(target_id)
        {
            return Err(format!(
                "source superseded state was not durably reconciled for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_retained_backups(retained_backups)?;
        phases.push("source_superseded".to_string());
        Ok(MigrationOutcome {
            source_store_ref: item.source_store_ref.clone(),
            source_id: item.source_id.clone(),
            target_store_ref: item.target_store_ref.clone(),
            target_id: Some(target_id.to_string()),
            action: item.action.clone(),
            outcome: "copied_and_superseded".to_string(),
            phases,
        })
    }
}

fn raw_from_entry(entry: &MemoryEntry) -> RawRow {
    RawRow {
        id: entry.id.clone(),
        path: entry.path.clone(),
        summary: entry.summary.clone(),
        text: entry.text.clone(),
        importance: entry.importance,
        timestamp: entry.timestamp.clone(),
        valid_from: entry.valid_from.clone(),
        valid_until: entry.valid_until.clone(),
        category: entry.category.clone(),
        topic: entry.topic.clone(),
        keywords: entry.keywords.clone(),
        entities: entry.entities.clone(),
        source: entry.source.clone(),
        scope: entry.scope.clone(),
        archived: entry.archived,
        revision: entry.revision,
        access_count: entry.access_count,
        scored_count: entry.scored_count,
        last_access: entry.last_access.clone(),
        last_use_at: entry.last_use_at.clone(),
        retention_policy: entry.retention_policy.clone(),
        domain: entry.domain.clone(),
        metadata: entry.metadata.clone(),
        vector: entry.vector.clone(),
        recall_count: entry.recall_count,
        query_diversity: entry.query_diversity,
        tier: entry.tier.clone(),
        superseded_by: None,
        metadata_parse_error: None,
        classification: None,
        reasons: Vec::new(),
        effective: None,
    }
}

fn backup_file_name(physical_id: &str) -> String {
    format!("wiki-corpus-v1-{}.db", digest_string(physical_id))
}

fn quick_check(conn: &Connection) -> Result<String, String> {
    let result: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if result.eq_ignore_ascii_case("ok") {
        Ok(result)
    } else {
        Err(format!("SQLite quick_check returned '{result}'"))
    }
}

fn regular_non_symlink_metadata(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot inspect protected Wiki corpus path {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!(
            "protected Wiki corpus path {} must be a regular non-symlink file",
            path.display()
        ));
    }
    Ok(metadata)
}

fn regular_directory_metadata(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot inspect backup directory {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(format!(
            "backup directory {} must be a regular non-symlink directory",
            path.display()
        ));
    }
    Ok(metadata)
}

fn open_file_no_follow(
    path: &Path,
    read: bool,
    write: bool,
    create_new: bool,
) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(read).write(write).create_new(create_new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetainedFileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows { volume: u32, index: u64 },
    #[cfg(not(any(unix, windows)))]
    Unsupported,
}

fn retained_file_identity(metadata: &std::fs::Metadata) -> Result<RetainedFileIdentity, String> {
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

struct RetainedPathFile {
    file: File,
    identity: RetainedFileIdentity,
}

impl RetainedPathFile {
    fn from_file(path: &Path, file: File) -> Result<Self, String> {
        let identity = retained_file_identity(&file.metadata().map_err(|error| {
            format!(
                "cannot inspect retained protected path {}: {error}",
                path.display()
            )
        })?)?;
        Ok(Self { file, identity })
    }

    fn open(path: &Path, read: bool, write: bool, create_new: bool) -> Result<Self, String> {
        let file = open_file_no_follow(path, read, write, create_new)
            .map_err(|error| format!("cannot open protected path {}: {error}", path.display()))?;
        Self::from_file(path, file)
    }

    fn verify_path(&self, path: &Path) -> Result<(), String> {
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

struct RetainedBackup {
    receipt: BackupReceipt,
    path: PathBuf,
    retained: RetainedPathFile,
}

impl RetainedBackup {
    fn verify(&self) -> Result<(), String> {
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

fn verify_retained_backups(backups: &[RetainedBackup]) -> Result<(), String> {
    for backup in backups {
        backup.verify()?;
    }
    Ok(())
}

fn maybe_replace_retained_backup_before_source_mutation(
    backups: &[RetainedBackup],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let replacement_path = match race_hook.as_ref() {
        Some(CorpusRaceHook::ReplaceRetainedBackupBeforeSourceMutation { replacement_path }) => {
            replacement_path.clone()
        }
        _ => return Ok(()),
    };
    let backup = backups
        .first()
        .ok_or_else(|| "race hook requires a retained rollback backup".to_string())?;
    race_hook.take();
    atomic_exchange_paths(&backup.path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject retained backup replacement for {}: {error}",
            backup.path.display()
        )
    })
}

fn verify_backups_before_source_mutation(
    backups: &[RetainedBackup],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    maybe_replace_retained_backup_before_source_mutation(backups, race_hook)?;
    verify_retained_backups(backups)
}

fn verify_sqlite_connection_retained_identity(
    connection: &Connection,
    retained: &RetainedPathFile,
    retained_path: &Path,
) -> Result<(), String> {
    retained.verify_path(retained_path)?;
    let opened_path: String = connection
        .query_row("PRAGMA database_list", [], |row| row.get(2))
        .map_err(|error| format!("cannot resolve opened backup database path: {error}"))?;
    let opened_metadata = regular_non_symlink_metadata(Path::new(&opened_path))?;
    if retained_file_identity(&opened_metadata)? != retained.identity {
        return Err(format!(
            "opened SQLite backup handle is detached from retained object: {}",
            retained_path.display()
        ));
    }
    retained.verify_path(retained_path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactRacePoint {
    Backup,
    Manifest,
}

fn maybe_replace_reserved_artifact(
    path: &Path,
    point: ArtifactRacePoint,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let should_replace = matches!(
        (race_hook.as_ref(), point),
        (
            Some(CorpusRaceHook::ReplaceBackupTempWithNormalFile),
            ArtifactRacePoint::Backup
        ) | (
            Some(CorpusRaceHook::ReplaceManifestTempWithNormalFile),
            ArtifactRacePoint::Manifest
        )
    );
    if !should_replace {
        return Ok(());
    }
    race_hook.take();
    fs::remove_file(path).map_err(|error| {
        format!(
            "cannot inject protected artifact replacement at {}: {error}",
            path.display()
        )
    })?;
    let mut replacement = open_file_no_follow(path, false, true, true).map_err(|error| {
        format!(
            "cannot inject ordinary protected artifact replacement at {}: {error}",
            path.display()
        )
    })?;
    replacement
        .write_all(b"ordinary-file-race-replacement")
        .map_err(|error| format!("cannot write race replacement: {error}"))?;
    replacement
        .sync_all()
        .map_err(|error| format!("cannot sync race replacement: {error}"))?;
    Ok(())
}

fn maybe_replace_existing_backup_after_retain(
    backup_path: &Path,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let replacement_path = match race_hook.as_ref() {
        Some(CorpusRaceHook::ReplaceExistingBackupAfterRetain { replacement_path }) => {
            replacement_path.clone()
        }
        _ => return Ok(()),
    };
    race_hook.take();
    atomic_exchange_paths(backup_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject existing backup replacement for {}: {error}",
            backup_path.display()
        )
    })
}

fn read_file_no_follow(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = open_file_no_follow(path, true, false, false)
        .map_err(|error| format!("cannot read protected path {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read protected path {}: {error}", path.display()))?;
    Ok(bytes)
}

fn open_sqlite_no_follow(path: &Path, flags: OpenFlags) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW)
}

fn canonical_parent_open_path(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("protected SQLite path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("protected SQLite path has no file name: {}", path.display()))?;
    Ok(fs::canonicalize(parent)
        .map_err(|error| format!("cannot canonicalize protected SQLite parent: {error}"))?
        .join(file_name))
}

/// Rename a completed private artifact into its deterministic name without
/// replacing an entry that another process may have reserved. macOS and Linux
/// provide the required kernel primitive; the hard-link fallback is also
/// exclusive and is only used on platforms without either primitive.
fn atomic_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in source path")
        })?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in target path")
        })?;
        let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in source path")
        })?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in target path")
        })?;
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        fs::hard_link(from, to)?;
        fs::remove_file(from)
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn atomic_exchange_paths(left: &Path, right: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let left = CString::new(left.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in left swap path")
    })?;
    let right = CString::new(right.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in right swap path")
    })?;
    let rc = unsafe {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP)
        }
        #[cfg(target_os = "linux")]
        {
            libc::renameat2(
                libc::AT_FDCWD,
                left.as_ptr(),
                libc::AT_FDCWD,
                right.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        }
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
fn atomic_exchange_paths(_left: &Path, _right: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic path exchange is unavailable on this platform",
    ))
}

fn verify_retained_artifact_bytes(
    retained: &RetainedPathFile,
    path: &Path,
    bytes: &[u8],
) -> Result<(), String> {
    retained.verify_path(path)?;
    let installed = read_file_no_follow(path)?;
    retained.verify_path(path)?;
    if installed != bytes {
        return Err(format!(
            "protected artifact {} failed post-install byte verification",
            path.display()
        ));
    }
    Ok(())
}

fn verify_existing_artifact_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    regular_non_symlink_metadata(path)?;
    let retained = RetainedPathFile::open(path, true, false, false)?;
    verify_retained_artifact_bytes(&retained, path, bytes)
}

fn write_private_artifact_with_hook(
    path: &Path,
    bytes: &[u8],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    if fs::symlink_metadata(path).is_ok() {
        return verify_existing_artifact_bytes(path, bytes).map_err(|error| {
            format!(
                "protected artifact {} already exists with different or unstable content: {error}",
                path.display()
            )
        });
    }

    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("artifact")
    ));
    let retained = match open_file_no_follow(&temporary, true, true, true) {
        Ok(file) => {
            let mut retained = RetainedPathFile::from_file(&temporary, file)?;
            retained
                .file
                .write_all(bytes)
                .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
            retained
                .file
                .sync_all()
                .map_err(|error| format!("cannot sync {}: {error}", temporary.display()))?;
            retained
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            regular_non_symlink_metadata(&temporary)?;
            let retained = RetainedPathFile::open(&temporary, true, true, false)?;
            verify_retained_artifact_bytes(&retained, &temporary, bytes).map_err(|error| {
                format!(
                    "protected artifact reservation {} contains different or unstable content: {error}",
                    temporary.display()
                )
            })?;
            retained
        }
        Err(error) => {
            return Err(format!(
                "cannot reserve protected artifact {}: {error}",
                temporary.display()
            ));
        }
    };

    maybe_replace_reserved_artifact(&temporary, ArtifactRacePoint::Manifest, race_hook)?;
    retained.verify_path(&temporary)?;

    match atomic_noreplace(&temporary, path) {
        Ok(()) => {
            retained.file.sync_all().map_err(|error| {
                format!("cannot sync installed artifact {}: {error}", path.display())
            })?;
            verify_retained_artifact_bytes(&retained, path, bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            retained.verify_path(&temporary)?;
            verify_existing_artifact_bytes(path, bytes).map_err(|error| {
                format!(
                    "protected artifact {} belongs to different or unstable content: {error}",
                    path.display()
                )
            })
        }
        Err(error) => Err(format!(
            "cannot atomically reserve protected artifact {}: {error}",
            path.display()
        )),
    }
}

fn verify_backup(
    source: &StoreScan,
    backup_path: &Path,
    logical_store_refs: Vec<String>,
    expected: &PlanStoreFingerprint,
) -> Result<RetainedBackup, String> {
    regular_non_symlink_metadata(backup_path)?;
    let retained = RetainedPathFile::open(backup_path, true, false, false)?;
    let receipt =
        verify_retained_backup(&retained, source, backup_path, logical_store_refs, expected)?;
    Ok(RetainedBackup {
        receipt,
        path: backup_path.to_path_buf(),
        retained,
    })
}

#[cfg(test)]
fn create_or_verify_backup(
    source: &StoreScan,
    backup_dir: &Path,
    logical_store_refs: Vec<String>,
    expected: &PlanStoreFingerprint,
) -> Result<BackupReceipt, String> {
    let physical_id = source
        .physical
        .as_ref()
        .map(|physical| physical.physical_id.clone())
        .unwrap_or_default();
    let mut race_hook = None;
    create_or_verify_backup_with_hook(
        source,
        backup_dir,
        &backup_file_name(&physical_id),
        logical_store_refs,
        expected,
        &mut race_hook,
    )
    .map(|backup| backup.receipt)
}

fn verify_retained_backup(
    retained: &RetainedPathFile,
    source: &StoreScan,
    backup_path: &Path,
    logical_store_refs: Vec<String>,
    expected: &PlanStoreFingerprint,
) -> Result<BackupReceipt, String> {
    let source_physical = source
        .physical
        .as_ref()
        .ok_or_else(|| "backup source has no physical identity".to_string())?;
    retained.verify_path(backup_path)?;
    let source_retained =
        RetainedPathFile::open(Path::new(&source_physical.open_path), true, false, false)?;
    if retained.identity == source_retained.identity {
        return Err(format!(
            "backup path {} resolves to the source physical database",
            backup_path.display()
        ));
    }
    source_retained.verify_path(Path::new(&source_physical.open_path))?;
    let backup_inventory = classify_paths([backup_path.to_path_buf()]);
    if let Some(unresolved) = backup_inventory.unresolved_paths.first() {
        return Err(format!(
            "backup path {} has no readable physical identity: {}",
            backup_path.display(),
            unresolved.error
        ));
    }
    let backup_physical = backup_inventory
        .stores
        .into_iter()
        .next()
        .ok_or_else(|| "backup has no physical identity".to_string())?;
    retained.verify_path(backup_path)?;
    let backup_open_path = canonical_parent_open_path(Path::new(&backup_physical.open_path))?;
    let conn = open_sqlite_no_follow(&backup_open_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    verify_sqlite_connection_retained_identity(&conn, retained, backup_path)?;
    let schema = read_schema_version(&conn).map_err(|error| error.to_string())?;
    if schema != EXPECTED_SCHEMA_VERSION {
        return Err(format!(
            "backup {} schema mismatch: stored {}, expected {}",
            backup_path.display(),
            schema,
            EXPECTED_SCHEMA_VERSION
        ));
    }
    let quick_check = quick_check(&conn)?;
    let (rows, _, vector_table_present) = load_rows(&conn)?;
    let backup_digest = row_digest(&rows, vector_table_present);
    if rows.len() != expected.total_memory_rows
        || backup_digest != expected.row_digest
        || vector_table_present != expected.vector_table_present
    {
        return Err(format!(
            "backup {} row evidence mismatch: rows={}/{} digest={}/{} vector_table={}/{}",
            backup_path.display(),
            rows.len(),
            expected.total_memory_rows,
            backup_digest,
            expected.row_digest,
            vector_table_present,
            expected.vector_table_present
        ));
    }
    verify_sqlite_connection_retained_identity(&conn, retained, backup_path)?;
    retained.verify_path(backup_path)?;
    source_retained.verify_path(Path::new(&source_physical.open_path))?;
    Ok(BackupReceipt {
        logical_store_refs,
        source_physical_id: source_physical.physical_id.clone(),
        source_canonical_path: source_physical.canonical_path.clone(),
        source_open_path: source_physical.open_path.clone(),
        backup_path: backup_path.display().to_string(),
        backup_physical_id: backup_physical.physical_id,
        schema,
        total_memory_rows: rows.len(),
        source_row_digest: expected.row_digest.clone(),
        backup_row_digest: backup_digest,
        vector_table_present,
        source_identity_verified: true,
        backup_identity_verified: true,
        quick_check,
        verified: true,
    })
}

fn create_or_verify_backup_with_hook(
    source: &StoreScan,
    backup_dir: &Path,
    backup_file_name: &str,
    logical_store_refs: Vec<String>,
    expected: &PlanStoreFingerprint,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<RetainedBackup, String> {
    regular_directory_metadata(backup_dir)?;
    let backup_dir = fs::canonicalize(backup_dir)
        .map_err(|error| format!("cannot canonicalize backup directory: {error}"))?;
    let source_physical = source
        .physical
        .as_ref()
        .ok_or_else(|| "backup source has no physical identity".to_string())?;
    let backup_path = backup_dir.join(backup_file_name);
    if fs::symlink_metadata(&backup_path).is_ok() {
        regular_non_symlink_metadata(&backup_path)?;
        let retained = RetainedPathFile::open(&backup_path, true, false, false)?;
        maybe_replace_existing_backup_after_retain(&backup_path, race_hook)?;
        let receipt = verify_retained_backup(
            &retained,
            source,
            &backup_path,
            logical_store_refs,
            expected,
        )?;
        return Ok(RetainedBackup {
            receipt,
            path: backup_path,
            retained,
        });
    }

    if source.row_digest != expected.row_digest
        || source.report.counts.total_memory_rows != expected.total_memory_rows
        || source.vector_table_present != expected.vector_table_present
    {
        return Err(format!(
            "cannot create original backup for {} after source state changed",
            source.spec.logical_store.reference()
        ));
    }

    let temporary = backup_path.with_extension("db.tmp");
    let retained = match fs::symlink_metadata(&temporary) {
        Ok(_) => {
            regular_non_symlink_metadata(&temporary)?;
            let retained = RetainedPathFile::open(&temporary, true, true, false)?;
            verify_retained_backup(
                &retained,
                source,
                &temporary,
                logical_store_refs.clone(),
                expected,
            )?;
            retained
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let reservation =
                open_file_no_follow(&temporary, true, true, true).map_err(|error| {
                    format!("cannot reserve backup {}: {error}", temporary.display())
                })?;
            let retained = RetainedPathFile::from_file(&temporary, reservation)?;
            maybe_replace_reserved_artifact(&temporary, ArtifactRacePoint::Backup, race_hook)?;
            retained.verify_path(&temporary)?;
            let source_conn = open_preview_connection(Path::new(&source_physical.open_path))
                .map_err(|error| format!("cannot open backup source: {error}"))?;
            retained.verify_path(&temporary)?;
            let temporary_open_path = canonical_parent_open_path(&temporary)?;
            let mut destination = open_sqlite_no_follow(
                &temporary_open_path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
            )
            .map_err(|error| format!("cannot create backup {}: {error}", temporary.display()))?;
            retained.verify_path(&temporary)?;
            {
                let backup = rusqlite::backup::Backup::new(&source_conn, &mut destination)
                    .map_err(|error| format!("cannot initialize SQLite backup: {error}"))?;
                backup
                    .run_to_completion(128, Duration::from_millis(100), None)
                    .map_err(|error| format!("SQLite backup failed: {error}"))?;
            }
            drop(destination);
            retained
                .file
                .sync_all()
                .map_err(|error| format!("cannot sync backup {}: {error}", temporary.display()))?;
            verify_retained_backup(
                &retained,
                source,
                &temporary,
                logical_store_refs.clone(),
                expected,
            )?;
            retained
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect backup reservation {}: {error}",
                temporary.display()
            ));
        }
    };

    retained.verify_path(&temporary)?;
    match atomic_noreplace(&temporary, &backup_path) {
        Ok(()) => {
            let receipt = verify_retained_backup(
                &retained,
                source,
                &backup_path,
                logical_store_refs,
                expected,
            )?;
            Ok(RetainedBackup {
                receipt,
                path: backup_path,
                retained,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            retained.verify_path(&temporary)?;
            verify_backup(source, &backup_path, logical_store_refs, expected)
        }
        Err(error) => Err(format!(
            "cannot atomically reserve backup {}: {error}",
            backup_path.display()
        )),
    }
}

#[cfg(test)]
fn write_manifest_if_needed(path: &Path, manifest: &BackupManifest) -> Result<(), String> {
    let mut race_hook = None;
    write_manifest_if_needed_with_hook(path, manifest, &mut race_hook)
}

fn write_manifest_if_needed_with_hook(
    path: &Path,
    manifest: &BackupManifest,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let value = serde_json::to_value(manifest).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?;
    if fs::symlink_metadata(path).is_ok() {
        regular_non_symlink_metadata(path)?;
        let existing = read_file_no_follow(path)?;
        let existing_value: Value = serde_json::from_slice(&existing)
            .map_err(|error| format!("existing backup manifest is invalid: {error}"))?;
        if existing_value != value {
            return Err(format!(
                "deterministic backup manifest {} belongs to a different plan or evidence",
                path.display()
            ));
        }
        return Ok(());
    }
    write_private_artifact_with_hook(path, &bytes, race_hook)
}

fn read_manifest_if_present(path: &Path) -> Result<Option<BackupManifest>, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            regular_non_symlink_metadata(path)?;
            let bytes = read_file_no_follow(path)?;
            serde_json::from_slice(&bytes).map(Some).map_err(|error| {
                format!("invalid Wiki corpus manifest {}: {error}", path.display())
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot inspect Wiki corpus manifest {}: {error}",
            path.display()
        )),
    }
}

fn plan_item_completed(scans: &[StoreScan], plan: &WikiCorpusPlan, item: &PlanItem) -> bool {
    let Ok(source) = find_scan(scans, &item.source_store_ref) else {
        return false;
    };
    let Some(row) = source.raw_row(&item.source_id) else {
        return false;
    };
    match item.action.as_str() {
        "reclassify_in_place" => receipt_matches(row, item, &plan.plan_id, &["reclassified"]),
        "copy_to_shared_and_supersede" => {
            let Some(target_id) = item.target_id.as_deref() else {
                return false;
            };
            if !receipt_matches(row, item, &plan.plan_id, &["source_superseded"])
                || !row.is_superseded_by(target_id)
            {
                return false;
            }

            // The source receipt and immutable supersession edge are not
            // sufficient no-op evidence: the deterministic target must still
            // be present and canonical. `canonical_target_matches_plan`
            // intentionally ignores mutable enrichment while binding the
            // target receipt, copy identity, and active lifecycle.
            let Ok(target) = find_scan(scans, &item.target_store_ref) else {
                return false;
            };
            let Some(target_row) = target.raw_row(target_id) else {
                return false;
            };
            canonical_target_matches_plan(target_row, item, &plan.plan_id)
        }
        _ => false,
    }
}

fn existing_outcome(item: &PlanItem) -> MigrationOutcome {
    MigrationOutcome {
        source_store_ref: item.source_store_ref.clone(),
        source_id: item.source_id.clone(),
        target_store_ref: item.target_store_ref.clone(),
        target_id: item.target_id.clone(),
        action: item.action.clone(),
        outcome: "existing_no_op".to_string(),
        phases: if item.action == "reclassify_in_place" {
            vec!["reclassified".to_string()]
        } else {
            vec!["target_copied".to_string(), "source_superseded".to_string()]
        },
    }
}

fn retain_plan_backups(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<Vec<RetainedBackup>, String> {
    let mut groups = BTreeMap::<String, (Vec<String>, &StoreScan)>::new();
    for scan in scans {
        let Some(physical) = scan.physical.as_ref() else {
            continue;
        };
        let entry = groups
            .entry(physical.physical_id.clone())
            .or_insert_with(|| (Vec::new(), scan));
        entry
            .0
            .push(scan.spec.logical_store.reference().to_string());
    }

    let mut backups = Vec::new();
    for (physical_id, (mut logical_refs, source)) in groups {
        logical_refs.sort();
        logical_refs.dedup();
        check_authority(source, None)?;
        let expected = plan
            .store_fingerprints
            .iter()
            .find(|fingerprint| {
                fingerprint.logical_store_ref == source.spec.logical_store.reference()
            })
            .ok_or_else(|| {
                format!(
                    "plan has no original backup evidence for {}",
                    source.spec.logical_store.reference()
                )
            })?;
        backups.push(create_or_verify_backup_with_hook(
            source,
            backup_dir,
            &backup_file_name(&physical_id),
            logical_refs,
            expected,
            race_hook,
        )?);
    }
    backups.sort_by(|left, right| {
        left.receipt
            .source_physical_id
            .cmp(&right.receipt.source_physical_id)
    });
    verify_retained_backups(&backups)?;
    Ok(backups)
}

#[cfg(test)]
fn apply_plan(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
) -> Result<(BackupManifest, Vec<MigrationOutcome>), String> {
    apply_plan_internal(scans, plan, backup_dir, None, None)
}

fn apply_plan_internal(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
    mut interruption: Option<MigrationInterruption>,
    mut race_hook: Option<CorpusRaceHook>,
) -> Result<(BackupManifest, Vec<MigrationOutcome>), String> {
    regular_directory_metadata(backup_dir)?;
    let backup_dir = fs::canonicalize(backup_dir)
        .map_err(|error| format!("cannot canonicalize backup directory: {error}"))?;
    validate_apply_inventory(scans)?;
    let current_scans = reinventory_apply_scans(scans);
    validate_apply_inventory(&current_scans)?;
    validate_plan(&current_scans, plan)?;
    let scans = current_scans.as_slice();
    maybe_mutate_reclassification_after_plan_validation(scans, &mut race_hook)?;
    maybe_mutate_valid_until_after_plan_validation(scans, &mut race_hook)?;
    if !plan.items.is_empty()
        && plan
            .items
            .iter()
            .all(|item| plan_item_completed(scans, plan, item))
    {
        let final_path = backup_dir.join("wiki-corpus-v1-apply-receipt.json");
        let initial_path = backup_dir.join("wiki-corpus-v1-manifest.json");
        let (manifest, needs_final_receipt) = if let Some(final_manifest) =
            read_manifest_if_present(&final_path)?
        {
            (final_manifest, false)
        } else if let Some(initial) = read_manifest_if_present(&initial_path)? {
            if initial.plan_id != plan.plan_id || initial.status != "backups_verified" {
                return Err(
                    "existing backup manifest belongs to a different completed plan".to_string(),
                );
            }
            (initial, true)
        } else {
            return Err("completed Wiki corpus state has no backup manifest evidence".to_string());
        };
        if manifest.plan_id != plan.plan_id
            || (!needs_final_receipt && manifest.status != "apply_completed")
        {
            return Err("existing apply receipt does not match the completed plan".to_string());
        }
        let retained_backups = retain_plan_backups(scans, plan, &backup_dir, &mut race_hook)?;
        let verified_receipts = retained_backups
            .iter()
            .map(|backup| backup.receipt.clone())
            .collect::<Vec<_>>();
        if manifest.receipts != verified_receipts {
            return Err(
                "completed Wiki corpus manifest does not match retained backup evidence"
                    .to_string(),
            );
        }
        let completed = if needs_final_receipt {
            let completed = BackupManifest {
                version: manifest.version.clone(),
                status: "apply_completed".to_string(),
                plan_id: manifest.plan_id.clone(),
                backup_directory: manifest.backup_directory.clone(),
                receipts: verified_receipts,
                migration_receipts: plan
                    .items
                    .iter()
                    .map(|item| {
                        let phase = if item.action == "reclassify_in_place" {
                            "reclassified"
                        } else {
                            "source_superseded"
                        };
                        receipt_value(item, &plan.plan_id, phase)
                    })
                    .collect(),
            };
            write_manifest_if_needed_with_hook(&final_path, &completed, &mut race_hook)?;
            completed
        } else {
            manifest
        };
        verify_retained_backups(&retained_backups)?;
        maybe_swap_logical_path_before_completed_return(scans, &mut race_hook)?;
        maybe_archive_target_before_completed_return(scans, plan, &mut race_hook)?;
        validate_apply_inventory(scans)?;
        // Keep every writer reservation alive through the final proof and the
        // return expression; dropping the guard is the last operation here.
        let _completed_no_op_locks = CompletedNoOpLocks::acquire(scans)?;
        let final_scans = reinventory_apply_scans(scans);
        validate_apply_inventory(&final_scans)?;
        if !plan
            .items
            .iter()
            .all(|item| plan_item_completed(&final_scans, plan, item))
        {
            return Err(
                "completed Wiki corpus state changed before existing_no_op return; completion proof is no longer valid"
                    .to_string(),
            );
        }
        validate_plan(&final_scans, plan)?;
        verify_retained_backups(&retained_backups)?;
        return Ok((completed, plan.items.iter().map(existing_outcome).collect()));
    }
    if plan.items.is_empty() {
        let manifest = BackupManifest {
            version: REPORT_VERSION.to_string(),
            status: "no_op_no_mutation".to_string(),
            plan_id: plan.plan_id.clone(),
            backup_directory: backup_dir.display().to_string(),
            receipts: Vec::new(),
            migration_receipts: Vec::new(),
        };
        let manifest_path = backup_dir.join("wiki-corpus-v1-manifest.json");
        write_manifest_if_needed_with_hook(&manifest_path, &manifest, &mut race_hook)?;
        maybe_swap_logical_path_before_completed_return(scans, &mut race_hook)?;
        validate_apply_inventory(scans)?;
        return Ok((manifest, Vec::new()));
    }

    let retained_backups = retain_plan_backups(scans, plan, &backup_dir, &mut race_hook)?;
    let receipts = retained_backups
        .iter()
        .map(|backup| backup.receipt.clone())
        .collect::<Vec<_>>();
    let manifest_path = backup_dir.join("wiki-corpus-v1-manifest.json");
    let initial_manifest = BackupManifest {
        version: REPORT_VERSION.to_string(),
        status: "backups_verified".to_string(),
        plan_id: plan.plan_id.clone(),
        backup_directory: backup_dir.display().to_string(),
        receipts: receipts.clone(),
        migration_receipts: plan
            .items
            .iter()
            .map(|item| receipt_value(item, &plan.plan_id, "planned"))
            .collect(),
    };
    write_manifest_if_needed_with_hook(&manifest_path, &initial_manifest, &mut race_hook)?;

    let mut outcomes = Vec::new();
    for item in &plan.items {
        verify_retained_backups(&retained_backups)?;
        let outcome = if item.action == "reclassify_in_place" {
            let source = find_scan(scans, &item.source_store_ref)?;
            apply_reclassification(
                source,
                item,
                &plan.plan_id,
                &retained_backups,
                &mut race_hook,
            )?
        } else {
            let source = find_scan(scans, &item.source_store_ref)?;
            let target = find_scan(scans, &item.target_store_ref)?;
            apply_copy_and_supersede(
                source,
                target,
                item,
                &plan.plan_id,
                &mut interruption,
                &retained_backups,
                &mut race_hook,
            )?
        };
        verify_retained_backups(&retained_backups)?;
        outcomes.push(outcome);
    }
    let final_manifest = BackupManifest {
        version: initial_manifest.version.clone(),
        status: "apply_completed".to_string(),
        plan_id: initial_manifest.plan_id.clone(),
        backup_directory: initial_manifest.backup_directory.clone(),
        receipts: initial_manifest.receipts.clone(),
        migration_receipts: outcomes
            .iter()
            .filter_map(|outcome| {
                plan.items
                    .iter()
                    .find(|item| {
                        item.source_store_ref == outcome.source_store_ref
                            && item.source_id == outcome.source_id
                    })
                    .map(|item| {
                        let phase = outcome
                            .phases
                            .last()
                            .map(String::as_str)
                            .unwrap_or("completed");
                        receipt_value(item, &plan.plan_id, phase)
                    })
            })
            .collect(),
    };
    // The initial manifest remains the deterministic evidence of all verified
    // backups. The separate final receipt makes an interrupted run explicit.
    let final_path = backup_dir.join("wiki-corpus-v1-apply-receipt.json");
    verify_retained_backups(&retained_backups)?;
    write_manifest_if_needed_with_hook(&final_path, &final_manifest, &mut race_hook)?;
    verify_retained_backups(&retained_backups)?;
    Ok((final_manifest, outcomes))
}

#[cfg(test)]
fn apply_plan_with_interruption(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
    interruption: MigrationInterruption,
) -> Result<(BackupManifest, Vec<MigrationOutcome>), String> {
    apply_plan_internal(scans, plan, backup_dir, Some(interruption), None)
}

#[cfg(test)]
fn apply_plan_with_race_hook(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
    race_hook: CorpusRaceHook,
) -> Result<(BackupManifest, Vec<MigrationOutcome>), String> {
    apply_plan_internal(scans, plan, backup_dir, None, Some(race_hook))
}

pub(crate) fn run_wiki_corpus_command(
    apply: bool,
    confirm: Option<String>,
    backup_dir: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
) -> Result<WikiCorpusReport, String> {
    run_wiki_corpus_command_internal(
        apply, confirm, backup_dir, plan_path, global_db, project_db, app_home, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_wiki_corpus_command_internal(
    apply: bool,
    confirm: Option<String>,
    backup_dir: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
    mut race_hook: Option<CorpusRaceHook>,
) -> Result<WikiCorpusReport, String> {
    if !apply {
        if confirm.is_some() || backup_dir.is_some() || plan_path.is_some() {
            return Err("--confirm, --backup-dir, and --plan require explicit --apply".to_string());
        }
    } else {
        if confirm.as_deref() != Some(WIKI_CORPUS_CONFIRMATION_TOKEN) {
            return Err(format!(
                "apply requires exact --confirm {}",
                WIKI_CORPUS_CONFIRMATION_TOKEN
            ));
        }
        let backup_dir = backup_dir
            .as_deref()
            .ok_or_else(|| "apply requires explicit --backup-dir".to_string())?;
        if regular_directory_metadata(backup_dir).is_err() {
            return Err(format!(
                "apply requires an existing backup directory: {}",
                backup_dir.display()
            ));
        }
    }

    let mut scans = store_specs(global_db, project_db, app_home)
        .into_iter()
        .map(inventory_store)
        .collect::<Vec<_>>();
    finalize_classifications(&mut scans);
    for scan in scans.iter_mut() {
        refresh_report(scan);
    }
    if apply {
        maybe_swap_logical_path_after_inventory(&scans, &mut race_hook)?;
    }
    let warnings = scans
        .iter()
        .filter_map(|scan| {
            scan.report
                .read_failure
                .as_ref()
                .map(|failure| format!("{}: {}", scan.report.logical_store_ref, failure.message))
        })
        .collect::<Vec<_>>();
    let preview_plan = build_plan(&scans).ok();
    if !apply {
        return Ok(WikiCorpusReport {
            version: REPORT_VERSION.to_string(),
            mode: "preview".to_string(),
            apply: false,
            stores: scans.into_iter().map(|scan| scan.report).collect(),
            plan: preview_plan,
            backup_manifest: None,
            migration_outcomes: Vec::new(),
            warnings,
            sibling_repair: None,
            legacy_adoption: None,
        });
    }
    let plan = match plan_path {
        Some(path) => load_plan(&path)?,
        None => preview_plan
            .ok_or_else(|| "cannot build an apply plan from the inventory".to_string())?,
    };
    let backup_dir = backup_dir.expect("validated above");
    let (backup_manifest, migration_outcomes) =
        apply_plan_internal(&scans, &plan, &backup_dir, None, race_hook)?;
    Ok(WikiCorpusReport {
        version: REPORT_VERSION.to_string(),
        mode: "apply".to_string(),
        apply: true,
        stores: scans.into_iter().map(|scan| scan.report).collect(),
        plan: Some(plan),
        backup_manifest: Some(backup_manifest),
        migration_outcomes,
        warnings,
        sibling_repair: None,
        legacy_adoption: None,
    })
}

/// `tachi wiki corpus --repair-sibling-damage`.
///
/// In-band compensation for the terminal state the sibling-worker race used to
/// leave behind: a deterministic target archived with a `target_noncanonical`
/// receipt whose source row is already superseded by it, which the user read
/// surface (`archived = 0 AND superseded_by IS NULL`) hides and which every
/// later apply dead-ends on with an occupant collision.
///
/// It is its own confirmed mode with its own token, and without `--confirm` it
/// is a pure dry run that opens no store for writing.
///
/// Trust boundary: the repair signature is judged entirely against the
/// `wiki_corpus_migration` receipt metadata written under `RECEIPT_KEY`, and
/// that key is not in memcore's reserved-metadata list (`memory_crud.rs`'s
/// `RESERVED_REFERENCE_KEYS` / `RESERVED_REM_KEY` / `RESERVED_WIKI_LOG_KEY`),
/// so an ordinary memory write can set it. That does not hand a forger any
/// new capability by itself -- forging a convincing repair still requires
/// planting an id-bound `target_noncanonical` receipt on an archived,
/// unsuperseded row *and* a matching `source_superseded` receipt plus a live
/// supersession edge on the row it names as source, in two separate stores,
/// consistent with each other. But this is the first verb in this module
/// that *un-archives a row* on the strength of that metadata alone, so it is
/// the first place that consistency is worth spelling out rather than
/// assuming.
pub(crate) fn run_wiki_corpus_sibling_repair_command(
    apply: bool,
    confirm: Option<String>,
    backup_dir: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
) -> Result<WikiCorpusReport, String> {
    if apply {
        return Err(
            "--repair-sibling-damage is its own confirmed mode and cannot be combined with --apply"
                .to_string(),
        );
    }
    if plan_path.is_some() {
        return Err(
            "--repair-sibling-damage does not take --plan; each repair is derived from the damaged row's own migration receipt"
                .to_string(),
        );
    }
    let confirmed = match confirm.as_deref() {
        None => {
            if backup_dir.is_some() {
                return Err(format!(
                    "--backup-dir requires --confirm {}",
                    WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN
                ));
            }
            false
        }
        Some(token) if token == WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN => true,
        Some(_) => {
            return Err(format!(
                "sibling-damage repair requires exact --confirm {}",
                WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN
            ))
        }
    };
    let backup_dir = if confirmed {
        let backup_dir = backup_dir
            .ok_or_else(|| "sibling-damage repair requires explicit --backup-dir".to_string())?;
        if regular_directory_metadata(&backup_dir).is_err() {
            return Err(format!(
                "sibling-damage repair requires an existing backup directory: {}",
                backup_dir.display()
            ));
        }
        Some(backup_dir)
    } else {
        None
    };

    let mut scans = store_specs(global_db, project_db, app_home)
        .into_iter()
        .map(inventory_store)
        .collect::<Vec<_>>();
    finalize_classifications(&mut scans);
    for scan in scans.iter_mut() {
        refresh_report(scan);
    }
    let warnings = scans
        .iter()
        .filter_map(|scan| {
            scan.report
                .read_failure
                .as_ref()
                .map(|failure| format!("{}: {}", scan.report.logical_store_ref, failure.message))
        })
        .collect::<Vec<_>>();

    let repair = if confirmed {
        validate_apply_inventory(&scans)?;
        let current = reinventory_apply_scans(&scans);
        validate_apply_inventory(&current)?;
        let repair = repair_sibling_damage(&current, backup_dir.as_deref())?;
        // Report the healed inventory, not the damaged snapshot the run started
        // from, so `stores` and the report agree with each other.
        scans = reinventory_apply_scans(&current);
        repair
    } else {
        repair_sibling_damage(&scans, None)?
    };

    Ok(WikiCorpusReport {
        version: REPORT_VERSION.to_string(),
        mode: if confirmed {
            "sibling_repair".to_string()
        } else {
            "sibling_repair_preview".to_string()
        },
        apply: confirmed,
        stores: scans.into_iter().map(|scan| scan.report).collect(),
        plan: None,
        backup_manifest: None,
        migration_outcomes: Vec::new(),
        warnings,
        sibling_repair: Some(repair),
        legacy_adoption: None,
    })
}

// ---------------------------------------------------------------------------
// `tachi wiki corpus --adopt-legacy` (tachi#1624)
// ---------------------------------------------------------------------------
//
// Bootstrap `<home>/projects/wiki/tachi-memory.db` and adopt the eligible
// legacy-global rows into it verbatim.
//
// # Why this is a separate mode from `--apply`
//
// `--apply` reconciles two stores that already exist. It refuses any absent
// involved store (`validate_apply_inventory`), `store_specs` has no create
// branch, and `build_plan` only emits items for `SharedCandidate` rows — of
// which a host whose corpus is entirely `manual_review` has none. It
// reconciles; it cannot bootstrap and it cannot move this corpus. That gap,
// not a policy disagreement, is why this verb exists.
//
// # What it asserts about the adopted rows: nothing
//
// No `knowledge_scope`, no `applies_to`, no `origin_projects`, no review
// receipt. Stamping `knowledge_scope=shared` on an unreviewed row would make
// the row *less* retrievable, not more (`shared_active_without_review` forces
// `PendingReview`), so the only honest stamp is no stamp. The single metadata
// delta is the additive, nested `wiki_legacy_adoption_v1` key, which every
// derive path ignores.
//
// # Known limitation this mode ships with, deliberately (tachi#1611 phase 5)
//
// Adoption copies rows without touching the legacy source, so after it runs
// every adopted normalized path exists in **two** logical stores.
// `finalize_classifications` forces both copies of any cross-store duplicate
// path to `ManualReview`, and `build_plan` only emits `SharedCandidate` items.
// The reconciler is therefore inert for every adopted path until one side is
// deleted: phase 5's *first* step must be choosing and removing a side. This
// is reported in `legacy_adoption.reconciler_impact` and measured in
// `legacy_adoption.legacy_rows_forced_to_manual_review_by_duplicate_path`
// rather than left for a later reader to rediscover.

/// One legacy row that was not adopted, with the exact rule that excluded it.
#[derive(Debug, Clone, Serialize)]
struct AdoptionSkip {
    id: String,
    path: String,
    reason: String,
}

/// A preserved supersession edge whose target is absent from the new store.
/// Reported, never fatal: repair is tachi#1350's operation.
#[derive(Debug, Clone, Serialize)]
struct AdoptionDanglingEdge {
    id: String,
    superseded_by: String,
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
struct AdoptionPathRewrite {
    id: String,
    source_path: String,
    stored_path: String,
}

#[derive(Debug, Clone, Serialize)]
struct LegacyAdoptionReport {
    version: String,
    confirmed: bool,
    confirm_token_required: String,
    target_path: String,
    target_existed_before: bool,
    /// True once this run has created the store's directory, i.e. from the
    /// first side effect onward — not only after a successful import. Read it
    /// together with `target_removed_after_failure`: `true`/`Some(true)` means
    /// the run created something and then cleaned it up again.
    target_created: bool,
    /// `Some(false)` is a hard failure, not a warning: an unstamped store is
    /// not the wiki corpus and must not be left holding adopted rows.
    target_store_role_stamped: Option<bool>,
    /// Set when a post-creation failure was compensated by removing what this
    /// run wrote: the whole `target_dir` if this run created it, or just the
    /// db file (and its WAL/SHM sidecars) if `target_dir` preexisted this
    /// run. `Some(false)` means the removal itself failed and the operator
    /// must clean up by hand; `remediation` states exactly what to remove.
    target_removed_after_failure: Option<bool>,
    legacy_open_path: Option<String>,
    legacy_physical_id: Option<String>,
    legacy_row_digest_before: String,
    legacy_row_digest_after: Option<String>,
    /// Rows the classifier calls wiki-related, i.e. rows carrying a
    /// classification. Rows the legacy store holds that are not wiki-related
    /// are neither counted here nor listed in `skipped`: they are not part of
    /// this corpus and a `classification_missing` entry per non-wiki row would
    /// bury the skip histogram an operator has to read.
    wiki_related_rows: usize,
    eligible_rows: usize,
    skipped: Vec<AdoptionSkip>,
    adopted_ids: Vec<String>,
    adoption_run_id: String,
    provenance_marker_key: String,
    /// Derived lifecycle of every adopted row, keyed by lifecycle name. A run
    /// can import every row, match every checksum, and still leave search
    /// empty if nothing derives a default-retrievable lifecycle — so the
    /// receipt states this rather than leaving it to be inferred.
    derived_lifecycle_counts: BTreeMap<String, usize>,
    default_retrievable_rows: usize,
    path_rewrites: Vec<AdoptionPathRewrite>,
    rows_imported: usize,
    vectors_imported: usize,
    vectors_absent: usize,
    dangling_supersessions: Vec<AdoptionDanglingEdge>,
    expected_lifecycle_checksum: String,
    expected_vector_checksum: String,
    observed_lifecycle_checksum: Option<String>,
    observed_vector_checksum: Option<String>,
    checksums_match: Option<bool>,
    /// The standing consequence of copy-without-supersede; see the module
    /// section above.
    reconciler_impact: String,
    legacy_rows_forced_to_manual_review_by_duplicate_path: Option<usize>,
    /// True when the run recorded a failure instead of returning `Err`. The
    /// CLI turns this into a non-zero exit after printing the receipt.
    had_failures: bool,
    /// Populated only when the operator still has cleanup to do.
    remediation: Option<String>,
    errors: Vec<String>,
}

const WIKI_LEGACY_ADOPTION_RECONCILER_IMPACT: &str =
    "adoption copies rows without superseding the legacy source, so every adopted normalized \
     path now exists in two logical stores; finalize_classifications forces both copies to \
     manual_review and build_plan only emits shared_candidate items, so `tachi wiki corpus \
     --apply` is inert for these paths until one side is deleted (tachi#1611 phase 5 must \
     delete a side first)";

fn legacy_adoption_target_path(app_home: &Path) -> PathBuf {
    app_home
        .join("projects")
        .join("wiki")
        .join(memcore::MEMORY_DB_FILENAME)
}

/// Removes exactly the files a bootstrap-and-import of `target` could have
/// written -- the main db file plus its `-wal`/`-shm` sidecars -- and nothing
/// else in `target`'s directory. Used by the round-2 bug B failure path,
/// where `target_dir` preexisted this run and so is not this run's to
/// remove wholesale. Missing files are not an error: a failure early in
/// `adopt_into_bootstrapped_store` may have written none of the three.
fn remove_adopted_store_files(target: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        match fs::remove_file(preview_sidecar_path(target, suffix)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// `Ok(())` when the row is adopted; `Err(reason)` is the exact reported
/// string. Rules run in declaration order so the reported reason is stable.
fn adoption_eligibility(row: &RawRow) -> Result<(), String> {
    // E1 -- only the class the classifier could not place. Every other class
    // carries positive typed evidence about where it belongs, and adoption
    // never overrides that evidence.
    match row.classification {
        Some(CorpusClassification::ManualReview) => {}
        Some(other) => {
            return Err(format!(
                "classification={}",
                serde_json::to_value(other)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".to_string())
            ))
        }
        None => return Err("classification_missing".to_string()),
    }

    // E2 -- the same two knowledge-artifact roots `wiki_ops::search` is
    // willing to project. Spelled out rather than imported: that predicate is
    // `pub(super)` to `wiki_ops`, and a bootstrap verb must not widen it.
    let path = row.path.as_str();
    let public_knowledge_artifact = path == "/wiki"
        || path.starts_with("/wiki/")
        || path == "/guide"
        || path.starts_with("/guide/");
    if !public_knowledge_artifact {
        return Err("path_not_public_knowledge_artifact".to_string());
    }

    // E3 -- every identity the shared write path refuses. Checked here so the
    // refusal is a reported skip in the preview rather than a mid-import
    // failure against a store that has already been created.
    if adoption_reserved_identity(row) {
        return Err("reserved_identity".to_string());
    }

    // E4 -- the marker is inserted into a JSON object; a row whose metadata is
    // not an object has nowhere to carry provenance.
    if row.metadata_parse_error.is_some() || !row.metadata.is_object() {
        return Err("metadata_json_invalid".to_string());
    }

    Ok(())
}

/// Mirror of the identities `memcore`'s shared
/// `refuse_reserved_write_identity` rejects, evaluated against the same values
/// the import will pass it (the id, the *normalized* path, and the topic).
fn adoption_reserved_identity(row: &RawRow) -> bool {
    let entry = memory_entry_from_raw(row);
    let normalized = memcore::path_router::normalize_path(&row.path);
    row.id.trim().is_empty()
        || row.id.starts_with("anchor:")
        || memcore::is_reserved_wiki_rem_id(&row.id)
        || row.id == "wiki-operation-log"
        || normalized == "/wiki/_log"
        || normalized.starts_with("/wiki/_log/")
        || memcore::namespace::is_wiki_log_entry(&entry)
}

fn adoption_run_id(legacy_physical_id: &str, ids: &[String]) -> String {
    digest_string(
        &json!({
            "version": WIKI_LEGACY_ADOPTION_REPORT_VERSION,
            "legacy_physical_id": legacy_physical_id,
            "ids": ids,
        })
        .to_string(),
    )
}

/// The two lifecycle columns `RawRow` does not carry.
///
/// `load_rows`' SELECT deliberately omits `created_at`/`updated_at`, and
/// extending `RawRow` would change `row_digest` and therefore every existing
/// plan fingerprint. Adoption reads them separately instead.
struct LegacyLifecycleColumns {
    created_at: String,
    updated_at: String,
}

fn load_legacy_lifecycle_columns(
    open_path: &Path,
) -> Result<BTreeMap<String, LegacyLifecycleColumns>, String> {
    let conn = open_preview_connection(open_path)?;
    let mut statement = conn
        .prepare("PRAGMA table_info(memories)")
        .map_err(|error| error.to_string())?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| error.to_string())?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);
    for required in ["created_at", "updated_at"] {
        if !columns.contains(required) {
            return Err(format!(
                "legacy store lacks column '{required}'; snapshot adoption cannot fabricate it"
            ));
        }
    }

    // One deferred transaction so both columns come from a single WAL
    // snapshot rather than two independent reads.
    let tx = conn
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    let mut statement = tx
        .prepare("SELECT id, created_at, updated_at FROM memories ORDER BY id ASC")
        .map_err(|error| error.to_string())?;
    let mut lifecycle = BTreeMap::new();
    let mapped = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                LegacyLifecycleColumns {
                    created_at: row.get(1)?,
                    updated_at: row.get(2)?,
                },
            ))
        })
        .map_err(|error| error.to_string())?;
    for row in mapped {
        let (id, columns) = row.map_err(|error| error.to_string())?;
        lifecycle.insert(id, columns);
    }
    drop(statement);
    drop(tx);
    Ok(lifecycle)
}

/// Build one import entry: the source row verbatim, plus exactly one additive
/// nested metadata key.
fn adoption_entry(
    row: &RawRow,
    lifecycle: &LegacyLifecycleColumns,
    legacy_physical_id: &str,
    adoption_run_id: &str,
    adopted_at: &str,
) -> Result<memcore::PortableImportEntry, String> {
    // The id is preserved verbatim. The destination is created empty by this
    // same run, so the import's duplicate-id refusal cannot fire and every
    // intra-set `superseded_by` edge still resolves to the row it named.
    let mut entry = memory_entry_from_raw(row);
    let Some(metadata) = entry.metadata.as_object_mut() else {
        // Unreachable: E4 already refused non-object metadata. Kept as a hard
        // refusal rather than a silent default so a future eligibility edit
        // cannot quietly start dropping provenance.
        return Err(format!(
            "legacy row '{}' metadata is not a JSON object; adoption cannot attach provenance",
            row.id
        ));
    };
    metadata.insert(
        WIKI_LEGACY_ADOPTION_MARKER_KEY.to_string(),
        json!({
            "source_store": "legacy_global",
            "source_physical_id": legacy_physical_id,
            "source_id": row.id,
            "adopted_at": adopted_at,
            "confirm_token": WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN,
            "adoption_run_id": adoption_run_id,
            "review_status": "review_pending",
            "reviewed": false,
        }),
    );
    Ok(memcore::PortableImportEntry {
        entry,
        created_at: lifecycle.created_at.clone(),
        updated_at: lifecycle.updated_at.clone(),
        superseded_by: row.superseded_by.clone(),
    })
}

/// Every refusal the import can raise that is a pure function of the entries,
/// evaluated **before** anything is created on disk.
///
/// `import_snapshot_batch` runs its own pre-walk before opening a
/// transaction, so these failures never write a row — but by then the store
/// file already exists, and an empty store is worse than no store: it flips
/// the named-store existence gate, which silences the zero-store search
/// refusal without making search work.
///
/// Not pre-flightable: vector availability, which is a property of the
/// destination and unknowable until it is open. That one is handled by the
/// post-creation compensation path.
fn adoption_preflight(entries: &[memcore::PortableImportEntry]) -> Result<(), String> {
    // The destination carries the `wiki` label, so `validate_write_path`'s only
    // rejection clause -- a `/wiki/...` path in a non-wiki store -- cannot fire
    // for any entry. Asserted once rather than assumed.
    if !memcore::path_router::db_label_is_wiki_corpus(memcore::path_router::WIKI_CORPUS_DB_LABEL) {
        return Err("wiki corpus label no longer identifies the wiki corpus".to_string());
    }
    let mut seen = BTreeSet::new();
    for import in entries {
        let entry = &import.entry;
        if !seen.insert(entry.id.as_str()) {
            return Err(format!(
                "legacy adoption pre-flight: duplicate id '{}' in the adoption set",
                entry.id
            ));
        }
        let normalized = memcore::path_router::normalize_path(&entry.path);
        if entry.id.trim().is_empty()
            || entry.id.starts_with("anchor:")
            || memcore::is_reserved_wiki_rem_id(&entry.id)
            || entry.id == "wiki-operation-log"
            || normalized == "/wiki/_log"
            || normalized.starts_with("/wiki/_log/")
            || entry.topic.eq_ignore_ascii_case("wiki_log")
        {
            return Err(format!(
                "legacy adoption pre-flight: id '{}' is a reserved write identity and cannot be \
                 imported",
                entry.id
            ));
        }
    }
    Ok(())
}

/// Read back every adopted row's stored `path` and compare it against the
/// value adoption predicted, closing the gap the lifecycle checksum leaves
/// open (it covers `archived`/`created_at`/`id`/`revision`/`superseded_by`/
/// `updated_at`/`valid_until` -- not `path`).
fn verify_adopted_paths(
    target: &Path,
    entries: &[memcore::PortableImportEntry],
) -> Result<(), String> {
    // Opened read-write, not read-only: a read-only open of a WAL database
    // whose `-shm` sidecar is absent has to create one, which is exactly the
    // failure `open_preview_connection` exists to work around. This is a file
    // this run created and owns, so there is nothing to protect it from.
    let conn = Connection::open(target)
        .map_err(|error| format!("cannot reopen adopted store for path readback: {error}"))?;
    for import in entries {
        let expected = memcore::path_router::normalize_path(&import.entry.path);
        let stored: Option<String> = conn
            .query_row(
                "SELECT path FROM memories WHERE id = ?1",
                [import.entry.id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| {
                format!(
                    "cannot read adopted path for '{}': {error}",
                    import.entry.id
                )
            })?;
        match stored {
            Some(stored) if stored == expected => {}
            Some(stored) => {
                return Err(format!(
                "adopted row '{}' stored path '{stored}' does not match the predicted '{expected}'",
                import.entry.id
            ))
            }
            None => {
                return Err(format!(
                    "adopted row '{}' is absent from the destination after a successful import",
                    import.entry.id
                ))
            }
        }
    }
    Ok(())
}

/// `tachi wiki corpus --adopt-legacy`.
///
/// Bootstrap-only by construction: it refuses to run against a `wiki` store
/// that already exists, and it is read-only on the legacy global — no
/// supersede, no archive, no metadata write. Rollback is therefore complete
/// removal of the store it created; there is no partial state to reconcile.
pub(crate) fn run_wiki_corpus_legacy_adoption_command(
    apply: bool,
    confirm: Option<String>,
    backup_dir: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
) -> Result<WikiCorpusReport, String> {
    if apply {
        return Err(
            "--adopt-legacy is its own confirmed mode and cannot be combined with --apply"
                .to_string(),
        );
    }
    if plan_path.is_some() {
        return Err(
            "--adopt-legacy does not take --plan; the adoption set is derived from the legacy \
             store's own classification"
                .to_string(),
        );
    }
    if backup_dir.is_some() {
        return Err(
            "--adopt-legacy does not take --backup-dir; it never writes to an existing store — \
             back up the legacy global out-of-band before running"
                .to_string(),
        );
    }
    let confirmed = match confirm.as_deref() {
        None => false,
        Some(token) if token == WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN => true,
        Some(_) => {
            return Err(format!(
                "legacy adoption requires exact --confirm {WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN}"
            ))
        }
    };

    // Round-2 bug A: this existing-target gate must run before the legacy
    // store is opened at all, not merely before the destination is written.
    // `target_existed_before` is a pure function of `app_home` and the wiki
    // store path -- it touches nothing legacy -- so hoisting it ahead of
    // `store_specs`/`inventory_store` (which does open and read the legacy
    // global) costs nothing and closes the window where a second confirmed
    // run against an occupied target would still read the legacy source
    // before refusing.
    let target = legacy_adoption_target_path(app_home);
    let target_dir = target
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "wiki store path has no parent directory".to_string())?;
    let target_existed_before =
        MemoryServer::resolve_existing_named_project_db_path_in_home("wiki", app_home)
            .map_err(|error| format!("cannot resolve the named wiki store: {error}"))?
            .is_some()
            || fs::symlink_metadata(&target).is_ok();
    if target_existed_before && confirmed {
        // B1: the remedy is removal and re-run, not `--apply`. `--apply`
        // refuses any absent involved store, and on a host with no bound
        // project DB the `bound_project` store is always absent -- so naming
        // it here would send the operator to a command that cannot run.
        return Err(format!(
            "wiki store already exists at {}; --adopt-legacy is a bootstrap-only mode — remove {} \
             and re-run if you intend to rebuild it",
            target.display(),
            target_dir.display()
        ));
    }

    let mut scans = store_specs(global_db, project_db, app_home)
        .into_iter()
        .map(inventory_store)
        .collect::<Vec<_>>();
    finalize_classifications(&mut scans);
    for scan in scans.iter_mut() {
        refresh_report(scan);
    }
    let warnings = scans
        .iter()
        .filter_map(|scan| {
            scan.report
                .read_failure
                .as_ref()
                .map(|failure| format!("{}: {}", scan.report.logical_store_ref, failure.message))
        })
        .collect::<Vec<_>>();

    let legacy = find_scan(&scans, LogicalStore::LegacyGlobal.reference())?;
    if let Some(failure) = legacy.report.read_failure.as_ref() {
        return Err(format!(
            "legacy global store is unreadable: {}",
            failure.message
        ));
    }
    if legacy.report.stored_schema != Some(EXPECTED_SCHEMA_VERSION) {
        return Err(format!(
            "legacy global store is at schema {:?}, expected {EXPECTED_SCHEMA_VERSION}",
            legacy.report.stored_schema
        ));
    }
    let physical = legacy
        .physical
        .clone()
        .ok_or_else(|| "legacy global store has no physical identity".to_string())?;
    let legacy_row_digest_before = legacy.row_digest.clone();
    let legacy_spec = legacy.spec.clone();

    // B3: the corpus is the classified rows, not every row the legacy store
    // holds. `load_rows` selects the whole `memories` table; only wiki-related
    // rows carry a classification.
    let mut skipped = Vec::new();
    // Owned clones, not borrows: the report below consumes `scans`, and the
    // adoption set must outlive the inventory it was derived from.
    let mut eligible: Vec<RawRow> = Vec::new();
    let mut wiki_related_rows = 0usize;
    for row in legacy.rows.iter() {
        if row.classification.is_none() {
            continue;
        }
        wiki_related_rows += 1;
        match adoption_eligibility(row) {
            Ok(()) => eligible.push(row.clone()),
            Err(reason) => skipped.push(AdoptionSkip {
                id: row.id.clone(),
                path: row.path.clone(),
                reason,
            }),
        }
    }
    skipped.sort_by(|left, right| left.id.cmp(&right.id));
    eligible.sort_by(|left, right| left.id.cmp(&right.id));
    let eligible_rows = eligible.len();
    if eligible.is_empty() && confirmed {
        return Err(
            "no eligible legacy rows to adopt; refusing to create an empty wiki store".to_string(),
        );
    }

    let lifecycle_columns = load_legacy_lifecycle_columns(&PathBuf::from(&physical.open_path))?;
    let adopted_ids = eligible
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let adoption_run_id = adoption_run_id(&physical.physical_id, &adopted_ids);
    let adopted_at = chrono::Utc::now().to_rfc3339();
    let mut entries = Vec::with_capacity(eligible.len());
    for row in eligible.iter() {
        let lifecycle = lifecycle_columns.get(&row.id).ok_or_else(|| {
            format!(
                "legacy row '{}' vanished between inventory and lifecycle read; re-run",
                row.id
            )
        })?;
        entries.push(adoption_entry(
            row,
            lifecycle,
            &physical.physical_id,
            &adoption_run_id,
            &adopted_at,
        )?);
    }

    let path_rewrites = entries
        .iter()
        .filter_map(|import| {
            let stored = memcore::path_router::normalize_path(&import.entry.path);
            (stored != import.entry.path).then(|| AdoptionPathRewrite {
                id: import.entry.id.clone(),
                source_path: import.entry.path.clone(),
                stored_path: stored,
            })
        })
        .collect::<Vec<_>>();

    // The lifecycle every adopted row will derive once stored. A run can
    // import every row and match every checksum while leaving search empty, so
    // this is measured, not assumed.
    let mut derived_lifecycle_counts = BTreeMap::<String, usize>::new();
    let mut default_retrievable_rows = 0usize;
    for import in entries.iter() {
        let effective = derive_effective_knowledge_artifact(
            &import.entry.metadata,
            &import.entry.path,
            &import.entry.scope,
        );
        *derived_lifecycle_counts
            .entry(effective.lifecycle.as_str().to_string())
            .or_default() += 1;
        if effective.lifecycle.is_default_retrievable() {
            default_retrievable_rows += 1;
        }
    }
    if confirmed && default_retrievable_rows == 0 {
        return Err(format!(
            "no adopted row would be default-retrievable (0 of {eligible_rows} derive a \
             default-retrievable lifecycle); creating the wiki store would silence the zero-store \
             search refusal without making search work"
        ));
    }

    let expected_lifecycle_checksum =
        memcore::PortableImportReceipt::expected_lifecycle_checksum(&entries)
            .map_err(|error| format!("cannot compute the expected lifecycle checksum: {error}"))?;
    let expected_vector_checksum =
        memcore::PortableImportReceipt::expected_vector_checksum(&entries)
            .map_err(|error| format!("cannot compute the expected vector checksum: {error}"))?;

    let mut report = LegacyAdoptionReport {
        version: WIKI_LEGACY_ADOPTION_REPORT_VERSION.to_string(),
        confirmed,
        confirm_token_required: WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN.to_string(),
        target_path: target.display().to_string(),
        target_existed_before,
        target_created: false,
        target_store_role_stamped: None,
        target_removed_after_failure: None,
        legacy_open_path: Some(physical.open_path.clone()),
        legacy_physical_id: Some(physical.physical_id.clone()),
        legacy_row_digest_before: legacy_row_digest_before.clone(),
        legacy_row_digest_after: None,
        wiki_related_rows,
        eligible_rows,
        skipped,
        adopted_ids,
        adoption_run_id,
        provenance_marker_key: WIKI_LEGACY_ADOPTION_MARKER_KEY.to_string(),
        derived_lifecycle_counts,
        default_retrievable_rows,
        path_rewrites,
        rows_imported: 0,
        vectors_imported: 0,
        vectors_absent: 0,
        dangling_supersessions: Vec::new(),
        expected_lifecycle_checksum: expected_lifecycle_checksum.clone(),
        expected_vector_checksum: expected_vector_checksum.clone(),
        observed_lifecycle_checksum: None,
        observed_vector_checksum: None,
        checksums_match: None,
        reconciler_impact: WIKI_LEGACY_ADOPTION_RECONCILER_IMPACT.to_string(),
        legacy_rows_forced_to_manual_review_by_duplicate_path: None,
        had_failures: false,
        remediation: None,
        errors: Vec::new(),
    };

    if !confirmed {
        return Ok(WikiCorpusReport {
            version: REPORT_VERSION.to_string(),
            mode: "legacy_adoption_preview".to_string(),
            apply: false,
            stores: scans.into_iter().map(|scan| scan.report).collect(),
            plan: None,
            backup_manifest: None,
            migration_outcomes: Vec::new(),
            warnings,
            sibling_repair: None,
            legacy_adoption: Some(report),
        });
    }

    // Everything that can be refused from the entries alone is refused here,
    // while nothing exists on disk.
    adoption_preflight(&entries)?;

    // Round-2 bug B: `target_existed_before` is a statement about the target
    // *file* (or a named resolution), not about `target_dir`. A preexisting,
    // empty `target_dir` -- no db, possibly holding unrelated content --
    // passes that gate and lands here with `target_existed_before == false`.
    // Failure cleanup must not conflate "this run's writes" with "the whole
    // directory": record, before creating anything, whether this run is the
    // one bringing the directory into existence.
    let target_dir_created_by_this_run = !target_dir.exists();
    fs::create_dir_all(&target_dir).map_err(|error| {
        format!(
            "cannot create wiki store directory {}: {error}",
            target_dir.display()
        )
    })?;
    let target_str = target
        .to_str()
        .ok_or_else(|| "wiki store path is not valid UTF-8".to_string())?
        .to_string();

    // From here on every failure is compensated and *recorded* rather than
    // thrown away with the receipt: the run has created a store, and an
    // operator who is handed a bare error string has no evidence of what state
    // the host is in.
    report.target_created = true;
    let outcome = adopt_into_bootstrapped_store(
        &target,
        &target_str,
        &entries,
        &expected_lifecycle_checksum,
        &expected_vector_checksum,
        &legacy_spec,
        &legacy_row_digest_before,
        &mut report,
    );

    if let Err(error) = outcome {
        report.errors.push(error);
        report.had_failures = true;
        // An empty or half-built store is worse than no store: its mere
        // existence flips the named-store gate and silences the zero-store
        // search refusal. Remove what *this run* wrote, and say whether the
        // removal worked. If this run also created `target_dir`, the whole
        // directory is this run's to remove; if the directory preexisted,
        // only the db file (and its WAL/SHM sidecars) this run wrote belong
        // to it -- the directory and anything else inside it is left alone.
        let removal = if target_dir_created_by_this_run {
            fs::remove_dir_all(&target_dir)
        } else {
            remove_adopted_store_files(&target)
        };
        match removal {
            Ok(()) => {
                report.target_removed_after_failure = Some(true);
                report.remediation = Some(if target_dir_created_by_this_run {
                    format!(
                        "the store this run created at {} was removed; the legacy global is \
                         untouched, so re-running `tachi wiki corpus --adopt-legacy --confirm {}` \
                         after fixing the reported error is safe",
                        target_dir.display(),
                        WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN
                    )
                } else {
                    format!(
                        "the wiki store file this run wrote at {} was removed; {} preexisted this \
                         run and was left untouched. The legacy global is untouched, so \
                         re-running `tachi wiki corpus --adopt-legacy --confirm {}` after fixing \
                         the reported error is safe",
                        target.display(),
                        target_dir.display(),
                        WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN
                    )
                });
            }
            Err(remove_error) => {
                report.target_removed_after_failure = Some(false);
                report.errors.push(if target_dir_created_by_this_run {
                    format!(
                        "cannot remove the wiki store this run created at {}: {remove_error}",
                        target_dir.display()
                    )
                } else {
                    format!(
                        "cannot remove the wiki store file this run wrote at {}: {remove_error}",
                        target.display()
                    )
                });
                report.remediation = Some(if target_dir_created_by_this_run {
                    format!(
                        "remove {} by hand and re-run `tachi wiki corpus --adopt-legacy --confirm {}`",
                        target_dir.display(),
                        WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN
                    )
                } else {
                    format!(
                        "remove {} by hand (leave {} in place unless you intend to rebuild it) \
                         and re-run `tachi wiki corpus --adopt-legacy --confirm {}`",
                        target.display(),
                        target_dir.display(),
                        WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN
                    )
                });
            }
        }
    }

    // Re-derive the specs rather than reusing the pre-run ones: the shared
    // `wiki` spec was resolved before this run created the store, so a reused
    // spec would still carry `addressed_path: None` and the report would deny
    // the existence of the store it had just built.
    let mut scans = store_specs(global_db, project_db, app_home)
        .into_iter()
        .map(inventory_store)
        .collect::<Vec<_>>();
    finalize_classifications(&mut scans);
    for scan in scans.iter_mut() {
        refresh_report(scan);
    }
    if !report.had_failures {
        // B4, measured rather than asserted: how many legacy rows the
        // duplicate-path pass has just pinned to manual_review.
        report.legacy_rows_forced_to_manual_review_by_duplicate_path = Some(
            find_scan(&scans, LogicalStore::LegacyGlobal.reference())
                .map(|scan| {
                    scan.rows
                        .iter()
                        .filter(|row| {
                            row.reasons.iter().any(|reason| {
                                reason == "duplicate_normalized_path_across_logical_stores"
                            })
                        })
                        .count()
                })
                .unwrap_or(0),
        );
    }
    let warnings = scans
        .iter()
        .filter_map(|scan| {
            scan.report
                .read_failure
                .as_ref()
                .map(|failure| format!("{}: {}", scan.report.logical_store_ref, failure.message))
        })
        .collect::<Vec<_>>();

    Ok(WikiCorpusReport {
        version: REPORT_VERSION.to_string(),
        mode: "legacy_adoption".to_string(),
        apply: true,
        stores: scans.into_iter().map(|scan| scan.report).collect(),
        plan: None,
        backup_manifest: None,
        migration_outcomes: Vec::new(),
        warnings,
        sibling_repair: None,
        legacy_adoption: Some(report),
    })
}

/// The confirmed run's write half, factored out so its caller can compensate
/// uniformly for every failure after the store file exists.
#[allow(clippy::too_many_arguments)]
fn adopt_into_bootstrapped_store(
    target: &Path,
    target_str: &str,
    entries: &[memcore::PortableImportEntry],
    expected_lifecycle_checksum: &str,
    expected_vector_checksum: &str,
    legacy_spec: &StoreSpec,
    legacy_row_digest_before: &str,
    report: &mut LegacyAdoptionReport,
) -> Result<(), String> {
    // The only construction that stamps role = "wiki" *and* the full store
    // profile at birth. `stored == 0` on a brand-new file makes this a build,
    // not a migration, so no `MigrationAuthority` is involved.
    let mut store = MemoryStore::open_with_label_and_context(
        target_str,
        memcore::path_router::WIKI_CORPUS_DB_LABEL,
        &memcore::DbOpenContext::create_fresh(),
    )
    .map_err(|error| {
        format!(
            "cannot bootstrap wiki store at {}: {error}",
            target.display()
        )
    })?;
    let stamped = store.is_wiki_corpus_store();
    report.target_store_role_stamped = Some(stamped);
    if !stamped {
        return Err(
            "bootstrapped store is not stamped as the wiki corpus; refusing to import".to_string(),
        );
    }

    let receipt = store
        .import_snapshot_batch(entries)
        .map_err(|error| format!("legacy adoption import failed: {error}"))?;
    drop(store);

    report.rows_imported = receipt.rows_imported;
    report.vectors_imported = receipt.vectors_imported;
    report.vectors_absent = receipt.vectors_absent;
    report.dangling_supersessions = receipt
        .dangling_supersessions
        .iter()
        .map(|edge| AdoptionDanglingEdge {
            id: edge.id.clone(),
            superseded_by: edge.superseded_by.clone(),
        })
        .collect();
    report.observed_lifecycle_checksum = Some(receipt.lifecycle_checksum.clone());
    report.observed_vector_checksum = Some(receipt.vector_checksum.clone());
    let checksums_match = receipt.lifecycle_checksum == expected_lifecycle_checksum
        && receipt.vector_checksum == expected_vector_checksum;
    report.checksums_match = Some(checksums_match);
    if receipt.rows_imported != entries.len() || !checksums_match {
        return Err(format!(
            "legacy adoption receipt mismatch: rows {}/{}, lifecycle {} vs {}, vector {} vs {}",
            receipt.rows_imported,
            entries.len(),
            receipt.lifecycle_checksum,
            expected_lifecycle_checksum,
            receipt.vector_checksum,
            expected_vector_checksum,
        ));
    }

    // `path` is outside the lifecycle checksum, so it gets its own readback.
    verify_adopted_paths(target, entries)?;

    // Independent read of what was written: `open_existing_read_write` opens
    // unlabelled and derives its label from the stamp alone, so this is not a
    // replay of the label the bootstrap claimed.
    let check = MemoryStore::open_existing_read_write(target_str)
        .map_err(|error| format!("cannot reopen the adopted wiki store: {error}"))?;
    let retained = check.is_wiki_corpus_store();
    drop(check);
    report.target_store_role_stamped = Some(retained);
    if !retained {
        return Err("adopted wiki store did not retain its role stamp".to_string());
    }

    // Source-immutability proof: adoption is read-only on the legacy global,
    // so its row digest must be byte-identical to the pre-run value. The
    // legacy spec alone is re-inventoried on purpose -- `row_digest` is
    // computed from the raw rows, before `finalize_classifications`, so it
    // does not depend on which other stores are in the scan set.
    let legacy_after = inventory_store(legacy_spec.clone());
    report.legacy_row_digest_after = Some(legacy_after.row_digest.clone());
    if legacy_after.row_digest != legacy_row_digest_before {
        return Err(format!(
            "legacy global changed during adoption (digest {legacy_row_digest_before} -> {}); the \
             adopted store cannot be qualified against it",
            legacy_after.row_digest
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn run_wiki_corpus_command_with_race_hook(
    apply: bool,
    confirm: Option<String>,
    backup_dir: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
    race_hook: CorpusRaceHook,
) -> Result<WikiCorpusReport, String> {
    run_wiki_corpus_command_internal(
        apply,
        confirm,
        backup_dir,
        plan_path,
        global_db,
        project_db,
        app_home,
        Some(race_hook),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_entry(id: &str, path: &str, metadata: Value) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: path.to_string(),
            summary: "fixture summary".to_string(),
            text: "fixture text".to_string(),
            importance: 0.5,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            valid_from: "2026-01-01T00:00:00Z".to_string(),
            valid_until: None,
            category: "wiki".to_string(),
            topic: "fixture".to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "wiki".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: Some("permanent".to_string()),
            domain: Some("wiki".to_string()),
            metadata,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn create_current_fixture(path: &Path, entries: &[MemoryEntry]) {
        let mut store = MemoryStore::open(&path.display().to_string()).unwrap();
        for entry in entries {
            store.upsert(entry).unwrap();
        }
    }

    fn remove_memory_hard_delete_guard(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("DROP TRIGGER IF EXISTS wiki_corpus_no_hard_delete")
            .unwrap();
    }

    fn fixture_entry_with_vector(
        id: &str,
        path: &str,
        metadata: Value,
        vector: Vec<f32>,
    ) -> MemoryEntry {
        let mut entry = fixture_entry(id, path, metadata);
        entry.vector = Some(vector);
        entry
    }

    fn db_snapshot(path: &Path) -> BTreeMap<String, Vec<u8>> {
        ["", "-wal", "-shm"]
            .into_iter()
            .filter_map(|suffix| {
                let candidate = PathBuf::from(format!("{}{}", path.display(), suffix));
                fs::read(&candidate)
                    .ok()
                    .map(|bytes| (candidate.display().to_string(), bytes))
            })
            .collect()
    }

    fn db_snapshot_fingerprint(
        snapshot: &BTreeMap<String, Vec<u8>>,
    ) -> BTreeMap<String, (usize, String)> {
        snapshot
            .iter()
            .map(|(path, bytes)| {
                (
                    path.clone(),
                    (bytes.len(), format!("{:x}", Sha256::digest(bytes))),
                )
            })
            .collect()
    }

    fn directory_snapshot(path: &Path) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }

    fn create_schema_18_fixture(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                summary TEXT NOT NULL,
                text TEXT NOT NULL,
                importance REAL NOT NULL,
                timestamp TEXT NOT NULL,
                valid_from TEXT NOT NULL,
                valid_until TEXT,
                category TEXT NOT NULL,
                topic TEXT NOT NULL,
                keywords TEXT NOT NULL,
                entities TEXT NOT NULL,
                source TEXT NOT NULL,
                scope TEXT NOT NULL,
                archived INTEGER NOT NULL,
                revision INTEGER NOT NULL,
                metadata TEXT
            );
            PRAGMA user_version = 18;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories
             (id, path, summary, text, importance, timestamp, valid_from,
              valid_until, category, topic, keywords, entities, source, scope,
              archived, revision, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10, ?11,
                     ?12, ?13, 0, 1, ?14)",
            rusqlite::params![
                "legacy-wiki",
                "/wiki/legacy",
                "legacy",
                "legacy text",
                0.5_f64,
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
                "wiki",
                "legacy",
                "[]",
                "[]",
                "wiki",
                "general",
                r#"{"wiki":true}"#,
            ],
        )
        .unwrap();
    }

    fn fixture_scan(store: LogicalStore, path: &Path) -> StoreScan {
        inventory_store(StoreSpec {
            logical_store: store,
            addressed_path: Some(path.to_path_buf()),
            resolution_error: None,
        })
    }

    fn classify_scans(scans: &mut [StoreScan]) {
        finalize_classifications(scans);
        for scan in scans {
            refresh_report(scan);
        }
    }

    fn item_for(source: &RawRow) -> PlanItem {
        let replay_identity = replay_identity_for(
            LogicalStore::LegacyGlobal.reference(),
            "unix:source",
            source,
            false,
        );
        PlanItem {
            action: "copy_to_shared_and_supersede".to_string(),
            source_store_ref: LogicalStore::LegacyGlobal.reference().to_string(),
            source_physical_id: "unix:source".to_string(),
            source_id: source.id.clone(),
            source_path: source.path.clone(),
            normalized_path: source.normalized_path(),
            source_revision: source.revision,
            source_content_sha256: source.content_sha256(),
            source_copy_identity_sha256: source.copy_identity_sha256(),
            source_valid_until: source.valid_until.clone(),
            source_superseded_by: source.superseded_by.clone(),
            source_vector: source.vector_fingerprint(false),
            replay_identity: replay_identity.clone(),
            target_store_ref: LogicalStore::SharedWiki.reference().to_string(),
            target_physical_id: "unix:target".to_string(),
            target_id: Some(format!("wiki-corpus:{replay_identity}")),
        }
    }

    fn row(store: LogicalStore, id: &str, path: &str, metadata: Value) -> RawRow {
        RawRow {
            id: id.to_string(),
            path: path.to_string(),
            summary: "summary".to_string(),
            text: "text".to_string(),
            importance: 0.5,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            valid_from: "2026-01-01T00:00:00Z".to_string(),
            valid_until: None,
            category: "wiki".to_string(),
            topic: "topic".to_string(),
            keywords: Vec::new(),
            entities: Vec::new(),
            source: "wiki".to_string(),
            scope: if store == LogicalStore::BoundProject {
                "general"
            } else {
                "general"
            }
            .to_string(),
            archived: false,
            revision: 1,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            retention_policy: Some("permanent".to_string()),
            domain: Some("wiki".to_string()),
            metadata,
            vector: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
            superseded_by: None,
            metadata_parse_error: None,
            classification: None,
            reasons: Vec::new(),
            effective: None,
        }
    }

    fn shared_metadata() -> Value {
        json!({
            "artifact_kind": "wiki",
            "knowledge_scope": "shared",
            "origin_projects": ["sigil"],
            "applies_to": {"projects": ["sigil"]},
            "lifecycle": "candidate"
        })
    }

    #[test]
    fn classifier_keeps_project_general_and_rejects_underspecified_shared() {
        let project = row(
            LogicalStore::BoundProject,
            "project",
            "/wiki/project",
            json!({}),
        );
        let (classification, reasons, _, _) = classify_row(LogicalStore::BoundProject, &project);
        assert_eq!(classification, CorpusClassification::ProjectBound);
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("legacy_scope_general")));

        let malformed = row(
            LogicalStore::LegacyGlobal,
            "malformed",
            "/wiki/malformed",
            json!({"knowledge_scope":"shared","origin_projects":[],"applies_to":{}}),
        );
        let (classification, _, _, _) = classify_row(LogicalStore::LegacyGlobal, &malformed);
        assert_eq!(classification, CorpusClassification::ManualReview);
    }

    #[test]
    fn authority_operational_and_test_markers_beat_wiki_category() {
        let lane = row(
            LogicalStore::LegacyGlobal,
            "lane",
            "/wiki/lane",
            json!({"record_kind":"lane_card","knowledge_scope":"shared","origin_projects":["sigil"],"applies_to":{"projects":["sigil"]}}),
        );
        assert_eq!(
            classify_row(LogicalStore::LegacyGlobal, &lane).0,
            CorpusClassification::AuthorityRecord
        );

        let operation = row(
            LogicalStore::LegacyGlobal,
            "operation",
            "/wiki",
            json!({"snapshot_kind":"operational"}),
        );
        assert_eq!(
            classify_row(LogicalStore::LegacyGlobal, &operation).0,
            CorpusClassification::OperationalSnapshot
        );

        let rem_operation = row(
            LogicalStore::LegacyGlobal,
            "wiki-rem:operation",
            "/wiki/rem",
            json!({"rem": {"operation": "recover"}}),
        );
        assert_eq!(
            classify_row(LogicalStore::LegacyGlobal, &rem_operation).0,
            CorpusClassification::OperationalSnapshot
        );

        let recall_cache = row(
            LogicalStore::LegacyGlobal,
            "foundry:recall-cache:fixture",
            "/wiki/recall-cache/fixture",
            json!({}),
        );
        assert_eq!(
            classify_row(LogicalStore::LegacyGlobal, &recall_cache).0,
            CorpusClassification::OperationalSnapshot
        );

        let mut recall_cache_outside_wiki = row(
            LogicalStore::LegacyGlobal,
            "foundry:recall-cache:outside-wiki",
            "/recall-cache",
            json!({}),
        );
        recall_cache_outside_wiki.category = "memory".to_string();
        recall_cache_outside_wiki.source = "runtime".to_string();
        recall_cache_outside_wiki.domain = None;
        assert!(is_wiki_related(&recall_cache_outside_wiki));
        assert_eq!(
            classify_row(LogicalStore::LegacyGlobal, &recall_cache_outside_wiki).0,
            CorpusClassification::OperationalSnapshot
        );

        let mut rem_outside_wiki = row(
            LogicalStore::LegacyGlobal,
            "wiki-rem:outside-wiki",
            "/operations",
            json!({"rem": {"operation": "recover"}}),
        );
        rem_outside_wiki.category = "memory".to_string();
        rem_outside_wiki.source = "runtime".to_string();
        rem_outside_wiki.domain = None;
        assert!(is_wiki_related(&rem_outside_wiki));
        assert_eq!(
            classify_row(LogicalStore::LegacyGlobal, &rem_outside_wiki).0,
            CorpusClassification::OperationalSnapshot
        );

        let fixture = row(
            LogicalStore::LegacyGlobal,
            "fixture",
            "/wiki/fixture",
            json!({"fixture":true}),
        );
        assert_eq!(
            classify_row(LogicalStore::LegacyGlobal, &fixture).0,
            CorpusClassification::TestEphemeral
        );
    }

    #[test]
    fn valid_shared_requires_bounded_typed_applicability() {
        let candidate = row(
            LogicalStore::LegacyGlobal,
            "candidate",
            "/wiki/candidate",
            shared_metadata(),
        );
        assert_eq!(
            classify_row(LogicalStore::LegacyGlobal, &candidate).0,
            CorpusClassification::SharedCandidate
        );
    }

    #[test]
    fn production_copy_only_completion_is_honest_and_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/copy-only",
                shared_metadata(),
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let item = &plan.items[0];
        let target_id = item.target_id.as_deref().unwrap();
        let source_store = open_apply_store(&scans[0]).unwrap();
        let mut target_store = open_apply_store(&scans[1]).unwrap();
        let source_entry = source_store
            .get_with_options(&item.source_id, true)
            .unwrap()
            .unwrap();
        let source_row = raw_from_entry(&source_entry);
        let mut target_entry = source_entry;
        target_entry.id = target_id.to_string();
        target_entry.metadata = metadata_with_receipt(
            &source_row,
            receipt_value(item, &plan.plan_id, "target_copied"),
        )
        .unwrap();
        target_store.insert_if_absent(&target_entry).unwrap();

        for _ in 0..2 {
            let outcome = copy_only_outcome_from_fresh_state(
                &source_store,
                &target_store,
                item,
                &plan.plan_id,
                target_id,
            )
            .expect("copy-only completion must be replay-safe");
            assert_eq!(outcome.outcome, "copied_without_supersession");
            assert_eq!(outcome.phases, vec!["target_copied"]);
        }
        let source_after = source_store
            .get_with_options(&item.source_id, true)
            .unwrap()
            .unwrap();
        assert!(!source_after.archived);
        assert_eq!(
            source_store.supersession_target(&item.source_id).unwrap(),
            Some(None)
        );
        assert!(parse_migration_receipt(&raw_from_entry(&source_after))
            .unwrap()
            .is_none());
    }

    #[test]
    fn duplicate_paths_are_manual_review_across_logical_stores() {
        let mut scans = vec![
            empty_test_scan(
                LogicalStore::BoundProject,
                vec![row(
                    LogicalStore::BoundProject,
                    "same-project",
                    "/wiki/same",
                    shared_metadata(),
                )],
            ),
            empty_test_scan(
                LogicalStore::SharedWiki,
                vec![row(
                    LogicalStore::SharedWiki,
                    "same-shared",
                    "/wiki/same",
                    shared_metadata(),
                )],
            ),
        ];
        finalize_classifications(&mut scans);
        assert!(scans.iter().all(|scan| {
            scan.rows[0].classification == Some(CorpusClassification::ManualReview)
        }));
    }

    fn empty_test_scan(store: LogicalStore, rows: Vec<RawRow>) -> StoreScan {
        StoreScan {
            spec: StoreSpec {
                logical_store: store,
                addressed_path: None,
                resolution_error: None,
            },
            physical: None,
            rows,
            report: StoreReport {
                logical_store_ref: store.reference().to_string(),
                semantic_role: store.semantic_role().to_string(),
                addressed_path: None,
                existence: false,
                resolved_path: None,
                canonical_path: None,
                open_path: None,
                open_path_basis: None,
                physical_identity: None,
                read_failure: None,
                stored_schema: None,
                expected_schema: EXPECTED_SCHEMA_VERSION,
                counts: RowCounts::default(),
                rows: Vec::new(),
            },
            row_digest: String::new(),
            vector_table_present: false,
        }
    }

    #[test]
    fn deterministic_plan_material_and_receipt_are_stable() {
        let candidate = row(
            LogicalStore::LegacyGlobal,
            "candidate",
            "/wiki/candidate",
            shared_metadata(),
        );
        let item = PlanItem {
            action: "copy_to_shared_and_supersede".to_string(),
            source_store_ref: LogicalStore::LegacyGlobal.reference().to_string(),
            source_physical_id: "unix:1:2".to_string(),
            source_id: candidate.id.clone(),
            source_path: candidate.path.clone(),
            normalized_path: "/wiki/candidate".to_string(),
            source_revision: 1,
            source_content_sha256: candidate.content_sha256(),
            source_copy_identity_sha256: candidate.copy_identity_sha256(),
            source_valid_until: candidate.valid_until.clone(),
            source_superseded_by: None,
            source_vector: candidate.vector_fingerprint(false),
            replay_identity: "replay".to_string(),
            target_store_ref: LogicalStore::SharedWiki.reference().to_string(),
            target_physical_id: "unix:3:4".to_string(),
            target_id: Some("wiki-corpus:target".to_string()),
        };
        assert_eq!(plan_item_material(&item), plan_item_material(&item));
        assert_eq!(
            receipt_value(&item, "plan", "target_copied"),
            receipt_value(&item, "plan", "target_copied")
        );
    }

    #[test]
    fn preview_inventory_reads_current_fixture_without_sidecars_or_byte_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("current.db");
        create_current_fixture(
            &path,
            &[fixture_entry("current-wiki", "/wiki/current", json!({}))],
        );
        let before = db_snapshot(&path);

        let mut scans = vec![fixture_scan(LogicalStore::SharedWiki, &path)];
        classify_scans(&mut scans);

        assert_eq!(scans[0].report.stored_schema, Some(EXPECTED_SCHEMA_VERSION));
        assert!(scans[0].report.read_failure.is_none());
        assert_eq!(scans[0].report.counts.total_memory_rows, 1);
        assert_eq!(scans[0].report.counts.wiki_related_rows, 1);
        assert_eq!(
            db_snapshot_fingerprint(&db_snapshot(&path)),
            db_snapshot_fingerprint(&before)
        );
    }

    #[cfg(unix)]
    #[test]
    fn preview_staging_directory_is_owner_only_before_any_snapshot_copy() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let controlled_root = tempfile::tempdir().unwrap();
        let staging = preview_staging_dir_in(controlled_root.path()).unwrap();
        let metadata = fs::symlink_metadata(&staging).unwrap();

        assert!(metadata.file_type().is_dir());
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
    }

    #[test]
    fn preview_inventories_schema_18_fixture_and_classifies_it_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-v18.db");
        create_schema_18_fixture(&path);
        let before = db_snapshot(&path);

        let mut scans = vec![fixture_scan(LogicalStore::LegacyGlobal, &path)];
        classify_scans(&mut scans);

        assert_eq!(scans[0].report.stored_schema, Some(18));
        assert_eq!(scans[0].report.expected_schema, EXPECTED_SCHEMA_VERSION);
        assert!(scans[0].report.read_failure.is_none());
        assert_eq!(scans[0].report.counts.total_memory_rows, 1);
        assert_eq!(scans[0].report.counts.wiki_related_rows, 1);
        assert_eq!(
            scans[0].report.rows[0].classification,
            CorpusClassification::ManualReview
        );
        assert_eq!(db_snapshot(&path), before);
    }

    #[test]
    fn apply_confirmation_backup_and_schema_gates_refuse_before_writes() {
        let directory = tempfile::tempdir().unwrap();
        let global = directory.path().join("missing-global.db");
        let app_home = directory.path().join("home");
        let before = directory_snapshot(directory.path());

        let error = run_wiki_corpus_command(
            true,
            Some("wrong-token".to_string()),
            Some(directory.path().join("backups")),
            None,
            &global,
            None,
            &app_home,
        )
        .unwrap_err();
        assert!(error.contains("exact --confirm"));
        assert_eq!(directory_snapshot(directory.path()), before);

        let error = run_wiki_corpus_command(
            true,
            Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
            Some(directory.path().join("backups")),
            None,
            &global,
            None,
            &app_home,
        )
        .unwrap_err();
        assert!(error.contains("existing backup directory"));
        assert_eq!(directory_snapshot(directory.path()), before);

        let legacy_path = directory.path().join("legacy-v18.db");
        create_schema_18_fixture(&legacy_path);
        let before_legacy = db_snapshot(&legacy_path);
        let legacy_scan = fixture_scan(LogicalStore::LegacyGlobal, &legacy_path);
        let error = validate_apply_inventory(&[legacy_scan]).unwrap_err();
        assert!(error.contains(&format!(
            "stored Some(18), expected {}",
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION
        )));
        assert_eq!(db_snapshot(&legacy_path), before_legacy);
    }

    #[test]
    fn deterministic_target_occupant_mismatch_fails_closed() {
        let source = row(
            LogicalStore::LegacyGlobal,
            "source",
            "/wiki/occupant",
            shared_metadata(),
        );
        let item = item_for(&source);
        let mut occupant = source.clone();
        occupant.id = item.target_id.clone().unwrap();
        occupant.text = "different content".to_string();
        let target = empty_test_scan(LogicalStore::SharedWiki, vec![occupant]);

        let error = validate_target_occupant(&item, "plan", &source, &target).unwrap_err();
        assert!(error.contains("deterministic target occupant collision"));
    }

    #[test]
    fn partial_target_phase_replays_without_delete_or_revision_churn() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        let source_entry = fixture_entry("source", "/wiki/replay", shared_metadata());
        create_current_fixture(&source_path, &[source_entry.clone()]);
        create_current_fixture(&target_path, &[]);

        let mut initial_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut initial_scans);
        let plan = build_plan(&initial_scans).unwrap();
        assert_eq!(plan.items.len(), 1);
        let item = &plan.items[0];
        let target_id = item.target_id.as_deref().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        for scan in &initial_scans {
            let expected = scan.fingerprint().unwrap();
            create_or_verify_backup(
                scan,
                &backup_dir,
                vec![scan.spec.logical_store.reference().to_string()],
                &expected,
            )
            .unwrap();
        }

        let mut target_store =
            MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
        let mut target_entry = source_entry.clone();
        target_entry.id = target_id.to_string();
        target_entry.metadata = metadata_with_receipt(
            &raw_from_entry(&source_entry),
            receipt_value(item, &plan.plan_id, "target_copied"),
        )
        .unwrap();
        target_store.insert_if_absent(&target_entry).unwrap();
        drop(target_store);

        let mut resumed_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut resumed_scans);
        let (_, outcomes) = apply_plan(&resumed_scans, &plan, &backup_dir).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].phases.contains(&"target_copied".to_string()));
        assert!(outcomes[0]
            .phases
            .contains(&"source_superseded".to_string()));

        let source_after_first = inventory_store(StoreSpec {
            logical_store: LogicalStore::LegacyGlobal,
            addressed_path: Some(source_path.clone()),
            resolution_error: None,
        });
        let source_row = source_after_first.raw_row("source").unwrap();
        assert_eq!(source_row.superseded_by.as_deref(), Some(target_id));
        assert!(source_after_first.raw_row("source").is_some());
        assert!(inventory_store(StoreSpec {
            logical_store: LogicalStore::SharedWiki,
            addressed_path: Some(target_path.clone()),
            resolution_error: None,
        })
        .raw_row(target_id)
        .is_some());

        let source_bytes = db_snapshot(&source_path);
        let target_bytes = db_snapshot(&target_path);
        let backup_bytes = directory_snapshot(&backup_dir);
        let mut replay_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut replay_scans);
        let (_, replay_outcomes) = apply_plan(&replay_scans, &plan, &backup_dir).unwrap();
        assert_eq!(replay_outcomes[0].outcome, "existing_no_op");
        assert_eq!(db_snapshot(&source_path), source_bytes);
        assert_eq!(db_snapshot(&target_path), target_bytes);
        assert_eq!(directory_snapshot(&backup_dir), backup_bytes);
    }

    #[test]
    fn completed_supersession_replay_rejects_missing_deterministic_target() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/missing-target",
                shared_metadata(),
            )],
        );
        create_current_fixture(&target_path, &[]);

        let mut initial_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut initial_scans);
        let plan = build_plan(&initial_scans).unwrap();
        let target_id = plan.items[0].target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        apply_plan(&initial_scans, &plan, &backup_dir).unwrap();

        // Simulate a foreign writer; migration paths do not issue hard deletes.
        let target_connection = Connection::open(&target_path).unwrap();
        assert_eq!(
            target_connection
                .execute(
                    "DELETE FROM memories WHERE id = ?1",
                    rusqlite::params![target_id],
                )
                .unwrap(),
            1
        );
        drop(target_connection);

        let mut replay_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut replay_scans);
        let error = apply_plan(&replay_scans, &plan, &backup_dir)
            .expect_err("a completed source with a missing target must not be a no-op");
        assert!(
            error.contains("deterministic target") || error.contains("source receipt"),
            "{error}"
        );

        let source = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
        let source_row = source.raw_row("source").unwrap();
        assert_eq!(
            source_row.superseded_by.as_deref(),
            Some(target_id.as_str())
        );
        assert_eq!(
            parse_migration_receipt(source_row).unwrap().unwrap().phase,
            MigrationPhase::SourceSuperseded
        );
        assert!(fixture_scan(LogicalStore::SharedWiki, &target_path)
            .raw_row(&target_id)
            .is_none());
    }

    #[test]
    fn supplied_plan_rejects_unkeyed_tamper_after_canonical_rederivation() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry("source", "/wiki/tamper", shared_metadata())],
        );
        create_current_fixture(&target_path, &[]);

        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        validate_plan(&scans, &plan).unwrap();

        let mut tampered = plan.clone();
        tampered.items[0].target_id = Some("wiki-corpus:forged-target".to_string());
        // This is the legacy unkeyed digest an attacker could recompute after
        // editing the serialized item. The current validator must still
        // rederive the item and the complete plan from live stores.
        tampered.plan_id = digest_string(
            &tampered
                .items
                .iter()
                .map(plan_item_material)
                .collect::<Vec<_>>()
                .join("\n"),
        );

        let error = validate_plan(&scans, &tampered).unwrap_err();
        assert!(error.contains("canonical") || error.contains("rederivation"));
    }

    #[cfg(unix)]
    #[test]
    fn backup_and_manifest_reservations_reject_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry("source", "/wiki/backup", shared_metadata())],
        );
        let source = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
        let expected = source.fingerprint().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        let backup_path = backup_dir.join(backup_file_name(
            &source.physical.as_ref().unwrap().physical_id,
        ));
        let backup_victim = directory.path().join("backup-victim");
        symlink(&backup_victim, &backup_path).unwrap();

        let error = create_or_verify_backup(
            &source,
            &backup_dir,
            vec![LogicalStore::LegacyGlobal.reference().to_string()],
            &expected,
        )
        .unwrap_err();
        assert!(error.contains("regular non-symlink"));
        assert!(!backup_victim.exists());

        let manifest_path = backup_dir.join("wiki-corpus-v1-manifest.json");
        let manifest_victim = directory.path().join("manifest-victim");
        let manifest = BackupManifest {
            version: REPORT_VERSION.to_string(),
            status: "backups_verified".to_string(),
            plan_id: "plan".to_string(),
            backup_directory: backup_dir.display().to_string(),
            receipts: Vec::new(),
            migration_receipts: Vec::new(),
        };
        symlink(&manifest_victim, &manifest_path).unwrap();
        let error = write_manifest_if_needed(&manifest_path, &manifest).unwrap_err();
        assert!(error.contains("regular non-symlink"));
        assert!(!manifest_victim.exists());

        let temporary = manifest_path.with_extension("json.tmp");
        symlink(&manifest_victim, &temporary).unwrap();
        fs::remove_file(&manifest_path).unwrap();
        let error = write_manifest_if_needed(&manifest_path, &manifest).unwrap_err();
        assert!(error.contains("regular non-symlink"));
        assert!(!manifest_victim.exists());
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_file_replacement_of_reserved_backup_or_manifest_fails_before_db_mutation() {
        for race_hook in [
            CorpusRaceHook::ReplaceBackupTempWithNormalFile,
            CorpusRaceHook::ReplaceManifestTempWithNormalFile,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let source_path = directory.path().join("legacy.db");
            let target_path = directory.path().join("shared.db");
            create_current_fixture(
                &source_path,
                &[fixture_entry(
                    "source",
                    "/wiki/artifact-race",
                    shared_metadata(),
                )],
            );
            create_current_fixture(&target_path, &[]);
            let mut scans = vec![
                fixture_scan(LogicalStore::LegacyGlobal, &source_path),
                fixture_scan(LogicalStore::SharedWiki, &target_path),
            ];
            classify_scans(&mut scans);
            let plan = build_plan(&scans).unwrap();
            let source_before = db_snapshot_fingerprint(&db_snapshot(&source_path));
            let target_before = db_snapshot_fingerprint(&db_snapshot(&target_path));
            let backup_dir = directory.path().join("backups");
            fs::create_dir(&backup_dir).unwrap();

            let error = apply_plan_with_race_hook(&scans, &plan, &backup_dir, race_hook)
                .expect_err("ordinary-file replacement must fail closed");

            assert!(
                error.contains("identity changed after reservation"),
                "{error}"
            );
            assert_eq!(
                db_snapshot_fingerprint(&db_snapshot(&source_path)),
                source_before
            );
            assert_eq!(
                db_snapshot_fingerprint(&db_snapshot(&target_path)),
                target_before
            );
            assert!(fixture_scan(LogicalStore::SharedWiki, &target_path)
                .raw_row(plan.items[0].target_id.as_deref().unwrap())
                .is_none());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    #[test]
    fn existing_final_backup_replacement_by_ordinary_or_source_hardlink_fails_closed() {
        for replace_with_source_hardlink in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let source_path = directory.path().join("source.db");
            create_current_fixture(
                &source_path,
                &[fixture_entry(
                    "source",
                    "/wiki/existing-backup-race",
                    shared_metadata(),
                )],
            );
            let mut source = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
            classify_scans(std::slice::from_mut(&mut source));
            let expected = source.fingerprint().unwrap();
            let backup_dir = directory.path().join("backups");
            fs::create_dir(&backup_dir).unwrap();
            create_or_verify_backup(
                &source,
                &backup_dir,
                vec![LogicalStore::LegacyGlobal.reference().to_string()],
                &expected,
            )
            .unwrap();
            let backup_path = backup_dir.join(backup_file_name(
                &source.physical.as_ref().unwrap().physical_id,
            ));
            let replacement_path = directory.path().join("replacement.db");
            if replace_with_source_hardlink {
                fs::hard_link(&source_path, &replacement_path).unwrap();
            } else {
                create_current_fixture(
                    &replacement_path,
                    &[fixture_entry(
                        "replacement",
                        "/wiki/ordinary-replacement",
                        json!({"kind": "ordinary_replacement"}),
                    )],
                );
            }
            let source_before = db_snapshot_fingerprint(&db_snapshot(&source_path));
            let mut race_hook =
                Some(CorpusRaceHook::ReplaceExistingBackupAfterRetain { replacement_path });

            let error = create_or_verify_backup_with_hook(
                &source,
                &backup_dir,
                &backup_file_name(&source.physical.as_ref().unwrap().physical_id),
                vec![LogicalStore::LegacyGlobal.reference().to_string()],
                &expected,
                &mut race_hook,
            )
            .err()
            .expect("existing final backup replacement must fail closed");

            assert!(
                error.contains("identity changed after reservation"),
                "{error}"
            );
            assert_eq!(
                db_snapshot_fingerprint(&db_snapshot(&source_path)),
                source_before
            );
            assert!(backup_path.exists());
            let source_after = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
            let row = source_after.raw_row("source").unwrap();
            assert_eq!(row.revision, 1);
            assert!(parse_migration_receipt(row).unwrap().is_none());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    #[test]
    fn retained_backup_replacement_before_first_source_update_fails_closed() {
        for replace_with_source_hardlink in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let source_path = directory.path().join("shared.db");
            create_current_fixture(
                &source_path,
                &[fixture_entry(
                    "source",
                    "/wiki/retained-backup-boundary",
                    shared_metadata(),
                )],
            );
            let mut scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
            classify_scans(&mut scans);
            let plan = build_plan(&scans).unwrap();
            assert_eq!(plan.items.len(), 1);
            assert_eq!(plan.items[0].action, "reclassify_in_place");

            let replacement_path = directory.path().join("replacement.db");
            if replace_with_source_hardlink {
                fs::hard_link(&source_path, &replacement_path).unwrap();
            } else {
                create_current_fixture(
                    &replacement_path,
                    &[fixture_entry(
                        "replacement",
                        "/wiki/ordinary-backup-replacement",
                        json!({"kind": "replacement"}),
                    )],
                );
            }
            let source_before = db_snapshot_fingerprint(&db_snapshot(&source_path));
            let backup_dir = directory.path().join("backups");
            fs::create_dir(&backup_dir).unwrap();

            let error = apply_plan_with_race_hook(
                &scans,
                &plan,
                &backup_dir,
                CorpusRaceHook::ReplaceRetainedBackupBeforeSourceMutation { replacement_path },
            )
            .expect_err("a detached rollback backup must block the first source update");

            assert!(
                error.contains("identity changed after reservation"),
                "{error}"
            );
            assert_eq!(
                db_snapshot_fingerprint(&db_snapshot(&source_path)),
                source_before
            );
            let source = fixture_scan(LogicalStore::SharedWiki, &source_path);
            let row = source.raw_row("source").unwrap();
            assert_eq!(row.revision, 1);
            assert!(parse_migration_receipt(row).unwrap().is_none());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    #[test]
    fn opened_source_or_target_path_swap_fails_before_first_write() {
        for swapped_store in [LogicalStore::LegacyGlobal, LogicalStore::SharedWiki] {
            let directory = tempfile::tempdir().unwrap();
            let source_path = directory.path().join("legacy.db");
            let target_path = directory.path().join("shared.db");
            create_current_fixture(
                &source_path,
                &[fixture_entry(
                    "source",
                    "/wiki/opened-store-race",
                    shared_metadata(),
                )],
            );
            create_current_fixture(&target_path, &[]);
            let replacement_path = directory.path().join("replacement.db");
            create_current_fixture(
                &replacement_path,
                &[fixture_entry(
                    "replacement",
                    "/wiki/replacement",
                    json!({"kind": "replacement"}),
                )],
            );
            let mut scans = vec![
                fixture_scan(LogicalStore::LegacyGlobal, &source_path),
                fixture_scan(LogicalStore::SharedWiki, &target_path),
            ];
            classify_scans(&mut scans);
            let plan = build_plan(&scans).unwrap();
            let target_id = plan.items[0].target_id.as_deref().unwrap().to_string();
            let backup_dir = directory.path().join("backups");
            fs::create_dir(&backup_dir).unwrap();

            let error = apply_plan_with_race_hook(
                &scans,
                &plan,
                &backup_dir,
                CorpusRaceHook::SwapOpenedStorePath {
                    store: swapped_store,
                    replacement_path: replacement_path.clone(),
                },
            )
            .expect_err("detached opened store must fail before the first write");

            assert!(error.contains("detached from logical path"), "{error}");
            let original_source_path = if swapped_store == LogicalStore::LegacyGlobal {
                &replacement_path
            } else {
                &source_path
            };
            let original_target_path = if swapped_store == LogicalStore::SharedWiki {
                &replacement_path
            } else {
                &target_path
            };
            let source_after = fixture_scan(LogicalStore::LegacyGlobal, original_source_path);
            let source_row = source_after.raw_row("source").unwrap();
            assert_eq!(source_row.revision, 1);
            assert!(parse_migration_receipt(source_row).unwrap().is_none());
            assert_eq!(source_row.superseded_by, None);
            assert!(fixture_scan(LogicalStore::SharedWiki, original_target_path)
                .raw_row(&target_id)
                .is_none());
            let replaced_logical_path = if swapped_store == LogicalStore::LegacyGlobal {
                &source_path
            } else {
                &target_path
            };
            assert!(fixture_scan(swapped_store, replaced_logical_path)
                .raw_row("replacement")
                .is_some());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    #[test]
    fn mixed_completion_reclassification_swap_after_receipt_read_cannot_report_success() {
        let directory = tempfile::tempdir().unwrap();
        let shared_path = directory.path().join("shared.db");
        create_current_fixture(
            &shared_path,
            &[
                fixture_entry("a-completed", "/wiki/a", shared_metadata()),
                fixture_entry("z-pending", "/wiki/z", shared_metadata()),
            ],
        );
        let mut initial_scans = vec![fixture_scan(LogicalStore::SharedWiki, &shared_path)];
        classify_scans(&mut initial_scans);
        let plan = build_plan(&initial_scans).unwrap();
        assert_eq!(plan.items.len(), 2);
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        let expected = initial_scans[0].fingerprint().unwrap();
        create_or_verify_backup(
            &initial_scans[0],
            &backup_dir,
            vec![LogicalStore::SharedWiki.reference().to_string()],
            &expected,
        )
        .unwrap();
        let completed = plan
            .items
            .iter()
            .find(|item| item.source_id == "a-completed")
            .unwrap();
        let mut store =
            MemoryStore::open_existing_read_write(&shared_path.display().to_string()).unwrap();
        let entry = store
            .get_with_options(&completed.source_id, true)
            .unwrap()
            .unwrap();
        let row = raw_from_entry(&entry);
        let metadata = metadata_with_receipt(
            &row,
            receipt_value(completed, &plan.plan_id, "reclassified"),
        )
        .unwrap();
        assert!(store
            .update_with_revision(
                &entry.id,
                &entry.text,
                &entry.summary,
                &entry.source,
                &metadata,
                entry.vector.as_deref(),
                entry.revision,
            )
            .unwrap());
        drop(store);

        let mut partial_scans = vec![fixture_scan(LogicalStore::SharedWiki, &shared_path)];
        classify_scans(&mut partial_scans);
        let replacement_path = directory.path().join("replacement.db");
        create_current_fixture(
            &replacement_path,
            &[fixture_entry(
                "replacement",
                "/wiki/reclassification-replacement",
                json!({"kind": "replacement"}),
            )],
        );
        let error = apply_plan_with_race_hook(
            &partial_scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::SwapReclassificationPathAfterReceiptRead {
                source_id: completed.source_id.clone(),
                replacement_path: replacement_path.clone(),
            },
        )
        .expect_err("reclassification must revalidate after its final receipt read");

        assert!(error.contains("detached from logical path"), "{error}");
        let original = fixture_scan(LogicalStore::SharedWiki, &replacement_path);
        let completed_row = original.raw_row("a-completed").unwrap();
        assert_eq!(completed_row.revision, 2);
        assert!(receipt_matches(
            completed_row,
            completed,
            &plan.plan_id,
            &["reclassified"]
        ));
        let pending_row = original.raw_row("z-pending").unwrap();
        assert_eq!(pending_row.revision, 1);
        assert!(parse_migration_receipt(pending_row).unwrap().is_none());
        assert!(fixture_scan(LogicalStore::SharedWiki, &shared_path)
            .raw_row("replacement")
            .is_some());
    }

    #[test]
    fn post_validation_enrichment_cannot_commit_a_stale_reclassification_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let shared_path = directory.path().join("shared.db");
        create_current_fixture(
            &shared_path,
            &[fixture_entry_with_vector(
                "source",
                "/wiki/reclassification-enrichment-race",
                shared_metadata(),
                vec![0.11; 1024],
            )],
        );
        let mut scans = vec![fixture_scan(LogicalStore::SharedWiki, &shared_path)];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].action, "reclassify_in_place");
        let expected = scans[0].fingerprint().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        create_or_verify_backup(
            &scans[0],
            &backup_dir,
            vec![LogicalStore::SharedWiki.reference().to_string()],
            &expected,
        )
        .unwrap();

        let error = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::MutateReclassificationAfterPlanValidation {
                source_id: "source".to_string(),
            },
        )
        .expect_err("post-validation enrichment must invalidate reclassification");

        assert!(
            error.contains("source fingerprint changed during reclassification"),
            "{error}"
        );
        let current = fixture_scan(LogicalStore::SharedWiki, &shared_path);
        let row = current.raw_row("source").unwrap();
        assert_eq!(row.revision, 1);
        assert_eq!(row.summary, "post-validation generated summary");
        assert_ne!(
            row.vector_fingerprint(current.vector_table_present),
            plan.items[0].source_vector
        );
        assert!(parse_migration_receipt(row).unwrap().is_none());
    }

    #[test]
    fn post_validation_valid_until_drift_cannot_commit_a_reclassification_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let shared_path = directory.path().join("shared.db");
        create_current_fixture(
            &shared_path,
            &[fixture_entry(
                "source",
                "/wiki/reclassification-valid-until-race",
                shared_metadata(),
            )],
        );
        let mut scans = vec![fixture_scan(LogicalStore::SharedWiki, &shared_path)];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        create_or_verify_backup(
            &scans[0],
            &backup_dir,
            vec![LogicalStore::SharedWiki.reference().to_string()],
            &scans[0].fingerprint().unwrap(),
        )
        .unwrap();

        let error = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::MutateValidUntilAfterPlanValidation {
                source_id: "source".to_string(),
            },
        )
        .expect_err("same-revision valid_until drift must invalidate reclassification");

        assert!(
            error.contains("source fingerprint changed during reclassification"),
            "{error}"
        );
        let current = fixture_scan(LogicalStore::SharedWiki, &shared_path);
        let row = current.raw_row("source").unwrap();
        assert_eq!(row.revision, 1);
        assert_eq!(row.valid_until.as_deref(), Some("2026-12-31T23:59:59Z"));
        assert!(parse_migration_receipt(row).unwrap().is_none());
        assert_ne!(
            scans[0].row_digest, current.row_digest,
            "valid_until must participate in plan and backup row evidence"
        );
        assert!(validate_plan(std::slice::from_ref(&current), &plan).is_err());
    }

    #[test]
    fn post_precheck_copy_enrichment_marks_the_new_target_noncanonical() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry_with_vector(
                "source",
                "/wiki/copy-enrichment-race",
                shared_metadata(),
                vec![0.11; 1024],
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let item = &plan.items[0];
        let target_id = item.target_id.as_deref().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();

        let error = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::MutateCopySourceAfterPrecheck {
                source_id: "source".to_string(),
            },
        )
        .expect_err("post-precheck source drift must reject the atomic source transition");

        assert!(error.contains("source fingerprint changed"), "{error}");
        let mut current = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut current);
        let source = current[0].raw_row("source").unwrap();
        assert_eq!(source.revision, 1);
        assert_eq!(source.summary, "post-precheck generated summary");
        assert!(parse_migration_receipt(source).unwrap().is_none());
        assert!(source.superseded_by.is_none());
        let target = current[1].raw_row(target_id).unwrap();
        assert!(target.archived);
        assert_eq!(
            parse_migration_receipt(target).unwrap().unwrap().phase,
            MigrationPhase::TargetNoncanonical
        );
        assert!(
            validate_plan(&current, &plan).is_err(),
            "the old plan must not replay against the enriched source"
        );
    }

    #[test]
    fn source_drift_after_receipt_prep_reconciles_target_without_hard_delete() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry_with_vector(
                "source",
                "/wiki/source-receipt-prep-race",
                shared_metadata(),
                vec![0.11; 1024],
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let target_id = plan.items[0].target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();

        let error = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::MutateSourceAfterReceiptPrepared {
                source_id: "source".to_string(),
            },
        )
        .expect_err("source drift must reject the atomic source transition");
        remove_memory_hard_delete_guard(&target_path);
        assert!(
            error.contains("source fingerprint changed before the atomic"),
            "{error}"
        );

        let mut current = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut current);
        let source = current[0].raw_row("source").unwrap();
        assert_eq!(source.revision, 1);
        assert_eq!(source.summary, "source enriched after receipt preparation");
        assert!(source.superseded_by.is_none());
        assert!(parse_migration_receipt(source).unwrap().is_none());
        let target = current[1].raw_row(&target_id).unwrap();
        assert!(
            target.archived,
            "rejected copy target must remain but be noncanonical"
        );
        assert_eq!(
            parse_migration_receipt(target).unwrap().unwrap().phase,
            MigrationPhase::TargetNoncanonical
        );
        let target_revision = target.revision;

        apply_plan(&current, &plan, &backup_dir)
            .expect_err("old plan replay must remain rejected after source drift");
        let replay_target = fixture_scan(LogicalStore::SharedWiki, &target_path);
        assert_eq!(
            replay_target.raw_row(&target_id).unwrap().revision,
            target_revision,
            "replay must converge without repeatedly mutating the noncanonical target"
        );
    }

    #[test]
    fn target_enrichment_after_verification_is_adopted_by_apply_and_replay() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry_with_vector(
                "source",
                "/wiki/target-enrichment-race",
                shared_metadata(),
                vec![0.11; 1024],
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let target_id = plan.items[0].target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();

        let (_, outcomes) = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::MutateTargetAfterVerification {
                source_id: "source".to_string(),
            },
        )
        .expect("mutable target enrichment must remain adoptable");
        assert_eq!(outcomes[0].outcome, "copied_and_superseded");

        let mut completed = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut completed);
        let target = completed[1].raw_row(&target_id).unwrap();
        assert_eq!(target.summary, "target enriched after verification");
        assert_eq!(
            target
                .metadata
                .pointer("/enrichment/status")
                .and_then(Value::as_str),
            Some("complete")
        );
        assert_ne!(target.vector_fingerprint(true), plan.items[0].source_vector);
        assert!(!target.archived);

        let (_, replay) = apply_plan(&completed, &plan, &backup_dir)
            .expect("replay must adopt the enriched deterministic target");
        assert_eq!(replay[0].outcome, "existing_no_op");
    }

    #[test]
    fn foreign_source_supersession_reconciles_target_without_hard_delete() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/foreign-supersession-race",
                shared_metadata(),
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let target_id = plan.items[0].target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();

        let error = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition {
                source_id: "source".to_string(),
            },
        )
        .expect_err("foreign source supersession must win atomically");
        remove_memory_hard_delete_guard(&target_path);
        assert!(error.contains("superseded by foreign target"), "{error}");

        let mut current = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut current);
        let source = current[0].raw_row("source").unwrap();
        assert_eq!(source.superseded_by.as_deref(), Some("foreign-wiki-target"));
        assert!(parse_migration_receipt(source).unwrap().is_none());
        let target = current[1].raw_row(&target_id).unwrap();
        assert!(target.archived);
        assert_eq!(
            parse_migration_receipt(target).unwrap().unwrap().phase,
            MigrationPhase::TargetNoncanonical
        );
        let target_revision = target.revision;

        apply_plan(&current, &plan, &backup_dir)
            .expect_err("old plan replay must converge to the foreign supersession failure");
        assert_eq!(
            fixture_scan(LogicalStore::SharedWiki, &target_path)
                .raw_row(&target_id)
                .unwrap()
                .revision,
            target_revision
        );
    }

    /// Assert the state a losing worker must leave behind after a sibling
    /// worker completed the same deterministic plan item: the winner's target
    /// stays canonical and user-visible, the source keeps the winner's
    /// supersession edge, and a later apply converges on the completed state
    /// instead of dead-ending on the occupant collision check.
    fn assert_sibling_completion_survived(
        source_path: &Path,
        target_path: &Path,
        page_path: &str,
        target_id: &str,
        plan: &WikiCorpusPlan,
        backup_dir: &Path,
    ) {
        let mut current = vec![
            fixture_scan(LogicalStore::LegacyGlobal, source_path),
            fixture_scan(LogicalStore::SharedWiki, target_path),
        ];
        classify_scans(&mut current);
        let source = current[0].raw_row("source").unwrap();
        assert_eq!(source.superseded_by.as_deref(), Some(target_id));
        assert_eq!(
            parse_migration_receipt(source).unwrap().unwrap().phase,
            MigrationPhase::SourceSuperseded
        );
        let target = current[1].raw_row(target_id).unwrap();
        assert!(
            !target.archived,
            "the sibling worker's canonical target must not be archived by the loser"
        );
        assert!(target.superseded_by.is_none());
        assert_eq!(
            parse_migration_receipt(target).unwrap().unwrap().phase,
            MigrationPhase::TargetCopied
        );

        // The user-facing Wiki read surface filters `archived = 0 AND
        // superseded_by IS NULL`, so archiving the target would delete the page
        // from every reader.
        let target_store =
            MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
        let visible = target_store
            .list_user_facing_wiki_entries(page_path, 10, false)
            .unwrap();
        assert_eq!(
            visible
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec![target_id],
            "the migrated page must stay on the user-facing read surface"
        );
        drop(target_store);

        let (_, replay) = apply_plan(&current, plan, backup_dir)
            .expect("replay must converge on the sibling-completed state");
        assert_eq!(replay[0].outcome, "existing_no_op");
    }

    #[test]
    fn sibling_worker_completion_before_target_precheck_is_adopted_not_archived() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry_with_vector(
                "source",
                "/wiki/sibling-worker-precheck-race",
                shared_metadata(),
                vec![0.11; 1024],
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let target_id = plan.items[0].target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();

        let (_, outcomes) = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::SiblingWorkerCompletesItem {
                source_id: "source".to_string(),
                seam: SiblingCompletionSeam::AfterSourcePrecheck,
            },
        )
        .expect("a sibling-completed item must return the completed no-op, not an error");
        assert_eq!(outcomes[0].outcome, "existing_no_op");
        assert_eq!(
            outcomes[0].phases,
            vec!["target_copied".to_string(), "source_superseded".to_string()]
        );

        assert_sibling_completion_survived(
            &source_path,
            &target_path,
            "/wiki/sibling-worker-precheck-race",
            &target_id,
            &plan,
            &backup_dir,
        );
    }

    #[test]
    fn sibling_worker_completion_after_receipt_prep_is_adopted_not_archived() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry_with_vector(
                "source",
                "/wiki/sibling-worker-transition-race",
                shared_metadata(),
                vec![0.11; 1024],
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let target_id = plan.items[0].target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();

        let (_, outcomes) = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::SiblingWorkerCompletesItem {
                source_id: "source".to_string(),
                seam: SiblingCompletionSeam::AfterReceiptPrepared,
            },
        )
        .expect("losing the atomic transition to a sibling must not fail the item");
        assert_eq!(outcomes[0].outcome, "existing_no_op");
        assert_eq!(
            outcomes[0].phases,
            vec!["target_copied".to_string(), "source_superseded".to_string()]
        );

        assert_sibling_completion_survived(
            &source_path,
            &target_path,
            "/wiki/sibling-worker-transition-race",
            &target_id,
            &plan,
            &backup_dir,
        );
    }

    #[test]
    fn vector_state_is_preserved_or_apply_refuses_without_target_capability() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("vector-source.db");
        let target_path = directory.path().join("no-vector-target.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry_with_vector(
                "vector-source",
                "/wiki/vector",
                shared_metadata(),
                vec![0.25; 1024],
            )],
        );
        create_current_fixture(&target_path, &[]);
        let target_conn = Connection::open(&target_path).unwrap();
        target_conn.execute("DROP TABLE memories_vec", []).unwrap();
        drop(target_conn);

        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        assert!(scans[0].vector_table_present);
        assert!(!scans[1].vector_table_present);
        let plan = build_plan(&scans).unwrap();
        let error = validate_plan(&scans, &plan).unwrap_err();
        assert!(error.contains("cannot preserve the source vector"));
        assert_eq!(scans[1].rows.len(), 0);
    }

    #[test]
    fn vector_copy_preserves_the_source_embedding_in_the_target_store() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("vector-source.db");
        let target_path = directory.path().join("vector-target.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry_with_vector(
                "vector-source",
                "/wiki/vector-copy",
                shared_metadata(),
                vec![0.125; 1024],
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        apply_plan(&scans, &plan, &backup_dir).unwrap();

        let mut after_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut after_scans);
        let item = &plan.items[0];
        let target_row = after_scans[1]
            .raw_row(item.target_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(
            target_row.vector_fingerprint(after_scans[1].vector_table_present),
            item.source_vector
        );
    }

    #[test]
    fn vector_mutation_without_memory_revision_invalidates_plan_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("vector-source.db");
        let target_path = directory.path().join("vector-target.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry_with_vector(
                "vector-source",
                "/wiki/vector-mutation",
                shared_metadata(),
                vec![0.25; 1024],
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();

        memcore::db::register_sqlite_vec();
        let source_conn = Connection::open(&source_path).unwrap();
        source_conn
            .execute(
                "UPDATE memories_vec SET embedding = ?1 WHERE id = ?2",
                rusqlite::params![
                    memcore::db::serialize_f32(&vec![0.75; 1024]),
                    "vector-source"
                ],
            )
            .unwrap();
        drop(source_conn);

        let mut changed_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut changed_scans);
        let error = validate_plan(&changed_scans, &plan).unwrap_err();
        assert!(error.contains("fingerprint") || error.contains("vector"));
    }

    #[test]
    fn interrupted_copy_boundaries_reconcile_and_rerun_idempotently() {
        for boundary in [
            MigrationBoundary::TargetCopied,
            MigrationBoundary::SourceSuperseded,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let source_path = directory.path().join("legacy.db");
            let target_path = directory.path().join("shared.db");
            create_current_fixture(
                &source_path,
                &[fixture_entry("source", "/wiki/crash", shared_metadata())],
            );
            create_current_fixture(&target_path, &[]);
            let mut initial_scans = vec![
                fixture_scan(LogicalStore::LegacyGlobal, &source_path),
                fixture_scan(LogicalStore::SharedWiki, &target_path),
            ];
            classify_scans(&mut initial_scans);
            let plan = build_plan(&initial_scans).unwrap();
            let backup_dir = directory.path().join("backups");
            fs::create_dir(&backup_dir).unwrap();
            let interruption = MigrationInterruption {
                source_id: Some("source".to_string()),
                boundary,
            };
            let error =
                apply_plan_with_interruption(&initial_scans, &plan, &backup_dir, interruption)
                    .unwrap_err();
            assert!(error.contains("simulated interruption"));

            let mut partial_scans = vec![
                fixture_scan(LogicalStore::LegacyGlobal, &source_path),
                fixture_scan(LogicalStore::SharedWiki, &target_path),
            ];
            classify_scans(&mut partial_scans);
            let source_row = partial_scans[0].raw_row("source").unwrap();
            let target_id = plan.items[0].target_id.as_deref().unwrap();
            let target_row = partial_scans[1].raw_row(target_id).unwrap();
            assert_eq!(
                parse_migration_receipt(target_row).unwrap().unwrap().phase,
                MigrationPhase::TargetCopied
            );
            match boundary {
                MigrationBoundary::TargetCopied => {
                    assert!(parse_migration_receipt(source_row).unwrap().is_none());
                    assert_eq!(source_row.superseded_by, None);
                }
                MigrationBoundary::SourceSuperseded => {
                    assert_eq!(
                        parse_migration_receipt(source_row).unwrap().unwrap().phase,
                        MigrationPhase::SourceSuperseded
                    );
                    assert_eq!(source_row.superseded_by.as_deref(), Some(target_id));
                }
            }

            let mut resumed_scans = vec![
                fixture_scan(LogicalStore::LegacyGlobal, &source_path),
                fixture_scan(LogicalStore::SharedWiki, &target_path),
            ];
            classify_scans(&mut resumed_scans);
            let (_, outcomes) = apply_plan(&resumed_scans, &plan, &backup_dir).unwrap();
            assert_eq!(outcomes.len(), 1);
            assert!(outcomes[0]
                .phases
                .contains(&"source_superseded".to_string()));

            let mut replay_scans = vec![
                fixture_scan(LogicalStore::LegacyGlobal, &source_path),
                fixture_scan(LogicalStore::SharedWiki, &target_path),
            ];
            classify_scans(&mut replay_scans);
            let (_, replay_outcomes) = apply_plan(&replay_scans, &plan, &backup_dir).unwrap();
            assert_eq!(replay_outcomes[0].outcome, "existing_no_op");
        }
    }

    #[test]
    fn completed_first_item_and_failed_second_item_resume_from_original_backup() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        let mut first_entry = fixture_entry("a", "/wiki/partial-a", shared_metadata());
        first_entry.summary = "partial first summary".to_string();
        first_entry.text = "partial first text".to_string();
        let mut second_entry = fixture_entry("b", "/wiki/partial-b", shared_metadata());
        second_entry.summary = "partial second summary".to_string();
        second_entry.text = "partial second text".to_string();
        create_current_fixture(&source_path, &[first_entry, second_entry]);
        create_current_fixture(&target_path, &[]);
        let mut initial_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut initial_scans);
        let plan = build_plan(&initial_scans).unwrap();
        assert_eq!(plan.items.len(), 2);
        let second = plan
            .items
            .iter()
            .find(|item| item.source_id == "b")
            .unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        let error = apply_plan_with_interruption(
            &initial_scans,
            &plan,
            &backup_dir,
            MigrationInterruption {
                source_id: Some(second.source_id.clone()),
                boundary: MigrationBoundary::TargetCopied,
            },
        )
        .unwrap_err();
        assert!(error.contains("simulated interruption"), "{error}");
        let backup_path = backup_dir.join(backup_file_name(
            &initial_scans[0].physical.as_ref().unwrap().physical_id,
        ));
        let backup_before_resume = db_snapshot(&backup_path);

        let mut partial_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut partial_scans);
        let first = plan
            .items
            .iter()
            .find(|item| item.source_id == "a")
            .unwrap();
        let first_row = partial_scans[0].raw_row(&first.source_id).unwrap();
        assert_eq!(
            parse_migration_receipt(first_row).unwrap().unwrap().phase,
            MigrationPhase::SourceSuperseded
        );
        let second_row = partial_scans[0].raw_row(&second.source_id).unwrap();
        assert!(parse_migration_receipt(second_row).unwrap().is_none());

        let mut resumed_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut resumed_scans);
        let (_, outcomes) = apply_plan(&resumed_scans, &plan, &backup_dir).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes
            .iter()
            .all(|outcome| { outcome.phases.contains(&"source_superseded".to_string()) }));
        assert_eq!(db_snapshot(&backup_path), backup_before_resume);

        let mut final_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut final_scans);
        let (_, replay_outcomes) = apply_plan(&final_scans, &plan, &backup_dir).unwrap();
        assert!(replay_outcomes
            .iter()
            .all(|outcome| outcome.outcome == "existing_no_op"));
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    #[test]
    fn completed_run_command_rejects_post_inventory_logical_path_swap() {
        let directory = tempfile::tempdir().unwrap();
        let global_path = directory.path().join("global.db");
        let project_path = directory.path().join("project.db");
        let app_home = directory.path().join("home");
        let shared_path = app_home.join("projects/wiki/memory.db");
        fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
        create_current_fixture(
            &global_path,
            &[fixture_entry(
                "global",
                "/wiki/completed-replay-race",
                shared_metadata(),
            )],
        );
        create_current_fixture(&project_path, &[]);
        create_current_fixture(&shared_path, &[]);

        let preview = run_wiki_corpus_command(
            false,
            None,
            None,
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .unwrap();
        let plan = preview.plan.unwrap();
        let plan_path = directory.path().join("plan.json");
        fs::write(
            &plan_path,
            serde_json::to_vec_pretty(&json!({"plan": plan})).unwrap(),
        )
        .unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        run_wiki_corpus_command(
            true,
            Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
            Some(backup_dir.clone()),
            Some(plan_path.clone()),
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .unwrap();

        let replacement_path = directory.path().join("replacement.db");
        create_current_fixture(
            &replacement_path,
            &[fixture_entry(
                "replacement",
                "/wiki/post-inventory-replacement",
                json!({"kind": "replacement"}),
            )],
        );
        let shared_before = db_snapshot_fingerprint(&db_snapshot(&shared_path));
        let backups_before = directory_snapshot(&backup_dir);

        let error = run_wiki_corpus_command_with_race_hook(
            true,
            Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
            Some(backup_dir.clone()),
            Some(plan_path),
            &global_path,
            Some(&project_path),
            &app_home,
            CorpusRaceHook::SwapLogicalPathAfterInventory {
                store: LogicalStore::LegacyGlobal,
                replacement_path: replacement_path.clone(),
            },
        )
        .expect_err("completed replay must not trust its stale inventory");

        assert!(
            error.contains("physical binding changed since inventory"),
            "{error}"
        );
        assert_eq!(
            db_snapshot_fingerprint(&db_snapshot(&shared_path)),
            shared_before
        );
        assert_eq!(directory_snapshot(&backup_dir), backups_before);
        assert!(fixture_scan(LogicalStore::LegacyGlobal, &global_path)
            .raw_row("replacement")
            .is_some());
        let original = fixture_scan(LogicalStore::LegacyGlobal, &replacement_path);
        let completed_row = original.raw_row("global").unwrap();
        assert_eq!(
            parse_migration_receipt(completed_row)
                .unwrap()
                .unwrap()
                .phase,
            MigrationPhase::SourceSuperseded
        );
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    #[test]
    fn completed_no_op_rejects_path_swap_at_actual_success_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/completed-success-boundary",
                shared_metadata(),
            )],
        );
        let mut initial_scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
        classify_scans(&mut initial_scans);
        let plan = build_plan(&initial_scans).unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        apply_plan(&initial_scans, &plan, &backup_dir).unwrap();

        let mut completed_scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
        classify_scans(&mut completed_scans);
        assert!(plan
            .items
            .iter()
            .all(|item| plan_item_completed(&completed_scans, &plan, item)));
        let replacement_path = directory.path().join("replacement.db");
        create_current_fixture(
            &replacement_path,
            &[fixture_entry(
                "replacement",
                "/wiki/completed-return-replacement",
                json!({"kind": "replacement"}),
            )],
        );
        let backups_before = directory_snapshot(&backup_dir);

        let error = apply_plan_with_race_hook(
            &completed_scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::SwapLogicalPathBeforeCompletedReturn {
                store: LogicalStore::SharedWiki,
                replacement_path: replacement_path.clone(),
            },
        )
        .expect_err("completed no-op must revalidate its logical path at return");

        assert!(
            error.contains("physical binding changed since inventory"),
            "{error}"
        );
        assert_eq!(directory_snapshot(&backup_dir), backups_before);
        assert!(fixture_scan(LogicalStore::SharedWiki, &source_path)
            .raw_row("replacement")
            .is_some());
        let original = fixture_scan(LogicalStore::SharedWiki, &replacement_path);
        let row = original.raw_row("source").unwrap();
        assert_eq!(row.revision, 2);
        assert!(receipt_matches(
            row,
            &plan.items[0],
            &plan.plan_id,
            &["reclassified"]
        ));
    }

    #[test]
    fn completed_copy_no_op_rejects_target_archive_after_first_proof() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/completed-target-archive",
                shared_metadata(),
            )],
        );
        create_current_fixture(&target_path, &[]);

        let mut initial_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut initial_scans);
        let plan = build_plan(&initial_scans).unwrap();
        let target_id = plan.items[0].target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        apply_plan(&initial_scans, &plan, &backup_dir).unwrap();

        let mut completed_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut completed_scans);
        assert!(plan
            .items
            .iter()
            .all(|item| plan_item_completed(&completed_scans, &plan, item)));

        let error = apply_plan_with_race_hook(
            &completed_scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::ArchiveTargetBeforeCompletedReturn {
                source_id: "source".to_string(),
            },
        )
        .expect_err("completed no-op must reject a target archived after its first proof");

        assert!(
            error.contains("completed Wiki corpus state changed")
                || error.contains("existing_no_op proof"),
            "{error}"
        );
        let target = fixture_scan(LogicalStore::SharedWiki, &target_path);
        assert!(target.raw_row(&target_id).unwrap().archived);
        let source = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
        assert_eq!(
            source.raw_row("source").unwrap().superseded_by.as_deref(),
            Some(target_id.as_str())
        );
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    #[test]
    fn completed_no_op_reverifies_backup_object_instead_of_trusting_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/completed-backup-recheck",
                shared_metadata(),
            )],
        );
        let mut initial_scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
        classify_scans(&mut initial_scans);
        let plan = build_plan(&initial_scans).unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        apply_plan(&initial_scans, &plan, &backup_dir).unwrap();

        let mut completed_scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
        classify_scans(&mut completed_scans);
        let replacement_path = directory.path().join("replacement.db");
        create_current_fixture(
            &replacement_path,
            &[fixture_entry(
                "replacement",
                "/wiki/completed-backup-replacement",
                json!({"kind": "replacement"}),
            )],
        );

        let error = apply_plan_with_race_hook(
            &completed_scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::ReplaceExistingBackupAfterRetain { replacement_path },
        )
        .expect_err("completed replay must reopen and retain its rollback backup");

        assert!(
            error.contains("identity changed after reservation"),
            "{error}"
        );
        let source = fixture_scan(LogicalStore::SharedWiki, &source_path);
        let row = source.raw_row("source").unwrap();
        assert_eq!(row.revision, 2);
        assert!(receipt_matches(
            row,
            &plan.items[0],
            &plan.plan_id,
            &["reclassified"]
        ));
    }

    #[test]
    fn command_boundary_uses_three_physical_stores_and_replays_the_same_plan() {
        let directory = tempfile::tempdir().unwrap();
        let global_path = directory.path().join("global.db");
        let project_path = directory.path().join("project.db");
        let app_home = directory.path().join("home");
        let shared_path = app_home.join("projects/wiki/memory.db");
        fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
        create_current_fixture(
            &global_path,
            &[fixture_entry("global", "/wiki/global", shared_metadata())],
        );
        create_current_fixture(
            &project_path,
            &[fixture_entry("project", "/wiki/project", shared_metadata())],
        );
        create_current_fixture(
            &shared_path,
            &[fixture_entry("shared", "/wiki/shared", shared_metadata())],
        );

        let preview = run_wiki_corpus_command(
            false,
            None,
            None,
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .unwrap();
        assert_eq!(preview.mode, "preview");
        assert_eq!(preview.stores.len(), 3);
        let plan = preview.plan.clone().unwrap();
        assert_eq!(plan.store_fingerprints.len(), 3);
        let physical_ids = plan
            .store_fingerprints
            .iter()
            .map(|fingerprint| fingerprint.physical_id.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(physical_ids.len(), 3);

        let plan_path = directory.path().join("plan.json");
        fs::write(
            &plan_path,
            serde_json::to_vec_pretty(&json!({"plan": plan})).unwrap(),
        )
        .unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        let applied = run_wiki_corpus_command(
            true,
            Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
            Some(backup_dir.clone()),
            Some(plan_path.clone()),
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .unwrap();
        assert_eq!(applied.mode, "apply");
        assert_eq!(applied.stores.len(), 3);
        assert_eq!(applied.migration_outcomes.len(), 3);

        let replay = run_wiki_corpus_command(
            true,
            Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
            Some(backup_dir),
            Some(plan_path),
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .unwrap();
        assert_eq!(replay.migration_outcomes.len(), 3);
        assert!(replay
            .migration_outcomes
            .iter()
            .all(|outcome| outcome.outcome == "existing_no_op"));
    }

    /// Put a completed migration into the exact terminal state the sibling
    /// race used to leave behind: the canonical target is archived and carries
    /// a `target_noncanonical` receipt while its source keeps the supersession
    /// edge and the `source_superseded` receipt.
    fn damage_completed_target_as_sibling_race(
        target_path: &Path,
        target_id: &str,
        item: &PlanItem,
        plan_id: &str,
    ) {
        let mut target_store =
            MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
        reconcile_target_noncanonical(&mut target_store, target_id, item, plan_id).unwrap();
        let damaged = target_store
            .get_with_options(target_id, true)
            .unwrap()
            .unwrap();
        assert!(
            damaged.archived,
            "the damage fixture must archive the target"
        );
    }

    fn user_facing_ids(target_path: &Path, page_path: &str) -> Vec<String> {
        let store =
            MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
        store
            .list_user_facing_wiki_entries(page_path, 10, false)
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect()
    }

    fn corpus_scans(source_path: &Path, target_path: &Path) -> Vec<StoreScan> {
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, source_path),
            fixture_scan(LogicalStore::SharedWiki, target_path),
        ];
        classify_scans(&mut scans);
        scans
    }

    #[test]
    fn sibling_race_damaged_target_is_repaired_back_onto_the_read_surface() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        let page_path = "/wiki/sibling-damage-repair";
        create_current_fixture(
            &source_path,
            &[fixture_entry_with_vector(
                "source",
                page_path,
                shared_metadata(),
                vec![0.11; 1024],
            )],
        );
        create_current_fixture(&target_path, &[]);
        let scans = corpus_scans(&source_path, &target_path);
        let plan = build_plan(&scans).unwrap();
        let item = plan.items[0].clone();
        let target_id = item.target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        let (_, outcomes) = apply_plan(&scans, &plan, &backup_dir).unwrap();
        assert_eq!(outcomes[0].outcome, "copied_and_superseded");
        assert_eq!(
            user_facing_ids(&target_path, page_path),
            vec![target_id.clone()]
        );

        damage_completed_target_as_sibling_race(&target_path, &target_id, &item, &plan.plan_id);
        let damaged = corpus_scans(&source_path, &target_path);
        let damaged_target = damaged[1].raw_row(&target_id).unwrap();
        assert!(damaged_target.archived);
        assert_eq!(
            parse_migration_receipt(damaged_target)
                .unwrap()
                .unwrap()
                .phase,
            MigrationPhase::TargetNoncanonical
        );
        assert!(
            user_facing_ids(&target_path, page_path).is_empty(),
            "the damaged page must be invisible to every reader before the repair"
        );

        // The dead end this repair exists for, with its upgraded diagnosis.
        let error = apply_plan(&damaged, &plan, &backup_dir)
            .expect_err("a damaged target must still dead-end the migration");
        assert!(
            error.contains("deterministic target occupant collision"),
            "{error}"
        );
        assert!(
            error.contains("observed receipt_phase=target_noncanonical archived=true"),
            "{error}"
        );
        assert!(
            error.contains("expected receipt_phase=target_copied archived=false"),
            "{error}"
        );
        assert!(error.contains("--repair-sibling-damage"), "{error}");

        let repair_dir = directory.path().join("repair-backups");
        fs::create_dir(&repair_dir).unwrap();
        let report = repair_sibling_damage(&damaged, Some(&repair_dir)).unwrap();
        assert_eq!(report.inspected_rows, 1);
        assert_eq!(report.repairable, 1);
        assert_eq!(report.repaired, 1);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.rows[0].outcome, "repaired");
        assert!(report.rows[0].skipped_reasons.is_empty());
        assert_eq!(
            report.backups.len(),
            1,
            "the mutated store must be backed up"
        );

        let healed = corpus_scans(&source_path, &target_path);
        let target = healed[1].raw_row(&target_id).unwrap();
        assert!(!target.archived);
        assert!(target.superseded_by.is_none());
        assert_eq!(
            parse_migration_receipt(target).unwrap().unwrap().phase,
            MigrationPhase::TargetCopied,
            "the repaired row must hold the canonical terminal receipt"
        );
        let history = target
            .metadata
            .get(REPAIR_RECEIPT_KEY)
            .and_then(Value::as_array)
            .expect("the repair history must be preserved");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["phase"], json!(REPAIR_PHASE));
        assert_eq!(
            history[0]["replaced_receipt"]["phase"],
            json!("target_noncanonical"),
            "the replaced receipt must be kept verbatim instead of erased"
        );
        assert_eq!(
            user_facing_ids(&target_path, page_path),
            vec![target_id.clone()],
            "the repaired page must be back on the user-facing read surface"
        );

        let (_, replay) = apply_plan(&healed, &plan, &backup_dir)
            .expect("apply must converge on the repaired state instead of colliding");
        assert_eq!(replay[0].outcome, "existing_no_op");
    }

    #[test]
    fn foreign_source_supersession_is_reported_but_never_repaired() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/foreign-supersession-not-repairable",
                shared_metadata(),
            )],
        );
        create_current_fixture(&target_path, &[]);
        let scans = corpus_scans(&source_path, &target_path);
        let plan = build_plan(&scans).unwrap();
        let target_id = plan.items[0].target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();

        apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition {
                source_id: "source".to_string(),
            },
        )
        .expect_err("foreign source supersession must win atomically");
        // The hook installs a hard-delete assertion trigger; the surrounding
        // test fixture drops it exactly like the reconciliation test does.
        remove_memory_hard_delete_guard(&target_path);

        let current = corpus_scans(&source_path, &target_path);
        let damaged_target = current[1].raw_row(&target_id).unwrap();
        assert!(damaged_target.archived);
        assert_eq!(
            parse_migration_receipt(damaged_target)
                .unwrap()
                .unwrap()
                .phase,
            MigrationPhase::TargetNoncanonical
        );
        let target_revision = damaged_target.revision;

        let repair_dir = directory.path().join("repair-backups");
        fs::create_dir(&repair_dir).unwrap();
        let report = repair_sibling_damage(&current, Some(&repair_dir)).unwrap();
        assert_eq!(report.inspected_rows, 1);
        assert_eq!(report.repairable, 0);
        assert_eq!(report.repaired, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.rows[0].outcome, "skipped");
        assert!(
            report.rows[0]
                .skipped_reasons
                .iter()
                .any(|reason| reason.contains("is superseded by foreign-wiki-target")),
            "{:?}",
            report.rows[0].skipped_reasons
        );
        assert!(
            report.rows[0]
                .skipped_reasons
                .iter()
                .any(|reason| reason.contains("does not carry a source_superseded receipt")),
            "{:?}",
            report.rows[0].skipped_reasons
        );
        assert!(
            report.backups.is_empty(),
            "a run with nothing to repair must not touch the backup directory"
        );
        assert!(directory_snapshot(&repair_dir).is_empty());

        let after = corpus_scans(&source_path, &target_path);
        let untouched = after[1].raw_row(&target_id).unwrap();
        assert_eq!(untouched.revision, target_revision);
        assert!(untouched.archived);
        assert_eq!(
            parse_migration_receipt(untouched).unwrap().unwrap().phase,
            MigrationPhase::TargetNoncanonical
        );
    }

    #[test]
    fn archived_target_without_the_noncanonical_receipt_is_reported_not_repaired() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/archived-without-noncanonical-receipt",
                shared_metadata(),
            )],
        );
        create_current_fixture(&target_path, &[]);
        let scans = corpus_scans(&source_path, &target_path);
        let plan = build_plan(&scans).unwrap();
        let target_id = plan.items[0].target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        apply_plan(&scans, &plan, &backup_dir).unwrap();

        // Archive the canonical target while leaving its `target_copied`
        // receipt in place: archived, but not the sibling-race signature.
        let mut target_store =
            MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
        let entry = target_store
            .get_with_options(&target_id, true)
            .unwrap()
            .unwrap();
        let metadata = entry.metadata.clone();
        let expected = ExpectedMemoryState::from_entry(&entry, None);
        assert!(target_store
            .archive_with_metadata_if_expected_state(&target_id, &metadata, &expected)
            .unwrap());
        drop(target_store);

        let current = corpus_scans(&source_path, &target_path);
        let revision = current[1].raw_row(&target_id).unwrap().revision;
        let repair_dir = directory.path().join("repair-backups");
        fs::create_dir(&repair_dir).unwrap();
        let report = repair_sibling_damage(&current, Some(&repair_dir)).unwrap();
        assert_eq!(report.inspected_rows, 1);
        assert_eq!(report.repairable, 0);
        assert_eq!(report.repaired, 0);
        assert_eq!(report.rows[0].outcome, "skipped");
        assert_eq!(
            report.rows[0].observed_receipt_phase.as_deref(),
            Some("target_copied")
        );
        assert!(
            report.rows[0]
                .skipped_reasons
                .iter()
                .any(|reason| reason
                    .contains("receipt phase is target_copied, not target_noncanonical")),
            "{:?}",
            report.rows[0].skipped_reasons
        );

        let after = corpus_scans(&source_path, &target_path);
        let untouched = after[1].raw_row(&target_id).unwrap();
        assert_eq!(untouched.revision, revision);
        assert!(untouched.archived);
    }

    #[test]
    fn copy_identity_drift_is_reported_but_never_repaired() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        let page_path = "/wiki/copy-identity-drift";
        create_current_fixture(
            &source_path,
            &[fixture_entry("source", page_path, shared_metadata())],
        );
        create_current_fixture(&target_path, &[]);
        let scans = corpus_scans(&source_path, &target_path);
        let plan = build_plan(&scans).unwrap();
        let item = plan.items[0].clone();
        let target_id = item.target_id.clone().unwrap();
        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        apply_plan(&scans, &plan, &backup_dir).unwrap();

        damage_completed_target_as_sibling_race(&target_path, &target_id, &item, &plan.plan_id);

        // Every other clause of the repair signature now matches; drift the
        // target's text directly (a foreign writer, not the repair
        // machinery) so its copy identity stops matching the receipt's
        // captured source identity. This is the clause that binds the row to
        // *this content*, not merely to this row's shape, and it is the one
        // future refactors of `copy_identity_sha256` are most likely to
        // silently break.
        let target_connection = Connection::open(&target_path).unwrap();
        assert_eq!(
            target_connection
                .execute(
                    "UPDATE memories SET text = ?1 WHERE id = ?2",
                    rusqlite::params!["drifted after the receipt was captured", target_id],
                )
                .unwrap(),
            1
        );
        drop(target_connection);

        let current = corpus_scans(&source_path, &target_path);
        let damaged_target = current[1].raw_row(&target_id).unwrap();
        assert!(damaged_target.archived);
        let target_revision = damaged_target.revision;

        let repair_dir = directory.path().join("repair-backups");
        fs::create_dir(&repair_dir).unwrap();
        let report = repair_sibling_damage(&current, Some(&repair_dir)).unwrap();
        assert_eq!(report.inspected_rows, 1);
        assert_eq!(report.repairable, 0);
        assert_eq!(report.repaired, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.rows[0].outcome, "skipped");
        assert!(
            report.rows[0]
                .skipped_reasons
                .iter()
                .any(|reason| reason.contains("does not match the receipt item's")),
            "{:?}",
            report.rows[0].skipped_reasons
        );
        assert!(
            report.backups.is_empty(),
            "a run with nothing to repair must not touch the backup directory"
        );
        assert!(directory_snapshot(&repair_dir).is_empty());

        let after = corpus_scans(&source_path, &target_path);
        let untouched = after[1].raw_row(&target_id).unwrap();
        assert_eq!(untouched.revision, target_revision);
        assert!(untouched.archived);
        assert_eq!(
            parse_migration_receipt(untouched).unwrap().unwrap().phase,
            MigrationPhase::TargetNoncanonical,
            "an unrepaired row must keep the damage signature intact for a later, correct repair"
        );
    }

    #[test]
    fn sibling_repair_command_dry_runs_before_it_writes() {
        let directory = tempfile::tempdir().unwrap();
        let global_path = directory.path().join("global.db");
        let project_path = directory.path().join("project.db");
        let app_home = directory.path().join("home");
        let shared_path = app_home.join("projects/wiki/memory.db");
        let page_path = "/wiki/global-repair";
        fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
        create_current_fixture(
            &global_path,
            &[fixture_entry("global", page_path, shared_metadata())],
        );
        create_current_fixture(&project_path, &[]);
        create_current_fixture(&shared_path, &[]);

        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
        let applied = run_wiki_corpus_command(
            true,
            Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
            Some(backup_dir.clone()),
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .unwrap();
        let plan = applied.plan.clone().unwrap();
        let item = plan.items[0].clone();
        let target_id = item.target_id.clone().unwrap();
        damage_completed_target_as_sibling_race(&shared_path, &target_id, &item, &plan.plan_id);

        let repair_dir = directory.path().join("repair-backups");
        fs::create_dir(&repair_dir).unwrap();
        let before = db_snapshot_fingerprint(&db_snapshot(&shared_path));

        let refused = run_wiki_corpus_sibling_repair_command(
            true,
            Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
            Some(repair_dir.clone()),
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .expect_err("repair must refuse to ride along with --apply");
        assert!(
            refused.contains("cannot be combined with --apply"),
            "{refused}"
        );
        let refused = run_wiki_corpus_sibling_repair_command(
            false,
            Some("MIGRATE_WIKI_CORPUS_V1".to_string()),
            Some(repair_dir.clone()),
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .expect_err("the apply token must not confirm a repair");
        assert!(refused.contains("requires exact --confirm"), "{refused}");
        let refused = run_wiki_corpus_sibling_repair_command(
            false,
            None,
            Some(repair_dir.clone()),
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .expect_err("a backup directory without a token must not write");
        assert!(
            refused.contains("--backup-dir requires --confirm"),
            "{refused}"
        );
        let refused = run_wiki_corpus_sibling_repair_command(
            false,
            Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
            None,
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .expect_err("a confirmed repair without --backup-dir must not write");
        assert!(
            refused.contains("requires explicit --backup-dir"),
            "{refused}"
        );
        let refused = run_wiki_corpus_sibling_repair_command(
            false,
            Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
            Some(directory.path().join("does-not-exist")),
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .expect_err("a confirmed repair with a nonexistent backup directory must not write");
        assert!(
            refused.contains("requires an existing backup directory"),
            "{refused}"
        );

        let preview = run_wiki_corpus_sibling_repair_command(
            false,
            None,
            None,
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .unwrap();
        assert_eq!(preview.mode, "sibling_repair_preview");
        assert!(!preview.apply);
        let preview_repair = preview.sibling_repair.clone().unwrap();
        assert!(!preview_repair.confirmed);
        assert_eq!(preview_repair.repairable, 1);
        assert_eq!(preview_repair.repaired, 0);
        assert_eq!(preview_repair.rows[0].outcome, "repairable");
        assert_eq!(preview_repair.rows[0].id, target_id);
        assert!(preview_repair.backups.is_empty());
        assert!(preview_repair.backup_directory.is_none());
        assert_eq!(
            db_snapshot_fingerprint(&db_snapshot(&shared_path)),
            before,
            "the dry run must not write a single byte"
        );
        assert!(directory_snapshot(&repair_dir).is_empty());
        assert!(user_facing_ids(&shared_path, page_path).is_empty());

        let repaired = run_wiki_corpus_sibling_repair_command(
            false,
            Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
            Some(repair_dir.clone()),
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .unwrap();
        assert_eq!(repaired.mode, "sibling_repair");
        assert!(repaired.apply);
        let repaired_report = repaired.sibling_repair.clone().unwrap();
        assert!(repaired_report.confirmed);
        assert_eq!(repaired_report.repaired, 1);
        assert_eq!(repaired_report.rows[0].outcome, "repaired");
        assert_eq!(repaired_report.backups.len(), 1);
        assert_eq!(user_facing_ids(&shared_path, page_path), vec![target_id]);

        // A second confirmed run has nothing left to inspect.
        let again = run_wiki_corpus_sibling_repair_command(
            false,
            Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
            Some(repair_dir),
            None,
            &global_path,
            Some(&project_path),
            &app_home,
        )
        .unwrap();
        let again = again.sibling_repair.clone().unwrap();
        assert_eq!(again.inspected_rows, 0);
        assert_eq!(again.repaired, 0);
    }

    // -----------------------------------------------------------------
    // `--adopt-legacy` (tachi#1624)
    // -----------------------------------------------------------------

    /// The shape the host runs: a legacy global, no bound project DB, and a
    /// Tachi home with no `wiki` store yet.
    struct AdoptionFixture {
        _directory: tempfile::TempDir,
        global_path: PathBuf,
        app_home: PathBuf,
    }

    impl AdoptionFixture {
        fn new(entries: &[MemoryEntry]) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let global_path = directory.path().join("global.db");
            let app_home = directory.path().join("home");
            fs::create_dir_all(&app_home).unwrap();
            create_current_fixture(&global_path, entries);
            Self {
                _directory: directory,
                global_path,
                app_home,
            }
        }

        fn target(&self) -> PathBuf {
            legacy_adoption_target_path(&self.app_home)
        }

        fn target_dir(&self) -> PathBuf {
            self.target().parent().unwrap().to_path_buf()
        }

        fn run(&self, confirm: Option<&str>) -> Result<WikiCorpusReport, String> {
            run_wiki_corpus_legacy_adoption_command(
                false,
                confirm.map(str::to_string),
                None,
                None,
                &self.global_path,
                None,
                &self.app_home,
            )
        }

        fn adopt(&self) -> LegacyAdoptionReport {
            self.run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
                .expect("confirmed adoption")
                .legacy_adoption
                .expect("legacy adoption report")
        }
    }

    fn adoption_entry_fixture(id: &str, path: &str, metadata: Value) -> MemoryEntry {
        let mut entry = fixture_entry(id, path, metadata);
        // Distinct bodies: memcore's write path consolidates near-duplicates
        // above a token-Jaccard of 0.9, and every `fixture_entry` shares one
        // literal body.
        entry.text = format!("adoption fixture body for {id} at {path}");
        entry.summary = format!("adoption fixture summary {id}");
        entry
    }

    /// Overwrite the lifecycle columns `upsert` stamps with wall clock, so a
    /// test can assert verbatim preservation of values a write path would
    /// never produce.
    #[allow(clippy::too_many_arguments)]
    fn force_legacy_lifecycle(
        path: &Path,
        id: &str,
        created_at: &str,
        updated_at: &str,
        revision: i64,
        archived: bool,
        valid_until: Option<&str>,
        superseded_by: Option<&str>,
    ) {
        let conn = Connection::open(path).unwrap();
        let changed = conn
            .execute(
                "UPDATE memories
                 SET created_at = ?2, updated_at = ?3, revision = ?4, archived = ?5,
                     valid_until = ?6, superseded_by = ?7
                 WHERE id = ?1",
                rusqlite::params![
                    id,
                    created_at,
                    updated_at,
                    revision,
                    archived,
                    valid_until,
                    superseded_by
                ],
            )
            .unwrap();
        assert_eq!(changed, 1, "fixture row {id} must exist");
    }

    /// Round-2 bug C. Overwrite the usage-counter columns directly, bypassing
    /// `upsert` the same way `force_legacy_lifecycle` does: the ordinary
    /// write path does not accept caller-supplied `access_count`/
    /// `scored_count`/`last_access`/`last_use_at` (they are bumped by the
    /// search/recall path, never set by a write), so this is the only way to
    /// build a fixture carrying values a legacy row genuinely accumulated
    /// before adoption.
    fn force_legacy_usage_counters(
        path: &Path,
        id: &str,
        access_count: i64,
        scored_count: i64,
        last_access: Option<&str>,
        last_use_at: Option<&str>,
    ) {
        let conn = Connection::open(path).unwrap();
        let changed = conn
            .execute(
                "UPDATE memories
                 SET access_count = ?2, scored_count = ?3, last_access = ?4, last_use_at = ?5
                 WHERE id = ?1",
                rusqlite::params![id, access_count, scored_count, last_access, last_use_at],
            )
            .unwrap();
        assert_eq!(changed, 1, "fixture row {id} must exist");
    }

    /// Overwrite the `path` column directly, bypassing `upsert`'s
    /// `normalize_path` call so the fixture can hold a raw legacy path the
    /// current write path would never itself persist -- the shape genuinely
    /// legacy data (written before normalization existed, or by a path
    /// outside this write seam) can carry.
    fn force_legacy_path(path: &Path, id: &str, raw_path: &str) {
        let conn = Connection::open(path).unwrap();
        let changed = conn
            .execute(
                "UPDATE memories SET path = ?2 WHERE id = ?1",
                rusqlite::params![id, raw_path],
            )
            .unwrap();
        assert_eq!(changed, 1, "fixture row {id} must exist");
    }

    struct DestinationRow {
        created_at: String,
        updated_at: String,
        revision: i64,
        archived: bool,
        valid_until: Option<String>,
        superseded_by: Option<String>,
        metadata: Value,
        path: String,
        // Round-2 bug C.
        access_count: i64,
        scored_count: i64,
        last_access: Option<String>,
        last_use_at: Option<String>,
    }

    fn destination_row(target: &Path, id: &str) -> DestinationRow {
        let conn = Connection::open(target).unwrap();
        conn.query_row(
            "SELECT created_at, updated_at, revision, archived, valid_until, superseded_by,
                    metadata, path, access_count, scored_count, last_access, last_use_at
             FROM memories WHERE id = ?1",
            [id],
            |row| {
                let metadata: String = row.get(6)?;
                Ok(DestinationRow {
                    created_at: row.get(0)?,
                    updated_at: row.get(1)?,
                    revision: row.get(2)?,
                    archived: row.get::<_, i64>(3)? != 0,
                    valid_until: row.get(4)?,
                    superseded_by: row.get(5)?,
                    metadata: serde_json::from_str(&metadata).unwrap(),
                    path: row.get(7)?,
                    access_count: row.get(8)?,
                    scored_count: row.get(9)?,
                    last_access: row.get(10)?,
                    last_use_at: row.get(11)?,
                })
            },
        )
        .unwrap_or_else(|error| panic!("adopted row {id} must exist: {error}"))
    }

    /// Give an already-seeded row an id the ordinary write path would refuse,
    /// which is the only way to build a fixture for E3. Renaming beats a raw
    /// `INSERT`: the reserved-reference insert guard is a trigger over
    /// `memories`, and preparing an `INSERT` on a connection that has not
    /// registered memcore's guard function would fail for reasons unrelated to
    /// what is being tested.
    fn rename_legacy_row_id(path: &Path, from: &str, to: &str) {
        let conn = Connection::open(path).unwrap();
        let changed = conn
            .execute(
                "UPDATE memories SET id = ?2 WHERE id = ?1",
                rusqlite::params![from, to],
            )
            .unwrap();
        assert_eq!(changed, 1, "fixture row {from} must exist");
    }

    /// T1
    #[test]
    fn adopt_legacy_refuses_wrong_confirm_token() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "adopt-token",
            "/wiki/adopt/token",
            json!({"lifecycle": "active"}),
        )]);

        let error = fixture
            .run(Some(WIKI_CORPUS_CONFIRMATION_TOKEN))
            .expect_err("the migration token must not confirm an adoption");
        assert!(
            error.contains(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN),
            "{error}"
        );
        assert!(
            fs::symlink_metadata(fixture.target()).is_err(),
            "a refused adoption must not create the store"
        );
    }

    /// T2
    #[test]
    fn adopt_legacy_refuses_apply_plan_and_backup_dir() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "adopt-flags",
            "/wiki/adopt/flags",
            json!({"lifecycle": "active"}),
        )]);
        let token = Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN.to_string());

        let error = run_wiki_corpus_legacy_adoption_command(
            true,
            token.clone(),
            None,
            None,
            &fixture.global_path,
            None,
            &fixture.app_home,
        )
        .expect_err("--adopt-legacy must refuse to ride along with --apply");
        assert_eq!(
            error,
            "--adopt-legacy is its own confirmed mode and cannot be combined with --apply"
        );
        assert!(fs::symlink_metadata(fixture.target()).is_err());

        let error = run_wiki_corpus_legacy_adoption_command(
            false,
            token.clone(),
            None,
            Some(fixture.app_home.join("plan.json")),
            &fixture.global_path,
            None,
            &fixture.app_home,
        )
        .expect_err("--adopt-legacy takes no plan");
        assert_eq!(
            error,
            "--adopt-legacy does not take --plan; the adoption set is derived from the legacy \
             store's own classification"
        );
        assert!(fs::symlink_metadata(fixture.target()).is_err());

        let error = run_wiki_corpus_legacy_adoption_command(
            false,
            token,
            Some(fixture.app_home.join("backups")),
            None,
            &fixture.global_path,
            None,
            &fixture.app_home,
        )
        .expect_err("--adopt-legacy takes no backup directory");
        assert_eq!(
            error,
            "--adopt-legacy does not take --backup-dir; it never writes to an existing store — \
             back up the legacy global out-of-band before running"
        );
        assert!(fs::symlink_metadata(fixture.target()).is_err());
    }

    /// T3
    #[test]
    fn adopt_legacy_preview_creates_nothing() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "adopt-preview",
            "/wiki/adopt/preview",
            json!({"lifecycle": "active"}),
        )]);
        let before = directory_snapshot(&fixture.app_home);

        let report = fixture.run(None).expect("preview must succeed");
        assert_eq!(report.mode, "legacy_adoption_preview");
        assert!(!report.apply);
        let adoption = report.legacy_adoption.expect("legacy adoption report");

        assert!(!adoption.confirmed);
        assert!(!adoption.target_created);
        assert!(!adoption.target_existed_before);
        assert_eq!(adoption.rows_imported, 0);
        assert_eq!(adoption.observed_lifecycle_checksum, None);
        assert_eq!(adoption.checksums_match, None);
        assert!(adoption.eligible_rows > 0);
        assert!(!adoption.expected_lifecycle_checksum.is_empty());
        assert_eq!(adoption.default_retrievable_rows, adoption.eligible_rows);
        assert_eq!(
            adoption.derived_lifecycle_counts.get("active").copied(),
            Some(adoption.eligible_rows)
        );
        assert!(
            fs::symlink_metadata(fixture.target()).is_err(),
            "preview must create nothing"
        );
        assert_eq!(directory_snapshot(&fixture.app_home), before);
    }

    /// T4
    #[test]
    fn adopt_legacy_refuses_when_wiki_store_already_exists() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "adopt-occupied",
            "/wiki/adopt/occupied",
            json!({"lifecycle": "active"}),
        )]);
        fs::create_dir_all(fixture.target_dir()).unwrap();
        create_current_fixture(&fixture.target(), &[]);
        let occupant = db_snapshot_fingerprint(&db_snapshot(&fixture.target()));

        let error = fixture
            .run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
            .expect_err("adoption is bootstrap-only");
        assert!(error.contains("bootstrap-only mode"), "{error}");
        assert!(
            error.contains("remove") && error.contains("re-run"),
            "the remedy must be removal and re-run, never --apply: {error}"
        );
        assert!(
            !error.contains("--apply"),
            "--apply refuses any absent involved store on a --no-project-db host: {error}"
        );
        assert_eq!(
            db_snapshot_fingerprint(&db_snapshot(&fixture.target())),
            occupant,
            "the refused run must not touch the existing store"
        );

        // The preview stays a safe probe against an occupied home.
        let preview = fixture.run(None).expect("preview must still report");
        let adoption = preview.legacy_adoption.unwrap();
        assert!(adoption.target_existed_before);
        assert!(!adoption.target_created);
    }

    /// T4b, round-2 bug A: the existing-target refusal must fire before the
    /// legacy store is opened at all, not merely before the destination is
    /// written. Proven with a discriminator rather than instrumentation: the
    /// legacy source is corrupted after the fixture is built, so *if* the
    /// confirmed path ever opened it, `inventory_store` would capture a read
    /// failure and the command would surface "legacy global store is
    /// unreadable" instead of the bootstrap-only refusal. Getting the
    /// bootstrap-only wording proves the legacy open never happened.
    #[test]
    fn adopt_legacy_refuses_existing_target_before_touching_legacy_store() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "adopt-order",
            "/wiki/adopt/order",
            json!({"lifecycle": "active"}),
        )]);
        fs::create_dir_all(fixture.target_dir()).unwrap();
        create_current_fixture(&fixture.target(), &[]);

        // Corrupt the legacy source only after the fixture (and the
        // preexisting target) are built. A second confirmed run against an
        // occupied target with a legacy source that would error if opened is
        // exactly the shape a refusal-ordering regression would mishandle.
        fs::write(&fixture.global_path, b"not a sqlite database").unwrap();

        let error = fixture
            .run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
            .expect_err("adoption is bootstrap-only");
        assert!(
            error.contains("bootstrap-only mode"),
            "the existing-target refusal must fire before the corrupt legacy store is ever \
             opened: {error}"
        );
        assert!(
            !error.contains("legacy global store is unreadable"),
            "a legacy-read error here would mean the legacy store was opened before the \
             existing-target check: {error}"
        );
    }

    /// T5
    #[test]
    fn adopt_legacy_eligibility_partition() {
        let fixture = AdoptionFixture::new(&[
            adoption_entry_fixture("row-a", "/wiki/a", json!({"lifecycle": "active"})),
            adoption_entry_fixture(
                "row-b",
                "/wiki/b",
                json!({"knowledge_scope": "project", "lifecycle": "active"}),
            ),
            // `rem` would be the more natural operational marker, but the
            // ordinary write path strips it from caller-supplied metadata, so
            // a fixture seeded through `upsert` cannot carry it.
            adoption_entry_fixture(
                "row-c",
                "/wiki/c",
                json!({"operational_snapshot": true, "lifecycle": "active"}),
            ),
            adoption_entry_fixture(
                "row-d",
                "/wiki/d",
                json!({"authority_record": true, "lifecycle": "active"}),
            ),
            adoption_entry_fixture(
                "row-e",
                "/kanban/e",
                json!({"artifact_kind": "wiki", "lifecycle": "active"}),
            ),
            adoption_entry_fixture("row-f", "/wiki/f", json!({"lifecycle": "active"})),
        ]);
        // E3's only case E1 does not already shadow. A `wiki-rem:` id or a
        // `/wiki/_log` row carries an operational marker and is excluded by E1
        // first; an `anchor:`-prefixed row on a wiki path is not, so it is the
        // one that proves the reserved-identity rule is load-bearing.
        rename_legacy_row_id(&fixture.global_path, "row-f", "anchor:f");

        let report = fixture.run(None).expect("preview");
        let adoption = report.legacy_adoption.unwrap();

        assert_eq!(adoption.adopted_ids, vec!["row-a".to_string()]);
        assert_eq!(adoption.eligible_rows, 1);
        let reasons = adoption
            .skipped
            .iter()
            .map(|skip| (skip.id.as_str(), skip.reason.as_str()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            reasons,
            BTreeMap::from([
                ("anchor:f", "reserved_identity"),
                ("row-b", "classification=project_bound"),
                ("row-c", "classification=operational_snapshot"),
                ("row-d", "classification=authority_record"),
                ("row-e", "path_not_public_knowledge_artifact"),
            ])
        );
        // B3: the six seeded rows are all wiki-related; no `classification_missing`
        // entry may appear, and the count is the classified rows, not the table.
        assert_eq!(adoption.wiki_related_rows, 6);
        assert!(adoption
            .skipped
            .iter()
            .all(|skip| skip.reason != "classification_missing"));
    }

    /// B3 directly: a legacy store holding non-wiki rows must not inflate
    /// `wiki_related_rows` or bury the skip histogram, because `load_rows`
    /// selects the whole `memories` table with no wiki predicate.
    #[test]
    fn adopt_legacy_ignores_rows_the_classifier_calls_non_wiki() {
        let mut kanban = adoption_entry_fixture("kanban-row", "/kanban/card", json!({}));
        kanban.category = "kanban".to_string();
        kanban.source = "kanban".to_string();
        kanban.domain = None;
        let fixture = AdoptionFixture::new(&[
            adoption_entry_fixture("wiki-row", "/wiki/kept", json!({"lifecycle": "active"})),
            kanban,
        ]);

        let adoption = fixture.run(None).expect("preview").legacy_adoption.unwrap();
        assert_eq!(
            adoption.wiki_related_rows, 1,
            "only classified rows are part of this corpus"
        );
        assert!(
            adoption.skipped.is_empty(),
            "a non-wiki row is not a skipped adoption candidate: {:?}",
            adoption.skipped
        );
        assert_eq!(adoption.adopted_ids, vec!["wiki-row".to_string()]);
    }

    /// T6
    #[test]
    fn adopt_legacy_preserves_lifecycle_verbatim() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "verbatim",
            "/wiki/adopt/verbatim",
            json!({"lifecycle": "active"}),
        )]);
        force_legacy_lifecycle(
            &fixture.global_path,
            "verbatim",
            "2019-01-01T00:00:00Z",
            "2020-02-02T00:00:00Z",
            7,
            true,
            Some(""),
            None,
        );

        let adoption = fixture.adopt();
        assert_eq!(adoption.checksums_match, Some(true));
        assert_eq!(adoption.rows_imported, 1);

        let stored = destination_row(&fixture.target(), "verbatim");
        assert_eq!(stored.created_at, "2019-01-01T00:00:00Z");
        assert_eq!(stored.updated_at, "2020-02-02T00:00:00Z");
        assert_eq!(stored.revision, 7);
        assert!(stored.archived);
        assert_eq!(
            stored.valid_until,
            Some(String::new()),
            "an empty string is a distinct value from NULL and must survive as such"
        );
        assert_eq!(stored.superseded_by, None);
        assert_eq!(stored.path, "/wiki/adopt/verbatim");
    }

    /// T6b, round-2 bug C: `access_count`/`scored_count`/`last_access`/
    /// `last_use_at` are usage history, not lifecycle policy, but
    /// `import_snapshot_batch` writes them straight from the `MemoryEntry`
    /// it is handed (memcore `snapshot_import.rs`). The loader used to
    /// hardcode all four to zero/None regardless of what the legacy row
    /// actually carried, so every adopted row silently lost its usage
    /// history while `checksums_match` still passed -- the lifecycle
    /// checksum's coverage is `archived`/`created_at`/`id`/`revision`/
    /// `superseded_by`/`updated_at`/`valid_until` only (see the field
    /// comment on `RawRow`), deliberately not widened here to cover these
    /// four; this test is the thing that would catch a regression, not the
    /// checksum.
    #[test]
    fn adopt_legacy_preserves_usage_counters_verbatim() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "usage-verbatim",
            "/wiki/adopt/usage-verbatim",
            json!({"lifecycle": "active"}),
        )]);
        force_legacy_usage_counters(
            &fixture.global_path,
            "usage-verbatim",
            42,
            17,
            Some("2024-03-01T00:00:00Z"),
            Some("2024-03-02T00:00:00Z"),
        );

        let adoption = fixture.adopt();
        assert_eq!(adoption.rows_imported, 1);

        let stored = destination_row(&fixture.target(), "usage-verbatim");
        assert_eq!(stored.access_count, 42);
        assert_eq!(stored.scored_count, 17);
        assert_eq!(stored.last_access, Some("2024-03-01T00:00:00Z".to_string()));
        assert_eq!(stored.last_use_at, Some("2024-03-02T00:00:00Z".to_string()));
    }

    /// T7
    #[test]
    fn adopt_legacy_reports_dangling_supersession() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "dangling",
            "/wiki/adopt/dangling",
            json!({"lifecycle": "active"}),
        )]);
        force_legacy_lifecycle(
            &fixture.global_path,
            "dangling",
            "2019-01-01T00:00:00Z",
            "2019-01-01T00:00:00Z",
            1,
            false,
            None,
            Some("not-adopted"),
        );

        let adoption = fixture.adopt();
        assert_eq!(adoption.rows_imported, 1);
        assert!(!adoption.had_failures, "a dangling edge is never fatal");
        assert_eq!(adoption.dangling_supersessions.len(), 1);
        assert_eq!(adoption.dangling_supersessions[0].id, "dangling");
        assert_eq!(
            adoption.dangling_supersessions[0].superseded_by,
            "not-adopted"
        );

        assert_eq!(
            destination_row(&fixture.target(), "dangling").superseded_by,
            Some("not-adopted".to_string()),
            "the edge is preserved, never repaired"
        );
    }

    /// T8
    #[test]
    fn adopt_legacy_marker_is_the_only_metadata_delta() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "marker",
            "/wiki/adopt/marker",
            json!({
                "lifecycle": "active",
                "artifact_kind": "wiki",
                "nested": {"b": 1, "a": [true, null, "x"]},
            }),
        )]);

        let adoption = fixture.adopt();
        assert_eq!(
            adoption.provenance_marker_key,
            WIKI_LEGACY_ADOPTION_MARKER_KEY
        );

        // Compared against the source row **as stored**, not against the
        // literal this test passed in: the ordinary write path that seeded the
        // fixture has its own metadata sanitization, and the claim under test
        // is that adoption adds nothing beyond the marker to whatever the
        // legacy store actually holds.
        let stored_source = legacy_source_rows(&fixture.global_path)["marker"]["metadata"]
            .as_str()
            .map(|raw| serde_json::from_str::<Value>(raw).unwrap())
            .expect("source metadata");
        assert!(
            stored_source
                .as_object()
                .is_some_and(|object| object.contains_key("nested")),
            "the fixture must actually carry the metadata it claims: {stored_source}"
        );

        let metadata = destination_row(&fixture.target(), "marker").metadata;
        let mut object = metadata.as_object().expect("object metadata").clone();
        let marker = object
            .remove(WIKI_LEGACY_ADOPTION_MARKER_KEY)
            .expect("adoption marker");
        assert_eq!(
            Value::Object(object),
            stored_source,
            "the marker must be the only metadata delta"
        );

        assert_eq!(marker["review_status"], json!("review_pending"));
        assert_eq!(marker["reviewed"], json!(false));
        assert_eq!(
            marker["confirm_token"],
            json!(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN)
        );
        assert_eq!(marker["source_id"], json!("marker"));
        assert_eq!(marker["source_store"], json!("legacy_global"));
        assert_eq!(marker["adoption_run_id"], json!(adoption.adoption_run_id));

        // No assertion is smuggled in at metadata top level. `review_status`
        // in particular would demote the derived lifecycle if it lived here.
        for forbidden in [
            "review_status",
            "reviewed",
            "knowledge_scope",
            "origin_projects",
            "applies_to",
            "applicability_status",
            "review_receipt",
            "source_bundle_hash",
            RECEIPT_KEY,
        ] {
            assert!(
                metadata.get(forbidden).is_none(),
                "adoption must not write a top-level '{forbidden}'"
            );
        }
    }

    /// T9
    #[test]
    fn adopt_legacy_never_mutates_the_source() {
        let fixture = AdoptionFixture::new(&[
            adoption_entry_fixture(
                "src-one",
                "/wiki/adopt/src-one",
                json!({"lifecycle": "active"}),
            ),
            adoption_entry_fixture(
                "src-two",
                "/wiki/adopt/src-two",
                json!({"lifecycle": "active"}),
            ),
        ]);
        let before = legacy_source_rows(&fixture.global_path);

        let adoption = fixture.adopt();
        assert_eq!(adoption.rows_imported, 2);
        assert_eq!(
            adoption.legacy_row_digest_after,
            Some(adoption.legacy_row_digest_before.clone()),
            "adoption is read-only on the legacy global"
        );
        assert_eq!(legacy_source_rows(&fixture.global_path), before);
    }

    /// T10
    #[test]
    fn adopt_legacy_stamps_store_identity_role_wiki() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "stamped",
            "/wiki/adopt/stamped",
            json!({"lifecycle": "active"}),
        )]);

        let adoption = fixture.adopt();
        assert!(adoption.target_created);
        assert_eq!(adoption.target_store_role_stamped, Some(true));
        assert_eq!(adoption.target_removed_after_failure, None);

        let target = fixture.target();
        let reopened = MemoryStore::open_existing_read_write(target.to_str().unwrap())
            .expect("reopen the adopted store");
        assert!(
            reopened.is_wiki_corpus_store(),
            "the role must be derived from the stamp on a plain reopen"
        );
        drop(reopened);

        // The stamp itself, read out of `hard_state` rather than inferred from
        // the label the bootstrap passed in.
        let conn = Connection::open(&target).unwrap();
        let stamp: String = conn
            .query_row(
                "SELECT value_json FROM hard_state
                 WHERE namespace = 'store_identity' AND key = 'role'",
                [],
                |row| row.get(0),
            )
            .expect("store_identity role stamp");
        let stamp: Value = serde_json::from_str(&stamp).unwrap();
        assert_eq!(
            stamp["value"],
            json!(memcore::path_router::WIKI_CORPUS_DB_LABEL)
        );
        assert_eq!(stamp["conferred_by"], json!("open:create-fresh"));

        let profile: String = conn
            .query_row(
                "SELECT value_json FROM hard_state
                 WHERE namespace = 'store_identity' AND key = 'profile'",
                [],
                |row| row.get(0),
            )
            .expect("store_identity profile stamp");
        let profile: Value = serde_json::from_str(&profile).unwrap();
        assert_eq!(profile["conferred_by"], json!("open:create-fresh"));
    }

    /// T11
    #[test]
    fn adopt_legacy_preserves_vectors() {
        let mut vectored = adoption_entry_fixture(
            "vectored",
            "/wiki/adopt/vectored",
            json!({"lifecycle": "active"}),
        );
        vectored.vector = Some(vec![0.25_f32; crate::status_ops::EXPECTED_EMBEDDING_DIM]);
        let plain =
            adoption_entry_fixture("plain", "/wiki/adopt/plain", json!({"lifecycle": "active"}));
        let fixture = AdoptionFixture::new(&[vectored, plain]);

        let adoption = fixture.adopt();
        assert_eq!(adoption.rows_imported, 2);
        assert_eq!(adoption.vectors_imported, 1);
        assert_eq!(adoption.vectors_absent, 1);
        assert_eq!(
            adoption.observed_vector_checksum,
            Some(adoption.expected_vector_checksum.clone())
        );
        assert_eq!(adoption.checksums_match, Some(true));
    }

    /// T12
    #[test]
    fn adopt_legacy_refuses_to_create_an_empty_store() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "all-project",
            "/wiki/adopt/all-project",
            json!({"knowledge_scope": "project", "lifecycle": "active"}),
        )]);

        let error = fixture
            .run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
            .expect_err("an empty adoption set must not create a store");
        assert_eq!(
            error,
            "no eligible legacy rows to adopt; refusing to create an empty wiki store"
        );
        assert!(
            fs::symlink_metadata(fixture.target_dir()).is_err(),
            "the store directory must not exist"
        );
    }

    /// T13 (rewritten): the reachable retrievability guard. A store whose
    /// adopted rows all derive a non-retrievable lifecycle would flip the
    /// named-store existence gate -- silencing the zero-store search refusal
    /// -- without making a single row findable.
    #[test]
    fn adopt_legacy_refuses_when_no_adopted_row_would_be_retrievable() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "pending-only",
            "/wiki/adopt/pending-only",
            json!({"lifecycle": "pending_review"}),
        )]);

        let preview = fixture.run(None).expect("preview").legacy_adoption.unwrap();
        assert_eq!(preview.eligible_rows, 1);
        assert_eq!(preview.default_retrievable_rows, 0);
        assert_eq!(
            preview
                .derived_lifecycle_counts
                .get("pending_review")
                .copied(),
            Some(1)
        );

        let error = fixture
            .run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
            .expect_err("a store nothing can be read out of must not be created");
        assert!(
            error.contains("default-retrievable") && error.contains("zero-store"),
            "{error}"
        );
        assert!(fs::symlink_metadata(fixture.target_dir()).is_err());
    }

    /// The path column is outside the lifecycle checksum, so a silent rewrite
    /// would otherwise verify clean. It is disclosed before the run and
    /// re-read after it.
    #[test]
    fn adopt_legacy_discloses_and_verifies_path_normalization() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "rewritten",
            "/wiki/adopt/rewritten",
            json!({"lifecycle": "active"}),
        )]);
        // `AdoptionFixture::new` seeds the legacy row through `upsert`, which
        // normalizes `path` on write (memcore's `normalize_path` call in
        // `upsert_prepared_within_tx`) -- so no fixture path passed through
        // that constructor can ever land in the legacy DB un-normalized.
        // Force the raw legacy path in directly, the same idiom
        // `force_legacy_lifecycle` uses for columns the current write path
        // would never itself produce.
        force_legacy_path(&fixture.global_path, "rewritten", "/wiki/adopt/rewritten/");

        let preview = fixture.run(None).expect("preview").legacy_adoption.unwrap();
        assert_eq!(preview.path_rewrites.len(), 1);
        assert_eq!(preview.path_rewrites[0].id, "rewritten");
        assert_eq!(
            preview.path_rewrites[0].source_path,
            "/wiki/adopt/rewritten/"
        );
        assert_eq!(
            preview.path_rewrites[0].stored_path,
            "/wiki/adopt/rewritten"
        );

        let adoption = fixture.adopt();
        assert!(!adoption.had_failures);
        assert_eq!(
            destination_row(&fixture.target(), "rewritten").path,
            "/wiki/adopt/rewritten"
        );
    }

    /// B4, stated by the receipt rather than left for a later reader: after
    /// adoption every adopted path lives in two logical stores, and the
    /// duplicate-path pass pins both copies to `manual_review`.
    #[test]
    fn adopt_legacy_reports_that_it_pins_the_reconciler_to_manual_review() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "pinned",
            "/wiki/adopt/pinned",
            json!({"lifecycle": "active"}),
        )]);

        let adoption = fixture.adopt();
        assert!(adoption
            .reconciler_impact
            .contains("build_plan only emits shared_candidate items"));
        assert_eq!(
            adoption.legacy_rows_forced_to_manual_review_by_duplicate_path,
            Some(1),
            "the duplicate-path pass must be measured, not asserted"
        );

        // And the mechanism itself: the next preview sees the row as a
        // cross-store duplicate in both stores.
        let preview = run_wiki_corpus_command(
            false,
            None,
            None,
            None,
            &fixture.global_path,
            None,
            &fixture.app_home,
        )
        .expect("post-adoption corpus preview");
        for store_ref in ["legacy_global", "named:wiki"] {
            let store = preview
                .stores
                .iter()
                .find(|store| store.logical_store_ref == store_ref)
                .unwrap_or_else(|| panic!("{store_ref} must be inventoried"));
            let row = store
                .rows
                .iter()
                .find(|row| row.id == "pinned")
                .unwrap_or_else(|| panic!("{store_ref} must hold the adopted row"));
            assert_eq!(row.classification, CorpusClassification::ManualReview);
            assert!(row
                .reasons
                .iter()
                .any(|reason| reason == "duplicate_normalized_path_across_logical_stores"));
        }
    }

    /// The three pre-existing modes must keep their exact JSON shape: the new
    /// field is `skip_serializing_if = "Option::is_none"`, so `legacy_adoption`
    /// may not appear in a preview, apply, or repair report.
    #[test]
    fn adoption_field_is_absent_from_every_other_mode() {
        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "shape",
            "/wiki/adopt/shape",
            json!({"lifecycle": "active"}),
        )]);

        for report in [
            run_wiki_corpus_command(
                false,
                None,
                None,
                None,
                &fixture.global_path,
                None,
                &fixture.app_home,
            )
            .expect("preview"),
            run_wiki_corpus_sibling_repair_command(
                false,
                None,
                None,
                None,
                &fixture.global_path,
                None,
                &fixture.app_home,
            )
            .expect("repair preview"),
        ] {
            assert!(!report.legacy_adoption_had_failures());
            let value = serde_json::to_value(&report).unwrap();
            assert!(
                value.get("legacy_adoption").is_none(),
                "legacy_adoption must not appear in mode '{}'",
                report.mode
            );
        }
    }

    /// Round-2 bug B: a preexisting, empty `target_dir` -- no db file inside,
    /// so `target_existed_before` is `false` and the confirmed run proceeds
    /// past the Bug A gate -- is not this run's to remove wholesale on
    /// failure. Only the db file (and its WAL/SHM sidecars) this run itself
    /// writes belong to it. Forced deterministically: the preexisting
    /// `target_dir` is made read-only before the confirmed run, so
    /// `MemoryStore::open_with_label_and_context`'s `create_fresh()` cannot
    /// write the db file into it and `adopt_into_bootstrapped_store` fails at
    /// its very first step, before anything is written.
    #[cfg(unix)]
    #[test]
    fn adopt_legacy_failure_cleanup_spares_preexisting_dir() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
            "adopt-preexisting-dir",
            "/wiki/adopt/preexisting-dir",
            json!({"lifecycle": "active"}),
        )]);
        let target_dir = fixture.target_dir();
        fs::create_dir_all(&target_dir).unwrap();
        fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o500)).unwrap();

        // Elevated privileges (e.g. root in some CI containers) bypass
        // directory write-permission checks entirely, which would make this
        // probe meaningless (the bootstrap write would silently succeed).
        // Detect that up front and skip rather than assert something
        // environment-dependent.
        let probe_path = target_dir.join("permission-probe");
        let permission_enforced = File::create(&probe_path).is_err();
        let _ = fs::remove_file(&probe_path);
        if !permission_enforced {
            fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o700)).unwrap();
            eprintln!(
                "skipping adopt_legacy_failure_cleanup_spares_preexisting_dir: target_dir \
                 write permission was not enforced (root?)"
            );
            return;
        }

        let report = fixture.adopt();
        fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(report.had_failures);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("cannot bootstrap wiki store")),
            "expected a bootstrap failure: {:?}",
            report.errors
        );
        assert!(
            target_dir.is_dir(),
            "the preexisting target_dir must survive the failure cleanup"
        );
        assert!(
            !fixture.target().exists(),
            "no db file may remain inside the preexisting target_dir"
        );
        assert_eq!(
            report.target_removed_after_failure,
            Some(true),
            "nothing was written before the bootstrap failed, so file-only removal is a no-op \
             success"
        );
        let remediation = report.remediation.expect("remediation must be set");
        assert!(
            remediation.contains("preexisted this run") && remediation.contains("untouched"),
            "remediation must state the preexisting-dir semantics: {remediation}"
        );
    }

    fn legacy_source_rows(path: &Path) -> BTreeMap<String, Value> {
        let conn = Connection::open(path).unwrap();
        let mut statement = conn
            .prepare(
                "SELECT id, path, revision, archived, metadata, superseded_by, created_at,
                        updated_at, valid_until
                 FROM memories ORDER BY id ASC",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    json!({
                        "path": row.get::<_, String>(1)?,
                        "revision": row.get::<_, i64>(2)?,
                        "archived": row.get::<_, i64>(3)?,
                        "metadata": row.get::<_, String>(4)?,
                        "superseded_by": row.get::<_, Option<String>>(5)?,
                        "created_at": row.get::<_, String>(6)?,
                        "updated_at": row.get::<_, String>(7)?,
                        "valid_until": row.get::<_, Option<String>>(8)?,
                    }),
                ))
            })
            .unwrap()
            .collect::<Result<BTreeMap<_, _>, _>>()
            .unwrap();
        rows
    }
}

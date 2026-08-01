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
use memcore::{InsertMemoryResult, MemoryEntry, MemoryStore};
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
const REPORT_VERSION: &str = "wiki_corpus_migration_v1";
const RECEIPT_KEY: &str = "wiki_corpus_migration";
const RECEIPT_VERSION: u32 = 2;
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

impl CorpusClassification {
    fn as_str(self) -> &'static str {
        match self {
            Self::SharedCandidate => "shared_candidate",
            Self::ProjectBound => "project_bound",
            Self::OperationalSnapshot => "operational_snapshot",
            Self::TestEphemeral => "test_ephemeral",
            Self::AuthorityRecord => "authority_record",
            Self::ManualReview => "manual_review",
        }
    }
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
    SourceReceipted,
    SourceSuperseded,
    Reclassified,
}

impl MigrationPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::TargetCopied => "target_copied",
            Self::SourceReceipted => "source_receipted",
            Self::SourceSuperseded => "source_superseded",
            Self::Reclassified => "reclassified",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationBoundary {
    TargetCopied,
    SourceReceipted,
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

    fn vector_fingerprint(&self, table_present: bool) -> VectorFingerprint {
        VectorFingerprint::from_vector(table_present, self.vector.as_deref())
    }

    fn migration_receipt(&self) -> Option<&Map<String, Value>> {
        self.metadata.get(RECEIPT_KEY).and_then(Value::as_object)
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
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
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

fn validate_target_occupant(
    item: &PlanItem,
    plan_id: &str,
    source: &RawRow,
    target: &StoreScan,
) -> Result<(), String> {
    let Some(target_id) = item.target_id.as_deref() else {
        return Ok(());
    };
    let Some(occupant) = target.raw_row(target_id) else {
        return Ok(());
    };
    let target_vector = occupant.vector_fingerprint(target.vector_table_present);
    let vector_matches = if source.vector_fingerprint(true).row_present {
        target_vector == source.vector_fingerprint(target.vector_table_present)
    } else {
        !target_vector.row_present
    };
    if occupant.content_sha256() != source.content_sha256()
        || !vector_matches
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
    source_superseded_by: Option<String>,
) -> PlanItem {
    let mut expected = build_plan_item(source, row, target);
    expected.source_revision = source_revision;
    expected.source_superseded_by = source_superseded_by.clone();
    let mut identity_row = row.clone();
    identity_row.revision = source_revision;
    identity_row.superseded_by = source_superseded_by;
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
                        && row.superseded_by == item.source_superseded_by;
                    let supersession_observed = row.revision == item.source_revision + 2
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
                        || row.revision != item.source_revision + 3
                        || row.superseded_by != item.target_id
                    {
                        return Err(format!(
                            "Wiki corpus source_superseded state is not proven for {}:{}",
                            item.source_store_ref, item.source_id
                        ));
                    }
                }
                MigrationPhase::Planned | MigrationPhase::TargetCopied => {
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
    if row.content_sha256() != item.source_content_sha256 {
        return Err(format!(
            "target {} content does not match its source",
            row.id
        ));
    }
    let target_vector = row.vector_fingerprint(target.vector_table_present);
    if item.source_vector.row_present {
        if target_vector != item.source_vector {
            return Err(format!("target {} vector was not preserved", row.id));
        }
    } else if target_vector.row_present {
        return Err(format!("target {} has an unexpected vector", row.id));
    }
    if row.revision != item.source_revision || row.superseded_by.is_some() {
        return Err(format!(
            "target {} lifecycle state is not canonical",
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
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    verify_backups_before_source_mutation(retained_backups, race_hook)?;
    let changed = store
        .update_with_revision(
            &entry.id,
            &entry.text,
            &entry.summary,
            &entry.source,
            &metadata,
            entry.vector.as_deref(),
            entry.revision,
        )
        .map_err(|error| error.to_string())?;
    verify_retained_backups(retained_backups)?;
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    if !changed {
        return Err(format!(
            "source revision CAS failed for {}:{}",
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
    let mut source_store = open_apply_store(source_scan)?;
    let mut target_store = open_apply_store(target_scan)?;
    maybe_swap_opened_store_path(race_hook, source_scan.spec.logical_store, source_path)?;
    maybe_swap_opened_store_path(race_hook, target_scan.spec.logical_store, target_path)?;
    verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
    let mut source_entry = source_store
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
    let expected_source_revision = item.source_revision
        + if source_superseded_receipted {
            3
        } else if initial_supersession.is_some() {
            2
        } else if source_receipted {
            1
        } else {
            0
        };
    if source_entry.revision != expected_source_revision
        || source_row.content_sha256() != item.source_content_sha256
        || source_row.vector_fingerprint(source_scan.vector_table_present) != item.source_vector
    {
        return Err(format!(
            "source revision/content changed during apply for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    let mut phases = Vec::new();

    if let Some(target_entry) = target_store
        .get_with_options(target_id, true)
        .map_err(|error| error.to_string())?
    {
        let target_row = raw_from_entry(&target_entry);
        if target_row.content_sha256() != item.source_content_sha256
            || target_entry.revision != item.source_revision
            || !receipt_matches(&target_row, item, plan_id, &["target_copied"])
            || if item.source_vector.row_present {
                target_row.vector_fingerprint(target_scan.vector_table_present)
                    != item.source_vector
            } else {
                target_row
                    .vector_fingerprint(target_scan.vector_table_present)
                    .row_present
            }
        {
            return Err(format!(
                "deterministic target occupant collision: {target_id} content, vector, lifecycle, or receipt mismatch"
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
        let verified_target_row = raw_from_entry(&verified_target);
        if verified_target_row.content_sha256() != item.source_content_sha256
            || verified_target.revision != item.source_revision
            || !receipt_matches(&verified_target_row, item, plan_id, &["target_copied"])
            || if item.source_vector.row_present {
                verified_target_row.vector_fingerprint(target_scan.vector_table_present)
                    != item.source_vector
            } else {
                verified_target_row
                    .vector_fingerprint(target_scan.vector_table_present)
                    .row_present
            }
        {
            return Err(
                "target insert produced a mismatched deterministic occupant or vector".to_string(),
            );
        }
        phases.push("target_copied".to_string());
    }
    verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
    maybe_interrupt(interruption, item, MigrationBoundary::TargetCopied)?;

    let current_supersession = source_store
        .supersession_target(&source_entry.id)
        .map_err(|error| error.to_string())?
        .flatten();
    if current_supersession.as_deref() == Some(target_id) && !source_receipted {
        return Err(format!(
            "source {}:{} is superseded without a durable source receipt",
            item.source_store_ref, item.source_id
        ));
    }
    if current_supersession.as_deref() == Some(target_id) && source_superseded_receipted {
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_retained_backups(retained_backups)?;
        phases.push("source_superseded".to_string());
        return Ok(MigrationOutcome {
            source_store_ref: item.source_store_ref.clone(),
            source_id: item.source_id.clone(),
            target_store_ref: item.target_store_ref.clone(),
            target_id: Some(target_id.to_string()),
            action: item.action.clone(),
            outcome: "existing_no_op".to_string(),
            phases,
        });
    }

    if !source_receipted {
        let metadata = metadata_with_receipt(
            &source_row,
            receipt_value(item, plan_id, "source_receipted"),
        )?;
        let expected_revision = source_entry.revision;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_backups_before_source_mutation(retained_backups, race_hook)?;
        if !source_store
            .update_with_revision(
                &source_entry.id,
                &source_entry.text,
                &source_entry.summary,
                &source_entry.source,
                &metadata,
                source_entry.vector.as_deref(),
                expected_revision,
            )
            .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "source receipt revision CAS failed for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
        verify_retained_backups(retained_backups)?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        source_entry = source_store
            .get_with_options(&item.source_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "source disappeared after source receipt".to_string())?;
        let source_receipt_row = raw_from_entry(&source_entry);
        if !receipt_matches(&source_receipt_row, item, plan_id, &["source_receipted"])
            || source_entry.revision != item.source_revision + 1
        {
            return Err("source receipt was not durably recorded".to_string());
        }
        phases.push("source_receipted".to_string());
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        maybe_interrupt(interruption, item, MigrationBoundary::SourceReceipted)?;
    }

    let current_supersession = source_store
        .supersession_target(&source_entry.id)
        .map_err(|error| error.to_string())?
        .flatten();
    if current_supersession.is_none() {
        let expected_revision = source_entry.revision;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_backups_before_source_mutation(retained_backups, race_hook)?;
        if !source_store
            .supersede_memory_if_revision(&source_entry.id, target_id, expected_revision)
            .map_err(|error| error.to_string())?
        {
            let observed = source_store
                .supersession_target(&item.source_id)
                .map_err(|error| error.to_string())?
                .flatten();
            if observed.as_deref() != Some(target_id) {
                return Err(format!(
                    "source supersession revision CAS failed for {}:{}",
                    item.source_store_ref, item.source_id
                ));
            }
        }
        verify_retained_backups(retained_backups)?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
    }
    let observed = source_store
        .supersession_target(&item.source_id)
        .map_err(|error| error.to_string())?
        .flatten();
    if observed.as_deref() != Some(target_id) {
        return Err(format!(
            "source supersession was not durably recorded for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
    maybe_interrupt(interruption, item, MigrationBoundary::SourceSuperseded)?;

    source_entry = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "source disappeared after supersession".to_string())?;
    let final_source_row = raw_from_entry(&source_entry);
    let final_receipt = parse_migration_receipt(&final_source_row)?;
    if !final_receipt.as_ref().is_some_and(|receipt| {
        receipt.plan_id == plan_id
            && receipt.item == *item
            && receipt.phase == MigrationPhase::SourceSuperseded
    }) {
        let metadata = metadata_with_receipt(
            &final_source_row,
            receipt_value(item, plan_id, "source_superseded"),
        )?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_backups_before_source_mutation(retained_backups, race_hook)?;
        if !source_store
            .update_with_revision(
                &source_entry.id,
                &source_entry.text,
                &source_entry.summary,
                &source_entry.source,
                &metadata,
                source_entry.vector.as_deref(),
                source_entry.revision,
            )
            .map_err(|error| error.to_string())?
        {
            let reread = source_store
                .get_with_options(&item.source_id, true)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "source disappeared while finalizing receipt".to_string())?;
            let reread_row = raw_from_entry(&reread);
            if !receipt_matches(&reread_row, item, plan_id, &["source_superseded"])
                || source_store
                    .supersession_target(&item.source_id)
                    .map_err(|error| error.to_string())?
                    .flatten()
                    .as_deref()
                    != Some(target_id)
            {
                return Err(format!(
                    "source superseded receipt revision CAS failed for {}:{}",
                    item.source_store_ref, item.source_id
                ));
            }
        }
        verify_retained_backups(retained_backups)?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
    }
    let final_source = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "source disappeared after final receipt".to_string())?;
    let final_row = raw_from_entry(&final_source);
    if !receipt_matches(&final_row, item, plan_id, &["source_superseded"])
        || final_source.revision != item.source_revision + 3
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
        return Ok(RetainedFileIdentity::Unix { device, inode });
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
        return Ok(RetainedFileIdentity::Windows { volume, index });
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
    let mut race_hook = None;
    create_or_verify_backup_with_hook(
        source,
        backup_dir,
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
    let backup_path = backup_dir.join(backup_file_name(&source_physical.physical_id));
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
            receipt_matches(row, item, &plan.plan_id, &["source_superseded"])
                && item
                    .target_id
                    .as_deref()
                    .is_some_and(|target_id| row.is_superseded_by(target_id))
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
    for (_physical_id, (mut logical_refs, source)) in groups {
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
        validate_apply_inventory(scans)?;
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
    })
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
        assert!(error.contains("stored Some(18), expected 28"));
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
            MigrationBoundary::SourceReceipted,
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
                MigrationBoundary::SourceReceipted => {
                    assert_eq!(
                        parse_migration_receipt(source_row).unwrap().unwrap().phase,
                        MigrationPhase::SourceReceipted
                    );
                    assert_eq!(source_row.superseded_by, None);
                }
                MigrationBoundary::SourceSuperseded => {
                    assert_eq!(
                        parse_migration_receipt(source_row).unwrap().unwrap().phase,
                        MigrationPhase::SourceReceipted
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
}

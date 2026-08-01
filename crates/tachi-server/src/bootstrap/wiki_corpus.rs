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
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub(crate) const WIKI_CORPUS_CONFIRMATION_TOKEN: &str = "MIGRATE_WIKI_CORPUS_V1";
const REPORT_VERSION: &str = "wiki_corpus_migration_v1";
const RECEIPT_KEY: &str = "wiki_corpus_migration";
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

fn preview_staging_dir() -> Result<PathBuf, String> {
    let root = std::env::temp_dir();
    let process_id = std::process::id();
    for attempt in 0..32u64 {
        let sequence = PREVIEW_SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(
            "sigil-wiki-corpus-preview-{process_id}-{sequence}-{attempt}"
        ));
        match fs::create_dir(&path) {
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PlanStoreFingerprint {
    logical_store_ref: String,
    physical_id: String,
    canonical_path: String,
    schema: u32,
    total_memory_rows: usize,
    row_digest: String,
    rows: Vec<PlanRowFingerprint>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        Some(PlanStoreFingerprint {
            logical_store_ref: self.spec.logical_store.reference().to_string(),
            physical_id: physical.physical_id.clone(),
            canonical_path: physical.canonical_path.clone(),
            schema,
            total_memory_rows: self.report.counts.total_memory_rows,
            row_digest: self.row_digest.clone(),
            rows: self.rows.iter().map(plan_row_fingerprint).collect(),
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

fn load_rows(conn: &Connection) -> Result<(Vec<RawRow>, usize), String> {
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
    let count = rows.len();
    Ok((rows, count))
}

fn plan_row_fingerprint(row: &RawRow) -> PlanRowFingerprint {
    PlanRowFingerprint {
        id: row.id.clone(),
        revision: row.revision,
        content_sha256: row.content_sha256(),
        superseded_by: row.superseded_by.clone(),
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

fn row_digest(rows: &[RawRow]) -> String {
    row_digest_values(
        rows.iter()
            .map(|row| serde_json::to_value(plan_row_fingerprint(row)).unwrap_or(Value::Null))
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
        vector: None,
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
    let (rows, _) = match load_rows(&conn) {
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
    scan.row_digest = row_digest(&scan.rows);
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
    format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        item.action,
        item.source_store_ref,
        item.source_physical_id,
        item.source_id,
        item.source_path,
        item.normalized_path,
        item.source_revision,
        item.source_content_sha256,
        item.replay_identity,
        item.target_store_ref,
        item.target_id.as_deref().unwrap_or_default(),
    )
}

fn build_plan(scans: &[StoreScan]) -> Result<WikiCorpusPlan, String> {
    let target = find_scan(scans, LogicalStore::SharedWiki.reference())?;
    let target_physical_id = target
        .physical
        .as_ref()
        .map(|physical| physical.physical_id.clone())
        .unwrap_or_default();
    let mut items = Vec::new();
    for scan in scans {
        let Some(source_physical) = scan.physical.as_ref() else {
            continue;
        };
        for row in scan
            .rows
            .iter()
            .filter(|row| row.classification == Some(CorpusClassification::SharedCandidate))
        {
            let source_ref = scan.spec.logical_store.reference().to_string();
            let material = format!(
                "wiki-corpus-replay-v1\0{}\0{}\0{}\0{}\0{}\0{}",
                source_ref,
                source_physical.physical_id,
                row.id,
                row.revision,
                row.normalized_path(),
                row.content_sha256(),
            );
            let replay_identity = digest_string(&material);
            let target_id = (scan.spec.logical_store != LogicalStore::SharedWiki)
                .then(|| format!("wiki-corpus:{replay_identity}"));
            items.push(PlanItem {
                action: if scan.spec.logical_store == LogicalStore::SharedWiki {
                    "reclassify_in_place".to_string()
                } else {
                    "copy_to_shared_and_supersede".to_string()
                },
                source_store_ref: source_ref,
                source_physical_id: source_physical.physical_id.clone(),
                source_id: row.id.clone(),
                source_path: row.path.clone(),
                normalized_path: row.normalized_path(),
                source_revision: row.revision,
                source_content_sha256: row.content_sha256(),
                replay_identity,
                target_store_ref: LogicalStore::SharedWiki.reference().to_string(),
                target_physical_id: target_physical_id.clone(),
                target_id,
            });
        }
    }
    items.sort_by(|left, right| {
        left.source_store_ref
            .cmp(&right.source_store_ref)
            .then_with(|| left.source_id.cmp(&right.source_id))
            .then_with(|| left.replay_identity.cmp(&right.replay_identity))
    });
    let plan_id = digest_string(
        &items
            .iter()
            .map(plan_item_material)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let store_fingerprints = scans
        .iter()
        .filter_map(StoreScan::fingerprint)
        .collect::<Vec<_>>();
    Ok(WikiCorpusPlan {
        version: REPORT_VERSION.to_string(),
        plan_id,
        store_fingerprints,
        items,
    })
}

fn receipt_value(item: &PlanItem, plan_id: &str, phase: &str) -> Value {
    json!({
        "kind": RECEIPT_KEY,
        "version": 1,
        "plan_id": plan_id,
        "phase": phase,
        "classification": CorpusClassification::SharedCandidate.as_str(),
        "replay_identity": item.replay_identity,
        "source_store_ref": item.source_store_ref,
        "source_physical_id": item.source_physical_id,
        "source_id": item.source_id,
        "source_revision": item.source_revision,
        "source_content_sha256": item.source_content_sha256,
        "target_store_ref": item.target_store_ref,
        "target_id": item.target_id,
    })
}

fn receipt_matches(row: &RawRow, item: &PlanItem, plan_id: &str, phases: &[&str]) -> bool {
    let Some(receipt) = row.migration_receipt() else {
        return false;
    };
    let string_field = |key: &str| receipt.get(key).and_then(Value::as_str);
    let number_field = |key: &str| receipt.get(key).and_then(Value::as_i64);
    string_field("kind") == Some(RECEIPT_KEY)
        && number_field("version") == Some(1)
        && string_field("plan_id") == Some(plan_id)
        && string_field("classification") == Some(CorpusClassification::SharedCandidate.as_str())
        && string_field("replay_identity") == Some(item.replay_identity.as_str())
        && string_field("source_store_ref") == Some(item.source_store_ref.as_str())
        && string_field("source_physical_id") == Some(item.source_physical_id.as_str())
        && string_field("source_id") == Some(item.source_id.as_str())
        && number_field("source_revision") == Some(item.source_revision)
        && string_field("source_content_sha256") == Some(item.source_content_sha256.as_str())
        && string_field("target_store_ref") == Some(item.target_store_ref.as_str())
        && receipt.get("target_id").and_then(Value::as_str) == item.target_id.as_deref()
        && string_field("phase").is_some_and(|phase| phases.contains(&phase))
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
    if occupant.content_sha256() != source.content_sha256()
        || !receipt_matches(occupant, item, plan_id, &["target_copied"])
    {
        return Err(format!(
            "deterministic target occupant collision: {target_id} already exists in {} but its canonical content or migration receipt does not match",
            target.spec.logical_store.reference()
        ));
    }
    Ok(())
}

fn validate_store_fingerprints(scans: &[StoreScan], plan: &WikiCorpusPlan) -> Result<(), String> {
    let mut expected = plan.store_fingerprints.clone();
    expected.sort_by(|left, right| left.logical_store_ref.cmp(&right.logical_store_ref));
    let mut current = scans
        .iter()
        .filter_map(StoreScan::fingerprint)
        .collect::<Vec<_>>();
    current.sort_by(|left, right| left.logical_store_ref.cmp(&right.logical_store_ref));
    if expected.len() != current.len() {
        return Err("Wiki corpus store fingerprint set changed since preview".to_string());
    }

    for (expected_store, current_store) in expected.iter().zip(current.iter()) {
        if expected_store.logical_store_ref != current_store.logical_store_ref
            || expected_store.physical_id != current_store.physical_id
            || expected_store.canonical_path != current_store.canonical_path
            || expected_store.schema != current_store.schema
        {
            return Err(format!(
                "Wiki corpus store identity or schema changed since preview for {}",
                expected_store.logical_store_ref
            ));
        }

        let scan = find_scan(scans, &expected_store.logical_store_ref)?;
        let expected_rows = expected_store
            .rows
            .iter()
            .map(|row| (row.id.as_str(), row))
            .collect::<BTreeMap<_, _>>();
        let mut normalized_rows = BTreeMap::<String, PlanRowFingerprint>::new();
        for row in &scan.rows {
            if let Some(expected_row) = expected_rows.get(row.id.as_str()) {
                let source_item = plan.items.iter().find(|item| {
                    item.source_store_ref == expected_store.logical_store_ref
                        && item.source_id == row.id
                        && receipt_matches(
                            row,
                            item,
                            &plan.plan_id,
                            &["reclassified", "source_superseded"],
                        )
                });
                if let Some(item) = source_item {
                    if row.content_sha256() != item.source_content_sha256 {
                        return Err(format!(
                            "completed Wiki corpus source content changed for {}:{}",
                            expected_store.logical_store_ref, row.id
                        ));
                    }
                    normalized_rows.insert(row.id.clone(), (*expected_row).clone());
                } else {
                    normalized_rows.insert(row.id.clone(), plan_row_fingerprint(row));
                }
            } else {
                let target_item = plan.items.iter().find(|item| {
                    item.target_store_ref == expected_store.logical_store_ref
                        && item.target_id.as_deref() == Some(row.id.as_str())
                        && receipt_matches(row, item, &plan.plan_id, &["target_copied"])
                });
                if target_item.is_none() {
                    normalized_rows.insert(row.id.clone(), plan_row_fingerprint(row));
                }
            }
        }
        if expected_rows.len() != normalized_rows.len()
            || expected_rows
                .iter()
                .any(|(id, expected_row)| normalized_rows.get(*id) != Some(expected_row))
        {
            return Err(format!(
                "Wiki corpus row fingerprint changed since preview for {}",
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
    let expected_plan_id = digest_string(
        &plan
            .items
            .iter()
            .map(plan_item_material)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    if expected_plan_id != plan.plan_id {
        return Err("Wiki corpus plan id does not match its deterministic items".to_string());
    }
    validate_store_fingerprints(scans, plan)?;
    let shared = find_scan(scans, LogicalStore::SharedWiki.reference())?;
    for item in &plan.items {
        let source = find_scan(scans, &item.source_store_ref)?;
        let source_physical = source.physical.as_ref().ok_or_else(|| {
            format!(
                "source store {} has no physical identity",
                item.source_store_ref
            )
        })?;
        if source_physical.physical_id != item.source_physical_id {
            return Err(format!(
                "source physical identity changed for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
        let row = source.raw_row(&item.source_id).ok_or_else(|| {
            format!(
                "source row {}:{} is missing",
                item.source_store_ref, item.source_id
            )
        })?;
        let completed = match item.action.as_str() {
            "reclassify_in_place" => receipt_matches(row, item, &plan.plan_id, &["reclassified"]),
            "copy_to_shared_and_supersede" => {
                receipt_matches(row, item, &plan.plan_id, &["source_superseded"])
                    && item
                        .target_id
                        .as_deref()
                        .is_some_and(|target_id| row.is_superseded_by(target_id))
            }
            other => return Err(format!("unknown Wiki corpus plan action '{other}'")),
        };
        if completed && row.content_sha256() != item.source_content_sha256 {
            return Err(format!(
                "completed Wiki corpus source content changed for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
        if !completed
            && item.action == "copy_to_shared_and_supersede"
            && row.superseded_by.is_some()
            && row.superseded_by.as_deref() != item.target_id.as_deref()
        {
            return Err(format!(
                "source row {}:{} is already superseded by another target",
                item.source_store_ref, item.source_id
            ));
        }
        let target_proven = if item.action == "copy_to_shared_and_supersede" {
            if let Some(target_id) = item.target_id.as_deref() {
                let proven = shared.raw_row(target_id).is_some();
                if proven {
                    validate_target_occupant(item, &plan.plan_id, row, shared)?;
                }
                proven
            } else {
                false
            }
        } else {
            false
        };
        if !completed
            && (row.revision != item.source_revision
                || row.content_sha256() != item.source_content_sha256
                || (row.classification != Some(CorpusClassification::SharedCandidate)
                    && !target_proven))
        {
            return Err(format!(
                "source revision/content changed after preview for {}:{}; refusing apply",
                item.source_store_ref, item.source_id
            ));
        }
        if item.target_store_ref != LogicalStore::SharedWiki.reference() {
            return Err(format!(
                "unsupported target store '{}'",
                item.target_store_ref
            ));
        }
        if item.target_physical_id
            != shared
                .physical
                .as_ref()
                .map(|physical| physical.physical_id.clone())
                .unwrap_or_default()
        {
            return Err("logical shared Wiki physical identity changed since preview".to_string());
        }
        if item.action == "copy_to_shared_and_supersede" {
            if item.target_physical_id == item.source_physical_id {
                return Err(
                    "source and logical shared target resolve to the same physical database"
                        .to_string(),
                );
            }
        }
        if item.action == "copy_to_shared_and_supersede" && !target_proven {
            validate_target_occupant(item, &plan.plan_id, row, shared)?;
        }
    }
    Ok(())
}

fn load_plan(path: &Path) -> Result<WikiCorpusPlan, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("cannot read Wiki corpus plan {}: {error}", path.display()))?;
    let value: Value = serde_json::from_str(&raw)
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
    physical_db_bindings_for_paths(&addressed_paths)
        .map_err(|error| format!("apply refuses ambiguous physical mutation identity: {error}"))?;
    Ok(())
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

fn apply_reclassification(
    scan: &StoreScan,
    item: &PlanItem,
    plan_id: &str,
) -> Result<MigrationOutcome, String> {
    check_authority(scan, None)?;
    let mut store = open_apply_store(scan)?;
    let entry = store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("source row {} disappeared during apply", item.source_id))?;
    let source_row = raw_from_entry(&entry);
    if receipt_matches(&source_row, item, plan_id, &["reclassified"]) {
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
    if !changed {
        return Err(format!(
            "source revision CAS failed for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
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
    let mut source_store = open_apply_store(source_scan)?;
    let mut target_store = open_apply_store(target_scan)?;
    let mut source_entry = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("source row {} disappeared during apply", item.source_id))?;
    let source_row = raw_from_entry(&source_entry);
    let source_receipted = receipt_matches(&source_row, item, plan_id, &["source_superseded"]);
    if !source_receipted
        && (source_entry.revision != item.source_revision
            || source_row.content_sha256() != item.source_content_sha256)
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
            || !receipt_matches(&target_row, item, plan_id, &["target_copied"])
        {
            return Err(format!(
                "deterministic target occupant collision: {target_id} content or receipt mismatch"
            ));
        }
        phases.push("target_copied".to_string());
    } else {
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
        let verified_target = target_store
            .get_with_options(target_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "target insert did not produce a readable row".to_string())?;
        let verified_target_row = raw_from_entry(&verified_target);
        if verified_target_row.content_sha256() != item.source_content_sha256
            || !receipt_matches(&verified_target_row, item, plan_id, &["target_copied"])
        {
            return Err("target insert produced a mismatched deterministic occupant".to_string());
        }
        phases.push("target_copied".to_string());
    }

    if source_store
        .supersession_target(&source_entry.id)
        .map_err(|error| error.to_string())?
        .flatten()
        .as_deref()
        == Some(target_id)
    {
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
            receipt_value(item, plan_id, "source_superseded"),
        )?;
        let expected_revision = source_entry.revision;
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
        source_entry = source_store
            .get_with_options(&item.source_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "source disappeared after source receipt".to_string())?;
        phases.push("source_receipted".to_string());
    }
    let expected_revision = source_entry.revision;
    if !source_store
        .supersede_memory_if_revision(&source_entry.id, target_id, expected_revision)
        .map_err(|error| error.to_string())?
    {
        if source_store
            .supersession_target(&item.source_id)
            .map_err(|error| error.to_string())?
            .flatten()
            .as_deref()
            == Some(target_id)
        {
            phases.push("source_superseded".to_string());
        } else {
            return Err(format!(
                "source supersession revision CAS failed for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
    } else {
        phases.push("source_superseded".to_string());
    }
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

fn verify_backup(
    source: &StoreScan,
    backup_path: &Path,
    logical_store_refs: Vec<String>,
) -> Result<BackupReceipt, String> {
    let source_physical = source
        .physical
        .as_ref()
        .ok_or_else(|| "backup source has no physical identity".to_string())?;
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
    let backup_metadata = fs::symlink_metadata(backup_path).map_err(|error| {
        format!(
            "cannot inspect backup path {}: {error}",
            backup_path.display()
        )
    })?;
    if !backup_metadata.file_type().is_file() || backup_metadata.file_type().is_symlink() {
        return Err(format!(
            "backup path {} is not a regular non-symlink file",
            backup_path.display()
        ));
    }
    if backup_physical.physical_id == source_physical.physical_id {
        return Err(format!(
            "backup path {} resolves to the source physical database",
            backup_path.display()
        ));
    }
    let conn = open_read_only_connection(Path::new(&backup_physical.open_path))
        .map_err(|error| error.to_string())?;
    let schema = read_schema_version(&conn).map_err(|error| error.to_string())?;
    if schema != EXPECTED_SCHEMA_VERSION {
        return Err(format!(
            "backup {} schema mismatch: stored {}, expected {}",
            backup_path.display(),
            schema,
            EXPECTED_SCHEMA_VERSION
        ));
    }
    // Apply-only validation: preview never reaches this normal current-schema
    // MemoryStore open.
    MemoryStore::open_read_only(&backup_path.display().to_string())
        .map_err(|error| format!("backup current-schema validation failed: {error}"))?;
    let quick_check = quick_check(&conn)?;
    let (rows, _) = load_rows(&conn)?;
    let backup_digest = row_digest(&rows);
    if rows.len() != source.report.counts.total_memory_rows || backup_digest != source.row_digest {
        return Err(format!(
            "backup {} row evidence mismatch",
            backup_path.display()
        ));
    }
    Ok(BackupReceipt {
        logical_store_refs,
        source_physical_id: source_physical.physical_id.clone(),
        source_canonical_path: source_physical.canonical_path.clone(),
        source_open_path: source_physical.open_path.clone(),
        backup_path: backup_path.display().to_string(),
        backup_physical_id: backup_physical.physical_id,
        schema,
        total_memory_rows: rows.len(),
        source_row_digest: source.row_digest.clone(),
        backup_row_digest: backup_digest,
        source_identity_verified: true,
        backup_identity_verified: true,
        quick_check,
        verified: true,
    })
}

fn create_or_verify_backup(
    source: &StoreScan,
    backup_dir: &Path,
    logical_store_refs: Vec<String>,
) -> Result<BackupReceipt, String> {
    let source_physical = source
        .physical
        .as_ref()
        .ok_or_else(|| "backup source has no physical identity".to_string())?;
    let backup_path = backup_dir.join(backup_file_name(&source_physical.physical_id));
    if !backup_path.exists() {
        let source_conn = open_preview_connection(Path::new(&source_physical.open_path))
            .map_err(|error| format!("cannot open backup source: {error}"))?;
        let mut destination = Connection::open(&backup_path)
            .map_err(|error| format!("cannot create backup {}: {error}", backup_path.display()))?;
        let backup = rusqlite::backup::Backup::new(&source_conn, &mut destination)
            .map_err(|error| format!("cannot initialize SQLite backup: {error}"))?;
        backup
            .run_to_completion(128, Duration::from_millis(100), None)
            .map_err(|error| format!("SQLite backup failed: {error}"))?;
    }
    verify_backup(source, &backup_path, logical_store_refs)
}

fn write_manifest_if_needed(path: &Path, manifest: &BackupManifest) -> Result<(), String> {
    let value = serde_json::to_value(manifest).map_err(|error| error.to_string())?;
    if path.exists() {
        let existing = fs::read_to_string(path)
            .map_err(|error| format!("cannot read existing backup manifest: {error}"))?;
        let existing_value: Value = serde_json::from_str(&existing)
            .map_err(|error| format!("existing backup manifest is invalid: {error}"))?;
        if existing_value != value {
            return Err(format!(
                "deterministic backup manifest {} belongs to a different plan or evidence",
                path.display()
            ));
        }
        return Ok(());
    }
    let temporary = path.with_extension("json.tmp");
    if temporary.exists() {
        return Err(format!(
            "backup manifest temporary path already exists: {}",
            temporary.display()
        ));
    }
    let bytes = serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?;
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
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

fn apply_plan(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
) -> Result<(BackupManifest, Vec<MigrationOutcome>), String> {
    validate_apply_inventory(scans)?;
    validate_plan(scans, plan)?;
    if !plan.items.is_empty()
        && plan
            .items
            .iter()
            .all(|item| plan_item_completed(scans, plan, item))
    {
        let final_path = backup_dir.join("wiki-corpus-v1-apply-receipt.json");
        let initial_path = backup_dir.join("wiki-corpus-v1-manifest.json");
        let manifest = if final_path.exists() {
            let raw = fs::read_to_string(&final_path).map_err(|error| error.to_string())?;
            serde_json::from_str::<BackupManifest>(&raw).map_err(|error| error.to_string())?
        } else {
            let raw = fs::read_to_string(&initial_path).map_err(|error| error.to_string())?;
            let initial =
                serde_json::from_str::<BackupManifest>(&raw).map_err(|error| error.to_string())?;
            if initial.plan_id != plan.plan_id {
                return Err(
                    "existing backup manifest belongs to a different completed plan".to_string(),
                );
            }
            let completed = BackupManifest {
                version: initial.version.clone(),
                status: "apply_completed".to_string(),
                plan_id: initial.plan_id.clone(),
                backup_directory: initial.backup_directory.clone(),
                receipts: initial.receipts.clone(),
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
            write_manifest_if_needed(&final_path, &completed)?;
            completed
        };
        if manifest.plan_id != plan.plan_id || manifest.status != "apply_completed" {
            return Err("existing apply receipt does not match the completed plan".to_string());
        }
        return Ok((manifest, plan.items.iter().map(existing_outcome).collect()));
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
        write_manifest_if_needed(&manifest_path, &manifest)?;
        return Ok((manifest, Vec::new()));
    }

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
    let mut receipts = Vec::new();
    for (_physical_id, (mut logical_refs, source)) in groups {
        logical_refs.sort();
        logical_refs.dedup();
        check_authority(source, None)?;
        receipts.push(create_or_verify_backup(source, backup_dir, logical_refs)?);
    }
    receipts.sort_by(|left, right| left.source_physical_id.cmp(&right.source_physical_id));
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
    write_manifest_if_needed(&manifest_path, &initial_manifest)?;

    let mut outcomes = Vec::new();
    for item in &plan.items {
        let outcome = if item.action == "reclassify_in_place" {
            let source = find_scan(scans, &item.source_store_ref)?;
            apply_reclassification(source, item, &plan.plan_id)?
        } else {
            let source = find_scan(scans, &item.source_store_ref)?;
            let target = find_scan(scans, &item.target_store_ref)?;
            apply_copy_and_supersede(source, target, item, &plan.plan_id)?
        };
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
    write_manifest_if_needed(&final_path, &final_manifest)?;
    Ok((final_manifest, outcomes))
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
        if !backup_dir.is_dir() {
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
    let (backup_manifest, migration_outcomes) = apply_plan(&scans, &plan, &backup_dir)?;
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
        let replay_identity = digest_string(&format!(
            "wiki-corpus-replay-v1\0{}\0unix:source\0{}\0{}\0{}\0{}",
            LogicalStore::LegacyGlobal.reference(),
            source.id,
            source.revision,
            source.normalized_path(),
            source.content_sha256(),
        ));
        PlanItem {
            action: "copy_to_shared_and_supersede".to_string(),
            source_store_ref: LogicalStore::LegacyGlobal.reference().to_string(),
            source_physical_id: "unix:source".to_string(),
            source_id: source.id.clone(),
            source_path: source.path.clone(),
            normalized_path: source.normalized_path(),
            source_revision: source.revision,
            source_content_sha256: source.content_sha256(),
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

        let backup_dir = directory.path().join("backups");
        fs::create_dir(&backup_dir).unwrap();
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
}

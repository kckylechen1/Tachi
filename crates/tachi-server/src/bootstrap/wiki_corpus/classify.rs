use super::fs::*;
use super::types::*;

use crate::physical_db_identity::{
    classify_memory_failure, classify_open_failure, classify_paths, InventoryFailureKind,
};
use crate::server_state::MemoryServer;
use crate::tool_params::{
    derive_effective_knowledge_artifact, EffectiveKnowledgeArtifactV1, WikiApplicabilityStatusV1,
    WikiKnowledgeScopeV1,
};
use memcore::db::migrations::{read_schema_version, EXPECTED_SCHEMA_VERSION};
use memcore::MemoryEntry;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(crate) fn digest_string(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub(crate) fn canonicalize_value(value: &Value) -> Value {
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

pub(crate) fn json_array(raw: Option<String>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
        .unwrap_or_default()
}

pub(crate) fn read_optional_string(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<String>> {
    row.get(index)
}

#[allow(clippy::chunks_exact_to_as_chunks)]
pub(crate) fn load_rows(conn: &Connection) -> Result<(Vec<RawRow>, usize, bool), String> {
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
                    blob.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                        .collect(),
                );
            }
        }
    }
    let count = rows.len();
    Ok((rows, count, vector_table_present))
}

pub(crate) fn plan_row_fingerprint(row: &RawRow, vector_table_present: bool) -> PlanRowFingerprint {
    PlanRowFingerprint {
        id: row.id.clone(),
        revision: row.revision,
        content_sha256: row.content_sha256(),
        superseded_by: row.superseded_by.clone(),
        vector: row.vector_fingerprint(vector_table_present),
    }
}

pub(crate) fn row_digest_values(mut values: Vec<Value>) -> String {
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

pub(crate) fn row_digest(rows: &[RawRow], vector_table_present: bool) -> String {
    row_digest_values(
        rows.iter()
            .map(|row| {
                serde_json::to_value(plan_row_fingerprint(row, vector_table_present))
                    .unwrap_or(Value::Null)
            })
            .collect(),
    )
}

pub(crate) fn metadata_value(metadata: &Value, key: &str) -> Option<Value> {
    metadata.get(key).cloned()
}

pub(crate) fn marker_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

pub(crate) fn marker_bool(metadata: &Value, key: &str) -> bool {
    metadata.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub(crate) fn exact_marker(metadata: &Value, keys: &[&str], values: &[&str]) -> bool {
    keys.iter()
        .filter_map(|key| marker_string(metadata, key))
        .any(|value| values.iter().any(|candidate| value == *candidate))
}

pub(crate) fn marker_list(metadata: &Value, keys: &[&str], values: &[&str]) -> Vec<String> {
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

pub(crate) fn memory_entry_from_raw(row: &RawRow) -> MemoryEntry {
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

pub(crate) fn authority_markers(row: &RawRow) -> Vec<String> {
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

pub(crate) fn operational_markers(row: &RawRow) -> Vec<String> {
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

pub(crate) fn test_markers(row: &RawRow) -> Vec<String> {
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

pub(crate) fn is_wiki_related(row: &RawRow) -> bool {
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

pub(crate) fn valid_typed_shared(row: &RawRow, effective: &EffectiveKnowledgeArtifactV1) -> bool {
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

pub(crate) fn classify_row(
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

pub(crate) fn typed_metadata_evidence(
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

pub(crate) fn finalize_classifications(scans: &mut [StoreScan]) {
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

pub(crate) fn row_report(scan: &StoreScan, row: &RawRow) -> WikiCorpusRow {
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

pub(crate) fn refresh_report(scan: &mut StoreScan) {
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

pub(crate) fn inventory_store(spec: StoreSpec) -> StoreScan {
    let logical_store_ref = spec.logical_store.reference().to_string();
    let existence = spec
        .addressed_path
        .as_ref()
        .is_some_and(|path| std::fs::metadata(path).is_ok());
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

pub(crate) fn store_specs(
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
) -> Vec<StoreSpec> {
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

pub(crate) fn find_scan<'a>(
    scans: &'a [StoreScan],
    reference: &str,
) -> Result<&'a StoreScan, String> {
    scans
        .iter()
        .find(|scan| scan.spec.logical_store.reference() == reference)
        .ok_or_else(|| format!("plan references unknown logical store '{reference}'"))
}

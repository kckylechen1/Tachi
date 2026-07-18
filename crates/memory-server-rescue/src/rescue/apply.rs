use super::source::read_source_rows;
use super::types::{RescueApplyReport, RescuePlan, SourceRow};
use memcore::types::{MemoryCategory, MemoryScope, MemorySource};
use rusqlite::{params, Connection};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Compute deterministic new id: prepend "rescue:<target>:" to the source id.
/// Keeps the original id discoverable in target metadata for traceability.
fn make_target_id(source_id: &str, target: &str) -> String {
    format!("rescue-{target}-{source_id}")
}

/// Detect whether a target DB has the post-migration columns we need.
struct TargetCaps {
    has_domain: bool,
    has_retention_policy: bool,
    has_location: bool,
}

fn merge_legacy_persons_into_entities(persons_raw: &str, entities_raw: &str) -> String {
    let persons: Vec<String> = serde_json::from_str(persons_raw).unwrap_or_default();
    let mut entities: Vec<String> = serde_json::from_str(entities_raw).unwrap_or_default();
    memcore::types::fold_person_names_into_entities(&mut entities, persons);
    serde_json::to_string(&entities).unwrap_or_else(|_| "[]".to_string())
}

fn detect_target_caps(conn: &Connection) -> Result<TargetCaps, String> {
    let mut has_domain = false;
    let mut has_retention_policy = false;
    let mut has_location = false;
    let mut stmt = conn
        .prepare("PRAGMA table_info(memories)")
        .map_err(|e| format!("inspect target memories schema: {e}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| format!("read target memories schema: {e}"))?;
    for col in rows {
        let col = col.map_err(|e| format!("decode target memories schema column: {e}"))?;
        if col == "domain" {
            has_domain = true;
        }
        if col == "retention_policy" {
            has_retention_policy = true;
        }
        if col == "location" {
            has_location = true;
        }
    }
    Ok(TargetCaps {
        has_domain,
        has_retention_policy,
        has_location,
    })
}

/// Apply the plan: insert each row into its target DB. Skips rows whose
/// computed target id already exists (idempotent re-runs). The source DB is
/// renamed `<source>.bak.<rfc3339>` after a fully successful pass.
pub fn apply_rescue(
    source: &Path,
    targets_root: &Path,
    plan: RescuePlan,
) -> Result<RescueApplyReport, String> {
    let mut report = RescueApplyReport {
        plan: plan.clone(),
        ..Default::default()
    };

    // Re-read rows so we have full payloads (the plan only carries metadata).
    let rows = read_source_rows(source)?;
    let by_id: std::collections::HashMap<String, &SourceRow> =
        rows.iter().map(|r| (r.id.clone(), r)).collect();

    // Open / cache one connection per target.
    let mut conns: BTreeMap<String, (Connection, TargetCaps)> = BTreeMap::new();
    for target in plan.per_target.keys() {
        let canonical_path = targets_root.join(target).join(memcore::MEMORY_DB_FILENAME);
        let legacy_path = targets_root
            .join(target)
            .join(memcore::LEGACY_MEMORY_DB_FILENAME);
        let path = if canonical_path.exists() {
            canonical_path
        } else if legacy_path.exists() {
            legacy_path
        } else {
            report.errors.push(format!(
                "target DB missing: {} (skipping {} rows for this target)",
                canonical_path.display(),
                plan.per_target[target]
            ));
            continue;
        };
        let conn =
            Connection::open(&path).map_err(|e| format!("open target {}: {e}", path.display()))?;
        let caps = detect_target_caps(&conn)?;
        conns.insert(target.clone(), (conn, caps));
    }

    for assignment in &plan.assignments {
        let row = match by_id.get(&assignment.source_id) {
            Some(r) => r,
            None => {
                report
                    .errors
                    .push(format!("missing source row id={}", assignment.source_id));
                continue;
            }
        };
        let (conn, caps) = match conns.get(&assignment.target) {
            Some(c) => c,
            None => continue, // already errored above
        };
        let new_id = make_target_id(&row.id, &assignment.target);

        // Idempotency check.
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM memories WHERE id = ?1",
                params![new_id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if exists {
            report.skipped_existing += 1;
            continue;
        }

        // Build the metadata blob, annotating provenance + isolation hints.
        let mut meta_val: serde_json::Value =
            serde_json::from_str(&row.metadata).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = meta_val.as_object_mut() {
            obj.insert(
                "rescue".into(),
                serde_json::json!({
                    "from": source.display().to_string(),
                    "from_id": row.id,
                    "from_path": row.path,
                    "target": assignment.target,
                    "reason": assignment.reason,
                    "trading": assignment.trading,
                    "at": chrono::Utc::now().to_rfc3339(),
                }),
            );
        }

        let scope_final = if assignment.trading {
            "user".to_string()
        } else {
            MemoryScope::normalize(&row.scope).to_string()
        };
        // B1/B2: normalize legacy source/category to satisfy CHECK constraints.
        let source_final = MemorySource::parse_or_external(&row.source).to_string();
        let category_final = MemoryCategory::normalize(&row.category).to_string();
        let entities_final = merge_legacy_persons_into_entities(&row.persons, &row.entities);
        let path_final =
            memcore::types::apply_location_relocation(&row.path, &row.location, &mut meta_val);
        let meta_str = meta_val.to_string();

        let result = if caps.has_domain && caps.has_retention_policy {
            if caps.has_location {
                conn.execute(
                    "INSERT INTO memories
                     (id, path, summary, text, importance, timestamp, category, topic, keywords,
                      entities, location, source, scope, archived, created_at, updated_at,
                      access_count, last_access, revision, metadata, retention_policy, domain)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
                    params![
                        new_id,
                        path_final,
                        row.summary,
                        row.text,
                        row.importance,
                        row.timestamp,
                        category_final,
                        row.topic,
                        row.keywords,
                        entities_final,
                        "",
                        source_final,
                        scope_final,
                        row.archived,
                        row.created_at,
                        row.updated_at,
                        row.access_count,
                        row.last_access,
                        row.revision,
                        meta_str,
                        "durable",
                        if assignment.trading { Some("equity_trading") } else { None::<&str> },
                    ],
                )
            } else {
                conn.execute(
                    "INSERT INTO memories
                     (id, path, summary, text, importance, timestamp, category, topic, keywords,
                      entities, source, scope, archived, created_at, updated_at,
                      access_count, last_access, revision, metadata, retention_policy, domain)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                    params![
                        new_id,
                        path_final,
                        row.summary,
                        row.text,
                        row.importance,
                        row.timestamp,
                        category_final,
                        row.topic,
                        row.keywords,
                        entities_final,
                        source_final,
                        scope_final,
                        row.archived,
                        row.created_at,
                        row.updated_at,
                        row.access_count,
                        row.last_access,
                        row.revision,
                        meta_str,
                        "durable",
                        if assignment.trading { Some("equity_trading") } else { None::<&str> },
                    ],
                )
            }
        } else if caps.has_location {
            conn.execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords,
                  entities, location, source, scope, archived, created_at, updated_at,
                  access_count, last_access, metadata, revision)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
                params![
                    new_id,
                    path_final,
                    row.summary,
                    row.text,
                    row.importance,
                    row.timestamp,
                    category_final,
                    row.topic,
                    row.keywords,
                    entities_final,
                    "",
                    source_final,
                    scope_final,
                    row.archived,
                    row.created_at,
                    row.updated_at,
                    row.access_count,
                    row.last_access,
                    meta_str,
                    row.revision,
                ],
            )
        } else {
            conn.execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords,
                  entities, source, scope, archived, created_at, updated_at,
                  access_count, last_access, metadata, revision)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![
                    new_id,
                    path_final,
                    row.summary,
                    row.text,
                    row.importance,
                    row.timestamp,
                    category_final,
                    row.topic,
                    row.keywords,
                    entities_final,
                    source_final,
                    scope_final,
                    row.archived,
                    row.created_at,
                    row.updated_at,
                    row.access_count,
                    row.last_access,
                    meta_str,
                    row.revision,
                ],
            )
        };

        match result {
            Ok(_) => {
                *report
                    .written_per_target
                    .entry(assignment.target.clone())
                    .or_insert(0) += 1;
            }
            Err(e) => {
                report.errors.push(format!(
                    "insert into {} (source_id={}): {e}",
                    assignment.target, row.id
                ));
            }
        }
    }

    // Backup the source DB if there were any successful writes and no errors.
    if report.errors.is_empty() && !report.written_per_target.is_empty() {
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let backup_path: PathBuf = source.with_extension(format!("db.bak.{ts}"));
        if let Err(e) = std::fs::rename(source, &backup_path) {
            report
                .errors
                .push(format!("backup rename failed: {e} (source preserved)"));
        } else {
            report.source_backed_up_to = Some(backup_path.display().to_string());
        }
    }

    Ok(report)
}

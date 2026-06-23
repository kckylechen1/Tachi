use std::collections::HashSet;

use serde_json::{json, Value};

use crate::server_state::MemoryServer;
use memory_core::{MemoryEntry, MemoryStore};

use crate::foundry_runtime_ops::maintenance::{
    coherence_bucket_key, coherent_distill_buckets, scheduled_distill_path_prefix,
};
use crate::foundry_runtime_ops::FOUNDRY_DISTILL_SOURCE;

use super::config::{resolve_candidate_scan_limit, resolve_processed_scan_limit, MIN_BUCKET_SIZE};
use super::types::CandidateGroup;

fn is_wiki_project_db(server: &MemoryServer) -> bool {
    server
        .project_db_path_buf()
        .and_then(|path| crate::path_utils::named_project_for_db_path(&path))
        .is_some_and(|name| name.eq_ignore_ascii_case("wiki"))
}

fn is_quarantine_entry(entry: &MemoryEntry) -> bool {
    entry.path.starts_with("/_quarantine/") || entry.path.starts_with("/quarantine/")
}

fn should_skip_distill_candidate(entry: &MemoryEntry, wiki_project: bool) -> bool {
    entry.archived
        || entry.source.eq_ignore_ascii_case(FOUNDRY_DISTILL_SOURCE)
        || memory_core::is_recall_cache_entry(entry)
        || is_quarantine_entry(entry)
        || (!wiki_project && memory_core::is_wiki_entry(entry))
}

/// Read + filter distill candidate memories from one already-open store. Pure
/// store logic, identical for the bound project and any named-project DB.
fn scan_distill_inputs(
    store: &mut MemoryStore,
    processed_scan_limit: i64,
    candidate_scan_limit: i64,
    wiki_project: bool,
) -> Result<Vec<MemoryEntry>, String> {
    let conn = store.connection();
    let mut processed_ids: HashSet<String> = HashSet::new();
    let mut stmt = conn
        .prepare(
            "SELECT metadata FROM memories
             WHERE archived = 0 AND source = ?1
             ORDER BY timestamp DESC
             LIMIT ?2",
        )
        .map_err(|e| format!("prepare distill metadata query: {e}"))?;
    let rows = stmt
        .query_map(
            rusqlite::params![FOUNDRY_DISTILL_SOURCE, processed_scan_limit],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| format!("query distill metadata rows: {e}"))?;
    for row in rows {
        let raw = row.map_err(|e| format!("read distill metadata row: {e}"))?;
        let metadata: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
        let ids = metadata
            .get("source_memory_ids")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .map(ToOwned::to_owned);
        processed_ids.extend(ids);
    }

    let mut stmt = conn
        .prepare(
            "SELECT id,path,summary,text,importance,timestamp,valid_from,valid_until,category,topic,keywords,'[]' AS persons,entities,'' AS location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain,recall_count,query_diversity,tier
              FROM memories
              WHERE archived = 0 AND source != ?1
              ORDER BY timestamp ASC
              LIMIT ?2",
        )
        .map_err(|e| format!("prepare candidate query: {e}"))?;
    let rows = stmt
        .query_map(
            rusqlite::params![FOUNDRY_DISTILL_SOURCE, candidate_scan_limit],
            memory_core::row_to_entry,
        )
        .map_err(|e| format!("query candidate rows: {e}"))?;

    let mut entries: Vec<MemoryEntry> = Vec::new();
    for row in rows {
        let entry = row.map_err(|e| format!("read candidate row: {e}"))?;
        if processed_ids.contains(&entry.id) {
            continue;
        }
        if !should_skip_distill_candidate(&entry, wiki_project) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Collect distill candidate groups for either the bound project DB
/// (`project = None`) or a specific named-project DB (`project = Some(name)`).
pub(crate) fn collect_candidate_groups(
    server: &MemoryServer,
    project: Option<&str>,
) -> Result<Vec<CandidateGroup>, String> {
    let processed_scan_limit = resolve_processed_scan_limit() as i64;
    let candidate_scan_limit = resolve_candidate_scan_limit() as i64;
    let wiki_project = match project {
        Some(name) => name.eq_ignore_ascii_case("wiki"),
        None => is_wiki_project_db(server),
    };
    let candidate_entries = match project {
        Some(name) => server.with_named_project_store_read(name, |store| {
            scan_distill_inputs(
                store,
                processed_scan_limit,
                candidate_scan_limit,
                wiki_project,
            )
        }),
        None => server.with_project_store_read(|store| {
            scan_distill_inputs(
                store,
                processed_scan_limit,
                candidate_scan_limit,
                wiki_project,
            )
        }),
    }?;

    if candidate_entries.is_empty() {
        return Ok(Vec::new());
    }

    let buckets = coherent_distill_buckets(candidate_entries);
    let mut groups = Vec::new();
    for (bucket_key, entries) in buckets {
        if entries.len() < MIN_BUCKET_SIZE {
            continue;
        }
        let (path_prefix, coherence_key) = bucket_key
            .split_once('#')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or_else(|| {
                let path_prefix = entries
                    .first()
                    .map(|e| scheduled_distill_path_prefix(&e.path))
                    .unwrap_or_else(|| "/".to_string());
                let coherence_key = entries
                    .first()
                    .and_then(|e| coherence_bucket_key(&e.topic, &e.entities))
                    .unwrap_or_else(|| "unknown".to_string());
                (path_prefix, coherence_key)
            });
        let group_id = format!(
            "{}|{}",
            sanitize_id_segment(&path_prefix),
            sanitize_id_segment(&coherence_key)
        );
        groups.push(CandidateGroup {
            group_id,
            path_prefix,
            coherence_key,
            entries,
        });
    }
    Ok(groups)
}

pub(crate) fn sanitize_id_segment(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').chars().take(40).collect::<String>()
}

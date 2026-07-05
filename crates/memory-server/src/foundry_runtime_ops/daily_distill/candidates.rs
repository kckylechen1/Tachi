use crate::server_state::MemoryServer;
use memory_core::{MemoryEntry, MemoryStore};

use crate::foundry_runtime_ops::maintenance::collect_coherent_distill_buckets;
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
    let processed_ids = store
        .distill_processed_source_ids(FOUNDRY_DISTILL_SOURCE, processed_scan_limit)
        .map_err(|e| format!("query distill metadata rows: {e}"))?;
    let candidates = store
        .distill_candidate_entries(FOUNDRY_DISTILL_SOURCE, candidate_scan_limit)
        .map_err(|e| format!("query candidate rows: {e}"))?;
    Ok(candidates
        .into_iter()
        .filter(|entry| {
            !processed_ids.contains(&entry.id)
                && !should_skip_distill_candidate(entry, wiki_project)
        })
        .collect())
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

    let buckets = collect_coherent_distill_buckets(candidate_entries);
    let mut groups = Vec::new();
    for bucket in buckets {
        if bucket.entries.len() < MIN_BUCKET_SIZE {
            continue;
        }
        let group_id = format!(
            "{}|{}",
            sanitize_id_segment(&bucket.path_prefix),
            sanitize_id_segment(&bucket.coherence_key)
        );
        groups.push(CandidateGroup {
            group_id,
            path_prefix: bucket.path_prefix,
            coherence_key: bucket.coherence_key,
            entries: bucket.entries,
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

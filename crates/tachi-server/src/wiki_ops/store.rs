use super::*;

fn compact_entry(entry: &MemoryEntry) -> Value {
    json!({
        "id": entry.id,
        "path": entry.path,
        "summary": entry.summary,
    })
}

pub(super) fn find_related_by_entities(
    server: &MemoryServer,
    project: &str,
    entities: &[String],
    exclude_id: &str,
    limit: usize,
) -> Vec<Value> {
    let entity_set: HashSet<String> = entities
        .iter()
        .map(|entity| entity.trim().to_ascii_lowercase())
        .filter(|entity| !entity.is_empty())
        .collect();
    if entity_set.is_empty() || limit == 0 {
        return Vec::new();
    }

    let entries = list_related_candidates(server, project, 5000).unwrap_or_default();

    let mut related = entries
        .into_iter()
        .filter(is_user_facing_wiki_entry)
        .filter(|entry| entry.id != exclude_id)
        .filter(|entry| {
            entry
                .entities
                .iter()
                .any(|entity| entity_set.contains(&entity.trim().to_ascii_lowercase()))
        })
        .collect::<Vec<_>>();
    related.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.timestamp.cmp(&a.timestamp))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut seen = HashSet::new();
    related
        .into_iter()
        .filter(|entry| seen.insert(entry.id.clone()))
        .take(limit)
        .map(|entry| compact_entry(&entry))
        .collect()
}

pub(super) fn list_related_candidates(
    server: &MemoryServer,
    project: &str,
    limit: usize,
) -> Result<Vec<MemoryEntry>, String> {
    list_wiki_entries(server, project, limit).map(|(entries, _)| entries)
}

pub(super) fn is_user_facing_wiki_entry(entry: &MemoryEntry) -> bool {
    entry.path != "/wiki/_log"
        && !entry.path.contains("/recall-cache/")
        && entry.source != "foundry_recall_rerank_cache"
        && !entry
            .metadata
            .get("wiki_log")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

pub(crate) fn filter_user_facing_wiki_rows(rows: &mut Vec<Value>) {
    rows.retain(|row| {
        let path = row.get("path").and_then(Value::as_str).unwrap_or_default();
        let source = row
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default();
        path != "/wiki/_log"
            && !path.contains("/recall-cache/")
            && source != "foundry_recall_rerank_cache"
    });
}

fn merge_wiki_store_entries(
    merged: &mut Vec<MemoryEntry>,
    seen: &mut HashSet<String>,
    first_source: &mut &'static str,
    store_label: &'static str,
    entries: Vec<MemoryEntry>,
    limit: usize,
) {
    for entry in entries.into_iter().filter(is_user_facing_wiki_entry) {
        if merged.len() >= limit {
            break;
        }
        if *first_source == "empty" {
            *first_source = store_label;
        }
        if seen.insert(entry.id.clone()) {
            merged.push(entry);
        }
    }
}

pub(super) fn list_wiki_entries(
    server: &MemoryServer,
    project: &str,
    limit: usize,
) -> Result<(Vec<MemoryEntry>, &'static str), String> {
    // Wiki entries can live in any of three stores: a named project DB
    // (when the caller scopes to one), the active workspace project DB, or
    // the global DB. We try named → project → global and merge the user-facing
    // entries so `wiki_read` finds an entry no matter which store holds it.
    let mut seen: HashSet<String> = HashSet::new();
    let mut merged: Vec<MemoryEntry> = Vec::new();
    let mut first_source: &'static str = "empty";

    // Try every store regardless of whether the previous one returned Ok:
    // a named project store may exist but not contain the entry the caller
    // is asking for (e.g. `project=wiki` resolves to `~/.tachi/projects/wiki`
    // which holds a different wiki namespace), and we must fall through to
    // the workspace project + global stores so the read still finds the
    // entry. Entries are deduplicated by id and capped at `limit`.
    if merged.len() < limit {
        if let Ok(entries) = server.with_named_project_store_read(project, |store| {
            store
                .list_by_path("/wiki", limit, false)
                .map_err(|e| format!("wiki list: {e}"))
        }) {
            merge_wiki_store_entries(
                &mut merged,
                &mut seen,
                &mut first_source,
                "named",
                entries,
                limit,
            );
        }
    }
    if merged.len() < limit {
        if let Ok(entries) = server.with_project_store_read(|store| {
            store
                .list_by_path("/wiki", limit, false)
                .map_err(|e| format!("wiki project list: {e}"))
        }) {
            merge_wiki_store_entries(
                &mut merged,
                &mut seen,
                &mut first_source,
                "project",
                entries,
                limit,
            );
        }
    }
    if merged.len() < limit {
        match server.with_global_store_read(|store| {
            store
                .list_by_path("/wiki", limit, false)
                .map_err(|e| format!("wiki fallback list: {e}"))
        }) {
            Ok(entries) => merge_wiki_store_entries(
                &mut merged,
                &mut seen,
                &mut first_source,
                "global",
                entries,
                limit,
            ),
            Err(global_err) => {
                if merged.is_empty() {
                    return Err(format!("wiki list: {global_err}"));
                }
            }
        }
    }

    merged.truncate(limit);
    Ok((merged, first_source))
}

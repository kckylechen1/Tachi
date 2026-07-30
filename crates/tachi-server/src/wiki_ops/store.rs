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
    let plan = WikiReadPlan::NamedOnly(StoreRef::named(project));
    list_wiki_entries_for_plan(server, &plan, "/wiki", limit)
        .map(|entries| entries.into_iter().map(|entry| entry.entry).collect())
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

#[derive(Debug, Clone)]
pub(super) struct StoredWikiEntry {
    pub(super) entry: MemoryEntry,
    pub(super) store: StoreRef,
}

fn same_store_path(server: &MemoryServer) -> bool {
    let Some(bound) = server.project_db_path_buf() else {
        return false;
    };
    let Ok(shared) = server.resolve_server_named_project_db_path(LOGICAL_SHARED_WIKI_PROJECT)
    else {
        return false;
    };
    if bound == shared {
        return true;
    }
    std::fs::canonicalize(bound)
        .ok()
        .zip(std::fs::canonicalize(shared).ok())
        .is_some_and(|(bound, shared)| bound == shared)
}

pub(super) fn stores_for_wiki_plan(
    server: &MemoryServer,
    plan: &WikiReadPlan,
) -> Vec<StoreRef> {
    match plan {
        WikiReadPlan::NamedOnly(store) => vec![store.clone()],
        WikiReadPlan::Federated => {
            let mut stores = Vec::new();
            if server.has_project_db() {
                stores.push(StoreRef::BoundProject);
            }
            if !same_store_path(server) {
                stores.push(StoreRef::named(LOGICAL_SHARED_WIKI_PROJECT));
            }
            stores.push(StoreRef::LegacyGlobal);
            stores
        }
        WikiReadPlan::ProjectOnly => server
            .has_project_db()
            .then_some(StoreRef::BoundProject)
            .into_iter()
            .collect(),
        WikiReadPlan::SharedOnly => vec![StoreRef::named(LOGICAL_SHARED_WIKI_PROJECT)],
    }
}

pub(super) fn with_wiki_store_read<T>(
    server: &MemoryServer,
    store_ref: &StoreRef,
    action: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    match store_ref {
        StoreRef::BoundProject => server.with_project_store_read(action),
        StoreRef::NamedProject { project } => {
            server.with_named_project_store_read(project, action)
        }
        StoreRef::LegacyGlobal => server.with_global_store_read(action),
    }
}

pub(super) fn with_wiki_store<T>(
    server: &MemoryServer,
    store_ref: &StoreRef,
    action: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    match store_ref {
        StoreRef::BoundProject => server.with_project_store(action),
        StoreRef::NamedProject { project } => server.with_named_project_store(project, action),
        StoreRef::LegacyGlobal => server.with_global_store(action),
    }
}

pub(super) fn list_wiki_entries_for_plan(
    server: &MemoryServer,
    plan: &WikiReadPlan,
    path_prefix: &str,
    limit: usize,
) -> Result<Vec<StoredWikiEntry>, String> {
    let mut entries = Vec::new();
    for store_ref in stores_for_wiki_plan(server, plan) {
        let listed = with_wiki_store_read(server, &store_ref, |store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|error| format!("wiki list: {error}"))
        });
        let listed = match listed {
            Ok(listed) => listed,
            Err(_) if matches!(plan, WikiReadPlan::Federated) => continue,
            Err(error) => return Err(error),
        };
        entries.extend(
            listed
                .into_iter()
                .filter(is_user_facing_wiki_entry)
                .map(|entry| StoredWikiEntry {
                    entry,
                    store: store_ref.clone(),
                }),
        );
    }
    Ok(entries)
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
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    let mut first_source = "empty";

    if let Ok(entries) = server.with_named_project_store_read(project, |store| {
        store
            .list_by_path("/wiki", limit, false)
            .map_err(|error| format!("wiki list: {error}"))
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
    if merged.len() < limit {
        if let Ok(entries) = server.with_project_store_read(|store| {
            store
                .list_by_path("/wiki", limit, false)
                .map_err(|error| format!("wiki project list: {error}"))
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
                .map_err(|error| format!("wiki fallback list: {error}"))
        }) {
            Ok(entries) => merge_wiki_store_entries(
                &mut merged,
                &mut seen,
                &mut first_source,
                "global",
                entries,
                limit,
            ),
            Err(global_error) if merged.is_empty() => {
                return Err(format!("wiki list: {global_error}"));
            }
            Err(_) => {}
        }
    }

    merged.truncate(limit);
    Ok((merged, first_source))
}

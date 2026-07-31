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
) -> Result<Vec<Value>, String> {
    let entity_set: HashSet<String> = entities
        .iter()
        .map(|entity| entity.trim().to_ascii_lowercase())
        .filter(|entity| !entity.is_empty())
        .collect();
    if entity_set.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let entries = list_related_candidates(server, project, 5000)?;

    let mut related = Vec::new();
    for entry in entries {
        if entry.id != exclude_id
            && is_ordinary_related_wiki_entry(&entry)?
            && entry_shares_normalized_entity(&entry, &entity_set)
        {
            related.push(entry);
        }
    }
    related.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.timestamp.cmp(&a.timestamp))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut seen = HashSet::new();
    Ok(related
        .into_iter()
        .filter(|entry| seen.insert(entry.id.clone()))
        .take(limit)
        .map(|entry| compact_entry(&entry))
        .collect())
}

pub(super) fn entry_shares_normalized_entity(
    entry: &MemoryEntry,
    normalized_entities: &HashSet<String>,
) -> bool {
    entry
        .entities
        .iter()
        .any(|entity| normalized_entities.contains(&entity.trim().to_ascii_lowercase()))
}

/// Related edges from ordinary Wiki ingest may target only the same
/// user-facing, default-retrievable corpus. Drafts and REM operations are
/// intentionally excluded even while their rows are active in SQLite.
pub(super) fn is_ordinary_related_wiki_entry(entry: &MemoryEntry) -> Result<bool, String> {
    Ok(is_user_facing_wiki_entry(entry)
        && entry.path != "/wiki/drafts"
        && !entry.path.starts_with("/wiki/drafts/")
        && !entry.id.starts_with("wiki-rem:")
        && wiki_entry_matches_lifecycle_scope(entry, None)?)
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
    memcore::db::is_user_facing_wiki_entry(entry)
}

#[derive(Debug, Clone)]
pub(crate) struct StoredWikiEntry {
    pub(crate) entry: MemoryEntry,
    pub(crate) store: StoreRef,
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

pub(super) fn stores_for_wiki_plan(server: &MemoryServer, plan: &WikiReadPlan) -> Vec<StoreRef> {
    match plan {
        WikiReadPlan::NamedOnly(store) => vec![store.clone()],
        WikiReadPlan::Federated => {
            let mut stores = Vec::new();
            if server.has_project_db() {
                stores.push(StoreRef::BoundProject);
            }
            if crate::memory_search_ops::named_project_db_exists(
                server,
                LOGICAL_SHARED_WIKI_PROJECT,
            ) && !same_store_path(server)
            {
                stores.push(StoreRef::named(LOGICAL_SHARED_WIKI_PROJECT));
            }
            stores
        }
        WikiReadPlan::ProjectOnly => server
            .has_project_db()
            .then_some(StoreRef::BoundProject)
            .into_iter()
            .collect(),
        WikiReadPlan::SharedOnly => vec![StoreRef::named(LOGICAL_SHARED_WIKI_PROJECT)],
        WikiReadPlan::MigrationAudit | WikiReadPlan::GuideFederated => {
            let mut stores = Vec::new();
            if server.has_project_db() {
                stores.push(StoreRef::BoundProject);
            }
            if crate::memory_search_ops::named_project_db_exists(
                server,
                LOGICAL_SHARED_WIKI_PROJECT,
            ) && !same_store_path(server)
            {
                stores.push(StoreRef::named(LOGICAL_SHARED_WIKI_PROJECT));
            }
            stores.push(StoreRef::LegacyGlobal);
            stores
        }
    }
}

pub(super) fn with_wiki_store_read<T>(
    server: &MemoryServer,
    store_ref: &StoreRef,
    action: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    match store_ref {
        StoreRef::BoundProject => server.with_project_store_read_identity_checked(action),
        StoreRef::NamedProject { project } => {
            server.with_named_project_store_read_identity_checked(project, action)
        }
        StoreRef::LegacyGlobal => server.with_global_store_read_identity_checked(action),
    }
}

pub(super) fn with_wiki_store<T>(
    server: &MemoryServer,
    store_ref: &StoreRef,
    action: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    match store_ref {
        StoreRef::BoundProject => server.with_project_store_identity_checked(action),
        StoreRef::NamedProject { project } => {
            server.with_named_project_store_identity_checked(project, action)
        }
        StoreRef::LegacyGlobal => server.with_global_store_identity_checked(action),
    }
}

pub(crate) fn list_wiki_entries_for_plan(
    server: &MemoryServer,
    plan: &WikiReadPlan,
    path_prefix: &str,
    limit: usize,
) -> Result<Vec<StoredWikiEntry>, String> {
    let mut entries = Vec::new();
    for store_ref in stores_for_wiki_plan(server, plan) {
        let listed = with_wiki_store_read(server, &store_ref, |store| {
            let listed = store.list_user_facing_wiki_entries(
                path_prefix,
                limit,
                matches!(plan, WikiReadPlan::MigrationAudit),
            );
            listed.map_err(|error| format!("wiki list: {error}"))
        });
        let listed = listed?;
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

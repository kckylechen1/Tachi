use super::error::format_save_error;
use crate::memory_search_ops::contradiction::apply_auto_contradiction_detection;
use crate::{DbScope, MemoryServer};
use memcore::{MemoryEntry, MemoryStore};

/// Return the id of an active row with the same normalized path and exact
/// text. #1041 F6: this used to fetch `list_by_path(path, 64, false)` (exact
/// path + descendants, capped at 64 rows) and filter for an exact path+text
/// match in memory — once 64+ rows already existed under that path's
/// descendant family, a genuinely duplicate row could sort past the cutoff
/// and never reach the filter. `find_exact_path_text_id` pushes the exact
/// match into SQL instead, so no window size can hide an existing duplicate.
pub(in crate::memory_search_ops::save_memory) fn find_exact_path_text_duplicate(
    server: &MemoryServer,
    path: &str,
    text: &str,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Result<Option<String>, String> {
    let normalized_path = memcore::path_router::normalize_path(path);
    let lookup = |store: &mut MemoryStore| {
        store
            .find_exact_path_text_id(&normalized_path, text)
            .map_err(|e| format_save_error(server, target_db, named_project, &e))
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store_read(project_name, lookup)
    } else {
        server.with_store_for_scope_read(target_db, lookup)
    }
}

pub(in crate::memory_search_ops::save_memory) fn lookup_existing_entry(
    server: &MemoryServer,
    id: &str,
    requested_id: bool,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Result<Option<MemoryEntry>, String> {
    if !requested_id {
        return Ok(None);
    }

    let lookup = |store: &mut MemoryStore| {
        store
            .get(id)
            .map_err(|e| format_save_error(server, target_db, named_project, &e))
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store_read(project_name, lookup)
    } else {
        server.with_store_for_scope_read(target_db, lookup)
    }
}

pub(in crate::memory_search_ops::save_memory) fn upsert_save_entry(
    server: &MemoryServer,
    entry: &MemoryEntry,
    target_db: DbScope,
    named_project: Option<&str>,
    idless_save: bool,
) -> Result<Option<String>, String> {
    let upsert = |store: &mut MemoryStore| {
        if idless_save {
            match store
                .upsert_idless_deduplicated(entry)
                .map_err(|e| format_save_error(server, target_db, named_project, &e))?
            {
                memcore::db::IdlessSaveWrite::Stored => Ok(None),
                memcore::db::IdlessSaveWrite::Duplicate { existing_id } => Ok(Some(existing_id)),
            }
        } else {
            store
                .upsert(entry)
                .map_err(|e| format_save_error(server, target_db, named_project, &e))?;
            Ok(None)
        }
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store(project_name, upsert)
    } else {
        server.with_store_for_scope(target_db, upsert)
    }
}

pub(in crate::memory_search_ops::save_memory) fn spawn_save_contradiction_detection(
    server: &MemoryServer,
    entry_id: String,
    target_db: DbScope,
    named_project: Option<String>,
) {
    let contradiction_server = server.clone();
    tokio::spawn(async move {
        if let Err(err) = apply_auto_contradiction_detection(
            &contradiction_server,
            &entry_id,
            target_db,
            named_project.as_deref(),
            None,
        )
        .await
        {
            eprintln!("[save_memory] auto contradiction detection failed for {entry_id}: {err}");
        }
    });
}

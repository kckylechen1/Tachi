use super::error::format_save_error;
use crate::memory_search_ops::contradiction::apply_auto_contradiction_detection;
use crate::{DbScope, MemoryServer};
use memory_core::{MemoryEntry, MemoryStore};

/// Return the id of an active row with the same normalized path and exact text.
pub(in crate::memory_search_ops::save_memory) fn find_exact_path_text_duplicate(
    server: &MemoryServer,
    path: &str,
    text: &str,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Result<Option<String>, String> {
    let normalized_path = memory_core::path_router::normalize_path(path);
    let lookup = |store: &mut MemoryStore| {
        let entries = store
            .list_by_path(&normalized_path, 64, false)
            .map_err(|e| format_save_error(server, target_db, named_project, &e))?;
        Ok(entries
            .into_iter()
            .find(|entry| entry.path == normalized_path && entry.text == text)
            .map(|entry| entry.id))
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
) -> Result<(), String> {
    if let Some(project_name) = named_project {
        server.with_named_project_store(project_name, |store: &mut MemoryStore| {
            store
                .upsert(entry)
                .map_err(|e| format_save_error(server, target_db, Some(project_name), &e))
        })
    } else {
        server.with_store_for_scope(target_db, |store: &mut MemoryStore| {
            store
                .upsert(entry)
                .map_err(|e| format_save_error(server, target_db, None, &e))
        })
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

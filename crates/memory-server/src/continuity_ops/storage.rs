use std::path::PathBuf;

use memory_core::{ContinuityMetrics, MemoryEdge, MemoryEntry, TachiEventQuery, TachiEventRecord};
use memory_server_runtime::EventDbRoute;

use crate::{DbScope, MemoryServer};

#[derive(Debug, Clone)]
pub(crate) struct ContinuityEventTarget {
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<PathBuf>,
}

impl ContinuityEventTarget {
    pub(crate) fn new(
        target_db: DbScope,
        named_project: Option<String>,
        db_path: Option<PathBuf>,
    ) -> Self {
        Self {
            target_db,
            named_project,
            db_path,
        }
    }

    pub(crate) fn from_default_write(server: &MemoryServer, project: Option<&str>) -> Self {
        match server.event_db_route(project) {
            EventDbRoute::NamedProject(project) => Self::new(DbScope::Project, Some(project), None),
            EventDbRoute::Project => Self::new(DbScope::Project, None, None),
            EventDbRoute::Global => Self::new(DbScope::Global, None, None),
        }
    }

    pub(super) fn project_label(&self, explicit_project: Option<&str>) -> String {
        let pin = crate::memory_search_ops::explicit_workspace_project();
        let db_parent = self
            .db_path
            .as_ref()
            .and_then(|path| path.parent())
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str());
        if let Some(label) = pick_label(
            explicit_project,
            self.named_project.as_deref(),
            pin.as_deref(),
            db_parent,
        ) {
            return label.to_string();
        }
        // All cheap sources empty — only now pay for the git-root workspace lookup.
        crate::memory_search_ops::resolve_workspace_named_project()
            .unwrap_or_else(|| self.target_db.as_str().to_string())
    }
}

/// Label precedence for a continuity projection over the cheap (non-git-walk)
/// sources: explicit param > store's named project > TACHI_PROJECT pin > db-path
/// parent (git-hash) name. The pin sits ahead of the git-hash name so a pinned
/// repo's direct-daemon projections carry the same label as its pinned reads and
/// proxy-injected writes (#488). Pure — no env/git access — so the ordering is
/// unit-tested without a process-global env race.
fn pick_label<'a>(
    explicit_project: Option<&'a str>,
    named_project: Option<&'a str>,
    pin: Option<&'a str>,
    db_parent_name: Option<&'a str>,
) -> Option<&'a str> {
    [explicit_project, named_project, pin, db_parent_name]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
}

pub(super) fn write_event(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    event: &TachiEventRecord,
) -> Result<(), String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .insert_tachi_event(event)
                .map_err(|e| format!("insert continuity event: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store(db_path, |store| {
            store
                .insert_tachi_event(event)
                .map_err(|e| format!("insert continuity event: {e}"))
        })
    } else {
        server.with_store_for_scope(target.target_db, |store| {
            store
                .insert_tachi_event(event)
                .map_err(|e| format!("insert continuity event: {e}"))
        })
    }
}

pub(super) fn read_events(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    query: &TachiEventQuery,
) -> Result<Vec<TachiEventRecord>, String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store_read(project_name, |store| {
            store
                .list_tachi_events(query)
                .map_err(|e| format!("list continuity events: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store_read(db_path, |store| {
            store
                .list_tachi_events(query)
                .map_err(|e| format!("list continuity events: {e}"))
        })
    } else {
        server.with_store_for_scope_read(target.target_db, |store| {
            store
                .list_tachi_events(query)
                .map_err(|e| format!("list continuity events: {e}"))
        })
    }
}

pub(super) fn upsert_projection_memory(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    entry: &MemoryEntry,
) -> Result<(), String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .upsert(entry)
                .map_err(|e| format!("upsert projection memory: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store(db_path, |store| {
            store
                .upsert(entry)
                .map_err(|e| format!("upsert projection memory: {e}"))
        })
    } else {
        server.with_store_for_scope(target.target_db, |store| {
            store
                .upsert(entry)
                .map_err(|e| format!("upsert projection memory: {e}"))
        })
    }
}

pub(super) fn get_projection_memory(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    id: &str,
) -> Result<Option<MemoryEntry>, String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store_read(project_name, |store| {
            store
                .get(id)
                .map_err(|e| format!("get projection memory: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store_read(db_path, |store| {
            store
                .get(id)
                .map_err(|e| format!("get projection memory: {e}"))
        })
    } else {
        server.with_store_for_scope_read(target.target_db, |store| {
            store
                .get(id)
                .map_err(|e| format!("get projection memory: {e}"))
        })
    }
}

pub(super) fn add_memory_edge(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    edge: &MemoryEdge,
) -> Result<(), String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .add_edge(edge)
                .map_err(|e| format!("add continuity graph edge: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store(db_path, |store| {
            store
                .add_edge(edge)
                .map_err(|e| format!("add continuity graph edge: {e}"))
        })
    } else {
        server.with_store_for_scope(target.target_db, |store| {
            store
                .add_edge(edge)
                .map_err(|e| format!("add continuity graph edge: {e}"))
        })
    }
}

pub(super) fn list_projection_memories(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    path_prefix: &str,
    limit: usize,
) -> Result<Vec<MemoryEntry>, String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store_read(project_name, |store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|e| format!("list projected memories: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store_read(db_path, |store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|e| format!("list projected memories: {e}"))
        })
    } else {
        server.with_store_for_scope_read(target.target_db, |store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|e| format!("list projected memories: {e}"))
        })
    }
}

pub(super) fn continuity_metrics(
    server: &MemoryServer,
    target: &ContinuityEventTarget,
    limit: usize,
) -> Result<ContinuityMetrics, String> {
    if let Some(project_name) = target.named_project.as_deref() {
        server.with_named_project_store_read(project_name, |store| {
            store
                .continuity_metrics(limit)
                .map_err(|e| format!("compute continuity metrics: {e}"))
        })
    } else if let Some(db_path) = target.db_path.as_ref() {
        server.with_path_store_read(db_path, |store| {
            store
                .continuity_metrics(limit)
                .map_err(|e| format!("compute continuity metrics: {e}"))
        })
    } else {
        server.with_store_for_scope_read(target.target_db, |store| {
            store
                .continuity_metrics(limit)
                .map_err(|e| format!("compute continuity metrics: {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::pick_label;

    // #488: the TACHI_PROJECT pin must win over the git-hash db-path parent name
    // so direct-daemon continuity projections align with pinned reads/writes.
    #[test]
    fn pick_label_prefers_pin_over_git_hash_db_parent() {
        assert_eq!(
            pick_label(
                None,
                None,
                Some("trading"),
                Some("Quant_Analyzer_2026-b4773587"),
            ),
            Some("trading")
        );
    }

    #[test]
    fn pick_label_full_precedence_order() {
        // explicit param > named project > pin > db-path parent.
        assert_eq!(
            pick_label(
                Some("explicit"),
                Some("named"),
                Some("pin"),
                Some("dbparent")
            ),
            Some("explicit")
        );
        assert_eq!(
            pick_label(None, Some("named"), Some("pin"), Some("dbparent")),
            Some("named")
        );
        assert_eq!(
            pick_label(None, None, Some("pin"), Some("dbparent")),
            Some("pin")
        );
        // No pin -> db-path parent (git-hash) name — behavior unchanged.
        assert_eq!(
            pick_label(None, None, None, Some("Quant_Analyzer_2026-b4773587")),
            Some("Quant_Analyzer_2026-b4773587")
        );
        assert_eq!(pick_label(None, None, None, None), None);
    }

    #[test]
    fn pick_label_skips_blank_sources() {
        assert_eq!(
            pick_label(Some("   "), None, Some("trading"), None),
            Some("trading")
        );
        assert_eq!(pick_label(None, None, None, Some("  ")), None);
    }
}

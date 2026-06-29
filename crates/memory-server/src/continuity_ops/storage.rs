use std::path::PathBuf;

use memory_core::{ContinuityMetrics, MemoryEdge, MemoryEntry, TachiEventQuery, TachiEventRecord};

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
        if let Some(project) = project.map(str::trim).filter(|value| !value.is_empty()) {
            return Self::new(DbScope::Project, Some(project.to_string()), None);
        }
        if server.has_project_db() {
            Self::new(DbScope::Project, None, None)
        } else {
            Self::new(DbScope::Global, None, None)
        }
    }

    pub(super) fn project_label(&self, explicit_project: Option<&str>) -> String {
        if let Some(project) = explicit_project
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return project.to_string();
        }
        if let Some(project) = self.named_project.as_deref() {
            return project.to_string();
        }
        if let Some(label) = self
            .db_path
            .as_ref()
            .and_then(|path| path.parent())
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .filter(|value| !value.is_empty())
        {
            return label.to_string();
        }
        crate::memory_search_ops::resolve_workspace_named_project()
            .unwrap_or_else(|| self.target_db.as_str().to_string())
    }
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

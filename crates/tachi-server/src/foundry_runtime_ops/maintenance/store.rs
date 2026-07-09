use crate::server_state::MemoryServer;
use crate::utils::stable_hash;
use memcore::{MemoryEntry, MemoryStore};

use super::super::FoundryMaintenanceItem;
use super::enqueue::foundry_job_label;

pub(in crate::foundry_runtime_ops) fn with_foundry_store<T>(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
    f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if let Some(ref db_path) = item.db_path {
        server.with_path_store(db_path, f)
    } else if let Some(project_name) = item.named_project.as_deref() {
        server.with_named_project_store(project_name, f)
    } else {
        server.with_store_for_scope(item.target_db, f)
    }
}

pub(in crate::foundry_runtime_ops) fn with_foundry_store_read<T>(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
    f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if let Some(ref db_path) = item.db_path {
        server.with_path_store_read(db_path, f)
    } else if let Some(project_name) = item.named_project.as_deref() {
        server.with_named_project_store_read(project_name, f)
    } else {
        server.with_store_for_scope_read(item.target_db, f)
    }
}

pub(in crate::foundry_runtime_ops) fn memory_claim_signature(entry: &MemoryEntry) -> String {
    format!(
        "{}:r{}:vec{}:arch{}",
        entry.id,
        entry.revision,
        if entry.vector.is_some() { 1 } else { 0 },
        if entry.archived { 1 } else { 0 }
    )
}

pub(in crate::foundry_runtime_ops) fn build_foundry_event_hash(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<String, String> {
    let mut signatures = Vec::with_capacity(item.memory_ids.len());
    for memory_id in &item.memory_ids {
        let maybe_entry = with_foundry_store_read(server, item, |store| {
            store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {} for claim hash: {e}", memory_id))
        })?;
        match maybe_entry {
            Some(entry) => signatures.push(memory_claim_signature(&entry)),
            None => signatures.push(format!("{memory_id}:missing")),
        }
    }
    signatures.sort();
    let job_scope = match item.job.kind {
        memcore::FoundryJobKind::RecallRerankCache => stable_hash(&item.job.metadata.to_string()),
        _ => String::new(),
    };

    Ok(stable_hash(&format!(
        "{}:{}:{}:{}:{}",
        foundry_job_label(&item.job.kind),
        item.named_project.as_deref().unwrap_or("default"),
        item.path_prefix,
        job_scope,
        signatures.join(","),
    )))
}

pub(in crate::foundry_runtime_ops) fn merge_foundry_metadata(
    existing: &serde_json::Value,
    patch: serde_json::Value,
) -> serde_json::Value {
    let mut root = match existing {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    let mut foundry = root
        .get("foundry")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Some(patch_obj) = patch.as_object() {
        for (key, value) in patch_obj {
            foundry.insert(key.clone(), value.clone());
        }
    }
    root.insert("foundry".into(), serde_json::Value::Object(foundry));
    serde_json::Value::Object(root)
}

pub(in crate::foundry_runtime_ops) fn update_entry_metadata(
    store: &mut MemoryStore,
    entry: &MemoryEntry,
    metadata: &serde_json::Value,
) -> Result<bool, String> {
    store
        .update_with_revision(
            &entry.id,
            &entry.text,
            &entry.summary,
            &entry.source,
            metadata,
            entry.vector.as_deref(),
            entry.revision,
        )
        .map_err(|e| format!("Failed to update foundry metadata for {}: {e}", entry.id))
}

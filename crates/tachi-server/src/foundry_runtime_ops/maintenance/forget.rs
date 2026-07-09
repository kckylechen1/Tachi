use super::super::{FoundryMaintenanceItem, FOUNDRY_DISTILL_KEEP, FOUNDRY_DISTILL_SOURCE};
use super::store::{with_foundry_store, with_foundry_store_read};
use crate::server_state::MemoryServer;
use tachi_foundry::build_foundry_distill_root;

pub(super) fn process_forget_sweep_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<usize, String> {
    let agent_id = item
        .job
        .target_agent_id
        .as_deref()
        .unwrap_or("unknown-agent");
    let distill_root = build_foundry_distill_root(agent_id);
    let distill_entries = with_foundry_store_read(server, item, |store| {
        store
            .list_by_path(&distill_root, FOUNDRY_DISTILL_KEEP + 12, false)
            .map_err(|e| format!("Failed to list foundry distill memories: {e}"))
    })?;

    let mut distill_entries = distill_entries
        .into_iter()
        .filter(|entry| entry.source == FOUNDRY_DISTILL_SOURCE)
        .collect::<Vec<_>>();
    distill_entries.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.path.cmp(&a.path))
            .then_with(|| b.id.cmp(&a.id))
    });

    let stale_ids = distill_entries
        .into_iter()
        .skip(FOUNDRY_DISTILL_KEEP)
        .map(|entry| entry.id)
        .collect::<Vec<_>>();

    let mut archived = 0usize;
    for stale_id in stale_ids {
        let changed = with_foundry_store(server, item, |store| {
            store
                .archive_memory(&stale_id)
                .map_err(|e| format!("Failed to archive stale foundry distill {}: {e}", stale_id))
        })?;
        if changed {
            archived += 1;
        }
    }

    Ok(archived)
}

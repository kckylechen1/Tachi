use super::super::{FoundryMaintenanceItem, FOUNDRY_DISTILL_KEEP, FOUNDRY_DISTILL_SOURCE};
use super::store::{with_foundry_store, with_foundry_store_read};
use crate::server_state::MemoryServer;
use memory_core::MemoryEntry;
use tachi_foundry::build_foundry_distill_root;

/// How far past `FOUNDRY_DISTILL_KEEP` we scan so old distill tails are not left
/// behind when the newest window is dense (#775 forget policy).
const FOUNDRY_DISTILL_SCAN_EXTRA: usize = 48;

/// Foundry forget_sweep policy (#775 / neural-foundry harden):
///
/// 1. List distill-root rows with `source=FOUNDRY_DISTILL_SOURCE`.
/// 2. Sort newest-first (timestamp, then path, then id).
/// 3. **Never** archive `permanent` / `pinned` / `durable` retention.
/// 4. Keep the newest `FOUNDRY_DISTILL_KEEP` *eligible* rows.
/// 5. Archive the remainder (reversible; no physical delete).
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
            .list_by_path(
                &distill_root,
                FOUNDRY_DISTILL_KEEP + FOUNDRY_DISTILL_SCAN_EXTRA,
                false,
            )
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

    let stale_ids = select_forget_sweep_archive_ids(&distill_entries, FOUNDRY_DISTILL_KEEP);

    let mut archived = 0usize;
    for stale_id in stale_ids {
        let changed = with_foundry_store(server, item, |store| {
            // Re-check protection at apply time (retention may have changed).
            if let Some(entry) = store
                .get(&stale_id)
                .map_err(|e| format!("load distill {stale_id}: {e}"))?
            {
                if is_protected_distill(&entry) {
                    return Ok(false);
                }
            }
            store
                .archive_memory(&stale_id)
                .map_err(|e| format!("Failed to archive stale foundry distill {stale_id}: {e}"))
        })?;
        if changed {
            archived += 1;
        }
    }

    Ok(archived)
}

/// Pure selection of archive candidates (testable without a DB).
pub(super) fn select_forget_sweep_archive_ids(
    newest_first: &[MemoryEntry],
    keep: usize,
) -> Vec<String> {
    let mut kept = 0usize;
    let mut archive = Vec::new();
    for entry in newest_first {
        if is_protected_distill(entry) {
            // Protected rows never count against the keep budget and are never
            // archived by this sweep.
            continue;
        }
        if kept < keep {
            kept += 1;
            continue;
        }
        archive.push(entry.id.clone());
    }
    archive
}

fn is_protected_distill(entry: &MemoryEntry) -> bool {
    matches!(
        entry
            .retention_policy
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("permanent" | "pinned" | "durable")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn distill(id: &str, ts: &str, retention: Option<&str>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: format!("/foundry/agents/main/distilled/{id}"),
            summary: id.to_string(),
            text: id.to_string(),
            importance: 0.7,
            timestamp: ts.to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "other".to_string(),
            topic: "foundry_distill".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: FOUNDRY_DISTILL_SOURCE.to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: retention.map(|s| s.to_string()),
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn forget_sweep_keeps_newest_and_spares_protected() {
        // Newest-first list: keep 2 eligible; archive older; never archive durable.
        let rows = vec![
            distill("new", "2026-04-03T00:00:00Z", None),
            distill("durable", "2026-04-02T12:00:00Z", Some("durable")),
            distill("mid", "2026-04-02T00:00:00Z", None),
            distill("old", "2026-04-01T00:00:00Z", None),
            distill("pinned", "2026-03-01T00:00:00Z", Some("pinned")),
        ];
        let archive = select_forget_sweep_archive_ids(&rows, 2);
        assert_eq!(archive, vec!["old".to_string()]);
        assert!(!archive.iter().any(|id| id == "durable" || id == "pinned"));
        assert!(!archive.iter().any(|id| id == "new" || id == "mid"));
    }
}

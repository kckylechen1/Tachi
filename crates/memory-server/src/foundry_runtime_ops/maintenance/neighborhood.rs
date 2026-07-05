use super::super::capture::{
    merge_capture_entries, persist_capture_entry, queue_capture_enrichment,
    search_similar_capture_entries,
};
use super::super::helpers::round3;
use super::super::{
    FoundryMaintenanceItem, CAPTURE_DEDUP_THRESHOLD, CAPTURE_MERGE_THRESHOLD, FOUNDRY_RELATED_LIMIT,
};
use super::store::{
    merge_foundry_metadata, update_entry_metadata, with_foundry_store, with_foundry_store_read,
};
use crate::server_state::MemoryServer;
use chrono::Utc;
use serde_json::json;
use tachi_foundry::infer_memory_insight;

pub(super) async fn process_memory_neighborhood_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<usize, String> {
    let mut updated = 0usize;

    let avg_importance = with_foundry_store_read(server, item, |store| {
        store
            .avg_importance()
            .map_err(|e| format!("Failed to compute average importance: {e}"))
    })?;

    for memory_id in &item.memory_ids {
        let Some(entry) = with_foundry_store_read(server, item, |store| {
            store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {} for neighborhood: {e}", memory_id))
        })?
        else {
            continue;
        };

        let Some(vector) = entry.vector.clone() else {
            continue;
        };

        let neighbors = search_similar_capture_entries(
            server,
            item.target_db,
            item.named_project.as_deref(),
            item.db_path.as_ref(),
            &item.path_prefix,
            &vector,
            FOUNDRY_RELATED_LIMIT + 2,
        )?;

        let mut best_neighbor: Option<memory_core::SearchResult> = None;
        let related = neighbors
            .into_iter()
            .filter(|row| row.entry.id != entry.id)
            .inspect(|row| {
                if best_neighbor.is_none() {
                    best_neighbor = Some(row.clone());
                }
            })
            .take(FOUNDRY_RELATED_LIMIT)
            .map(|row| {
                json!({
                    "id": row.entry.id,
                    "topic": row.entry.topic,
                    "path": row.entry.path,
                    "score": round3(row.score.vector),
                })
            })
            .collect::<Vec<_>>();

        if let Some(similar) = best_neighbor {
            let similarity = similar.score.vector;
            if similarity >= CAPTURE_DEDUP_THRESHOLD {
                let changed = with_foundry_store(server, item, |store| {
                    store.archive_memory(&entry.id).map_err(|e| {
                        format!("Failed to archive duplicate memory {}: {e}", entry.id)
                    })
                })?;
                if changed {
                    updated += 1;
                }
                continue;
            }

            if similarity >= CAPTURE_MERGE_THRESHOLD {
                let merged = merge_capture_entries(&similar.entry, &entry, similarity);
                persist_capture_entry(
                    server,
                    item.target_db,
                    item.named_project.as_deref(),
                    item.db_path.as_ref(),
                    &merged,
                )?;
                let changed = with_foundry_store(server, item, |store| {
                    store
                        .archive_memory(&entry.id)
                        .map_err(|e| format!("Failed to archive merged memory {}: {e}", entry.id))
                })?;
                if changed {
                    updated += 1;
                }
                queue_capture_enrichment(
                    server,
                    item.target_db,
                    item.named_project.clone(),
                    item.db_path.clone(),
                    &merged,
                    true,
                    item.job.target_agent_id.as_deref(),
                    Some(&item.path_prefix),
                );
                continue;
            }
        }

        if related.is_empty() {
            continue;
        }

        let (contradiction_count, same_topic_count) =
            with_foundry_store_read(server, item, |store| {
                let contradiction_count = store
                    .get_contradiction_count(&entry.id)
                    .map_err(|e| format!("Failed to count contradictions for {}: {e}", entry.id))?;
                let topic = entry.topic.trim();
                let same_topic_count = if topic.is_empty() {
                    0u32
                } else {
                    store.count_same_topic(topic).map_err(|e| {
                        format!("Failed to count same-topic memories for {}: {e}", entry.id)
                    })?
                };
                Ok((contradiction_count, same_topic_count))
            })?;
        let insight = infer_memory_insight(
            &entry,
            avg_importance,
            contradiction_count,
            same_topic_count,
            related.len(),
            FOUNDRY_RELATED_LIMIT,
        );

        let metadata = merge_foundry_metadata(
            &entry.metadata,
            json!({
                "last_neighborhood_at": Utc::now().to_rfc3339(),
                "neighborhood_job_id": item.job.id,
                "related_entries": related,
                "insight": insight,
            }),
        );

        let applied = with_foundry_store(server, item, |store| {
            update_entry_metadata(store, &entry, &metadata)
        })?;
        if applied {
            updated += 1;
        }
    }

    Ok(updated)
}

use serde_json::json;

use crate::server_state::{MemoryServer, TOOL_CACHE_TTL};
use crate::shared_defs::{DLQ_MAX_ENTRIES, DLQ_TTL_SECS};

pub(crate) async fn handle_get_pipeline_status(server: &MemoryServer) -> Result<String, String> {
    let global_stats = server.with_global_store(|store| {
        store
            .stats(false)
            .map_err(|e| format!("Failed to get global stats: {}", e))
    })?;

    let project_stats = if server.has_project_db() {
        Some(server.with_project_store(|store| {
            store
                .stats(false)
                .map_err(|e| format!("Failed to get project stats: {}", e))
        })?)
    } else {
        None
    };

    let mut total_entries = global_stats.total;
    let mut by_scope = global_stats.by_scope;
    let mut by_category = global_stats.by_category;

    if let Some(project_stats) = project_stats {
        total_entries += project_stats.total;
        for (k, v) in project_stats.by_scope {
            *by_scope.entry(k).or_insert(0) += v;
        }
        for (k, v) in project_stats.by_category {
            *by_category.entry(k).or_insert(0) += v;
        }
    }

    let cache_size = server.tool_cache_lock().len();

    let (dlq_total, dlq_pending, dlq_resolved, dlq_abandoned) = {
        let dlq = server.dead_letters_lock();
        let total = dlq.len();
        let pending = dlq.iter().filter(|d| d.status == "pending").count();
        let resolved = dlq.iter().filter(|d| d.status == "resolved").count();
        let abandoned = dlq.iter().filter(|d| d.status == "abandoned").count();
        (total, pending, resolved, abandoned)
    };
    let hits = server.cache_hits.load(std::sync::atomic::Ordering::Relaxed);
    let misses = server
        .cache_misses
        .load(std::sync::atomic::Ordering::Relaxed);
    let foundry = json!({
        "queued": server.foundry_lock().foundry_stats.queued.load(std::sync::atomic::Ordering::Relaxed),
        "running": server.foundry_lock().foundry_stats.running.load(std::sync::atomic::Ordering::Relaxed),
        "completed": server.foundry_lock().foundry_stats.completed.load(std::sync::atomic::Ordering::Relaxed),
        "failed": server.foundry_lock().foundry_stats.failed.load(std::sync::atomic::Ordering::Relaxed),
        "skipped": server.foundry_lock().foundry_stats.skipped.load(std::sync::atomic::Ordering::Relaxed),
    });

    serde_json::to_string(&json!({
        "status": "running",
        "workers": if server.pipeline_enabled { "rust_async" } else { "disabled" },
        "total_entries": total_entries,
        "by_scope": by_scope,
        "by_category": by_category,
        "vec_available": {
            "global": server.global_vec_available,
            "project": server.project_vec_available,
        },
        "pipeline_enabled": server.pipeline_enabled,
        "phantom_tools": {
            "cache_size": cache_size,
            "cache_hits": hits,
            "cache_misses": misses,
            "ttl_seconds": TOOL_CACHE_TTL.as_secs(),
        },
        "dead_letter_queue": {
            "total": dlq_total,
            "pending": dlq_pending,
            "resolved": dlq_resolved,
            "abandoned": dlq_abandoned,
            "max_entries": DLQ_MAX_ENTRIES,
            "ttl_seconds": DLQ_TTL_SECS,
        },
        "foundry": foundry,
    }))
    .map_err(|e| format!("Failed to serialize response: {}", e))
}

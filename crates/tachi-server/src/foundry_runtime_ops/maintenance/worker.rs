use super::super::recall_cache::process_recall_rerank_cache_job;
use super::super::FoundryMaintenanceItem;
use super::distill_job::process_memory_distill_job;
use super::enqueue::{foundry_job_label, foundry_worker_name};
use super::forget::process_forget_sweep_job;
use super::neighborhood::process_memory_neighborhood_job;
use super::store::{build_foundry_event_hash, with_foundry_store};
use super::DistillOutcome;
use crate::server_state::MemoryServer;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

enum FoundryMaintenanceOutcome {
    Terminal(memcore::FoundryJobStatus, Option<String>),
    NotClaimed,
}

fn unsupported_foundry_job_reason(kind: &memcore::FoundryJobKind) -> String {
    format!("unsupported_foundry_job_kind:{}", foundry_job_label(kind))
}

async fn handle_foundry_maintenance_item(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<FoundryMaintenanceOutcome, String> {
    let running_cutoff = (chrono::Utc::now()
        - chrono::Duration::seconds(crate::status_ops::STUCK_THRESHOLD_SECS))
    .to_rfc3339();
    let lease = with_foundry_store(server, item, |store| {
        memcore::claim_foundry_job_for_run(store.connection(), &item.job.id, &running_cutoff)
            .map_err(|e| format!("Failed to lease foundry job {}: {e}", item.job.id))
    })?;
    let Some(lease) = lease else {
        return Ok(FoundryMaintenanceOutcome::NotClaimed);
    };

    let worker = foundry_worker_name(&item.job.kind);
    let event_hash = build_foundry_event_hash(server, item)?;
    if matches!(lease, memcore::FoundryJobLease::StaleRunning) {
        if let Err(err) = with_foundry_store(server, item, |store| {
            store
                .release_event_claim(&event_hash, worker)
                .map_err(|e| format!("Failed to release stale foundry claim {}: {e}", item.job.id))
        }) {
            tracing::warn!(
                error = %err,
                job_id = %item.job.id,
                "failed to release stale foundry claim"
            );
        }
    }
    let claimed = with_foundry_store(server, item, |store| {
        store
            .try_claim_event(&event_hash, &item.job.id, worker)
            .map_err(|e| format!("Failed to claim foundry job {}: {e}", item.job.id))
    })?;

    if !claimed {
        return Ok(FoundryMaintenanceOutcome::Terminal(
            memcore::FoundryJobStatus::Skipped,
            Some("event_already_claimed".to_string()),
        ));
    }

    let result = match item.job.kind {
        memcore::FoundryJobKind::MemoryNeighborhood => {
            process_memory_neighborhood_job(server, item)
                .await
                .map(|_| {
                    FoundryMaintenanceOutcome::Terminal(memcore::FoundryJobStatus::Completed, None)
                })
        }
        memcore::FoundryJobKind::RecallRerankCache => process_recall_rerank_cache_job(server, item)
            .await
            .map(|_| {
                FoundryMaintenanceOutcome::Terminal(memcore::FoundryJobStatus::Completed, None)
            }),
        memcore::FoundryJobKind::MemoryDistill => process_memory_distill_job(server, item)
            .await
            .map(|outcome| match outcome {
                DistillOutcome::Wrote => {
                    FoundryMaintenanceOutcome::Terminal(memcore::FoundryJobStatus::Completed, None)
                }
                DistillOutcome::Skipped(reason) => FoundryMaintenanceOutcome::Terminal(
                    memcore::FoundryJobStatus::Skipped,
                    Some(reason),
                ),
            }),
        memcore::FoundryJobKind::ForgetSweep => process_forget_sweep_job(server, item).map(|_| {
            FoundryMaintenanceOutcome::Terminal(memcore::FoundryJobStatus::Completed, None)
        }),
        _ => Ok(FoundryMaintenanceOutcome::Terminal(
            memcore::FoundryJobStatus::Skipped,
            Some(unsupported_foundry_job_reason(&item.job.kind)),
        )),
    };

    if let Err(err) = &result {
        if let Err(release_err) = with_foundry_store(server, item, |store| {
            store
                .release_event_claim(&event_hash, worker)
                .map_err(|e| format!("Failed to release foundry job claim {}: {e}", item.job.id))
        }) {
            tracing::warn!(
                error = %release_err,
                job_id = %item.job.id,
                "failed to release foundry claim after processing error"
            );
        }
        return Err(err.clone());
    }

    result
}

pub(crate) async fn run_foundry_maintenance_worker(
    server: MemoryServer,
    mut rx: mpsc::Receiver<FoundryMaintenanceItem>,
) {
    while let Some(item) = rx.recv().await {
        if item.counted_queue_slot {
            server
                .foundry_lock()
                .foundry_stats
                .queued
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                .ok();
        }
        server
            .foundry_lock()
            .foundry_stats
            .running
            .fetch_add(1, Ordering::Relaxed);

        let result = handle_foundry_maintenance_item(&server, &item).await;

        server
            .foundry_lock()
            .foundry_stats
            .running
            .fetch_sub(1, Ordering::Relaxed);

        // Branch #5 + PR-C: capture a structured reason for non-completed
        // terminal transitions so `tachi doctor --jobs` and post-mortems
        // can surface *why* a job skipped/failed instead of just the bare
        // status. The reason is now returned in-band by
        // `handle_foundry_maintenance_item` (no global stash, no draining).
        let terminal = match &result {
            Ok(FoundryMaintenanceOutcome::NotClaimed) => None,
            Ok(FoundryMaintenanceOutcome::Terminal(status, reason)) => {
                let (status_str, reason): (&str, Option<String>) = match status {
                    memcore::FoundryJobStatus::Skipped => (
                        "skipped",
                        Some(reason.clone().unwrap_or_else(|| {
                            "worker reported no-op (no qualifying inputs)".to_string()
                        })),
                    ),
                    _ => ("completed", None),
                };
                Some((status_str, reason))
            }
            Err(e) => Some(("failed", Some(e.clone()))),
        };
        if let Some((status_str, reason)) = terminal {
            if let Err(err) = with_foundry_store(&server, &item, |store| {
                memcore::update_foundry_job_status_with_reason(
                    store.connection(),
                    &item.job.id,
                    status_str,
                    reason.as_deref(),
                )
                .map_err(|e| format!("update foundry job status: {e}"))
            }) {
                tracing::warn!(
                    error = %err,
                    job_id = %item.job.id,
                    status = %status_str,
                    "failed to persist foundry job terminal status"
                );
            }
        }

        match result {
            Ok(FoundryMaintenanceOutcome::NotClaimed) => {
                tracing::debug!(
                    "[foundry-worker] job {} already leased or terminal; dropping duplicate item",
                    item.job.id
                );
            }
            Ok(FoundryMaintenanceOutcome::Terminal(memcore::FoundryJobStatus::Skipped, _)) => {
                server
                    .foundry_lock()
                    .foundry_stats
                    .skipped
                    .fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {
                server
                    .foundry_lock()
                    .foundry_stats
                    .completed
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => {
                tracing::warn!("[foundry-worker] job {} failed: {err}", item.job.id);
                server
                    .foundry_lock()
                    .foundry_stats
                    .failed
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    tracing::debug!("[foundry-worker] channel closed, worker exiting");
}

use crate::foundry_runtime_ops::FoundryMaintenanceItem;
use crate::server_state::MemoryServer;

impl MemoryServer {
    pub(crate) fn enqueue_foundry_job(&self, item: FoundryMaintenanceItem) -> Result<(), String> {
        // Persist to DB first so the job survives process exit
        let persisted = memcore::PersistedFoundryJob {
            spec: item.job.clone(),
            target_db: item.target_db.as_str().to_string(),
            named_project: item.named_project.clone(),
            path_prefix: item.path_prefix.clone(),
            memory_ids: item.memory_ids.clone(),
        };
        let persist_result = if let Some(ref db_path) = item.db_path {
            self.with_path_store(db_path, |store| {
                memcore::insert_foundry_job(store.connection(), &persisted)
                    .map_err(|e| format!("persist foundry job: {e}"))
            })
        } else if let Some(ref project_name) = item.named_project {
            self.with_named_project_store(project_name, |store| {
                memcore::insert_foundry_job(store.connection(), &persisted)
                    .map_err(|e| format!("persist foundry job: {e}"))
            })
        } else {
            self.with_store_for_scope(item.target_db, |store| {
                memcore::insert_foundry_job(store.connection(), &persisted)
                    .map_err(|e| format!("persist foundry job: {e}"))
            })
        };
        if let Err(err) = persist_result {
            eprintln!("[foundry] failed to persist job {}: {err}", item.job.id);
        }

        self.foundry_lock()
            .foundry_stats
            .queued
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.foundry_lock().foundry_tx.try_send(item).is_err() {
            self.foundry_lock()
                .foundry_stats
                .queued
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            return Err("foundry maintenance worker unavailable".to_string());
        }
        Ok(())
    }
}

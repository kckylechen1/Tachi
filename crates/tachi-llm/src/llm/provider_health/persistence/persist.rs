use super::*;

// This is deliberately below doctor's 10-second join deadline. A one-shot
// doctor process may not time out a waiter and then leave a `spawn_blocking`
// SQLite writer alive; the writer itself must return before the process may
// emit its terminal receipt. The two-second SQLite budget leaves room for the
// bounded MemCore startup retry to return its typed BUSY/LOCKED error and for
// the supervisor to join the exact blocking task.
const PROVIDER_HEALTH_PERSIST_SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(test)]
fn provider_key_health_persist_disabled_for_tests() -> bool {
    matches!(
        std::env::var("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

#[cfg(not(test))]
fn provider_key_health_persist_disabled_for_tests() -> bool {
    false
}

impl super::super::super::LlmClient {
    pub async fn await_provider_health_persistence(&self) -> Result<(), String> {
        let tracker = self
            .provider_health_persist
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tracker();
        tracker.wait_until_terminal().await
    }

    /// Queue a background write of this key's health snapshot.
    ///
    /// Not a debounce: every call is tracked by the persist tracker and
    /// schedules its own background task. Pending snapshots of one key are
    /// written in enqueue order; a pending plain-success snapshot may be
    /// replaced by a newer plain-success snapshot of the same key, evidence
    /// and credential generation, in which case the later task finds nothing
    /// left to write (see `provider_health/writer.rs`). The write reuses the
    /// client's retained vault handle when it is still valid.
    pub(in crate::llm::provider_health::persistence) fn persist_key_health(
        &self,
        health: &VaultKeyHealth,
    ) {
        if provider_key_health_persist_disabled_for_tests() {
            return;
        }
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let mut health = health.clone();
        health.updated_at = Self::format_now_utc();
        let migration = self.vault_db_migration.clone();
        let writer = Arc::clone(&self.provider_persist_writer);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let logical_name = health.logical_name.clone();
            let key_id = health.key_id.clone();
            let persist_state = Arc::clone(&self.provider_health_persist);
            let tracker = persist_state
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .tracker();
            let background_persist_lock = Arc::clone(&self.background_persist_lock);
            let completion = tracker.track();
            // Queued before the task exists, so a task of this key that runs
            // first writes this snapshot (or a newer merge of it) in order.
            writer.enqueue_key_health(health);
            handle.spawn(async move {
                let _completion = completion;
                let result = {
                    let _persist_guard = background_persist_lock.lock().await;
                    let task_logical_name = logical_name.clone();
                    let task_key_id = key_id.clone();
                    tokio::task::spawn_blocking(move || {
                        let Some(health) = writer.take_key_health(&task_logical_name, &task_key_id)
                        else {
                            // An earlier task of this key already wrote the
                            // snapshot this event was merged into.
                            return Ok(None);
                        };
                        Self::persist_key_health_blocking(&writer, db_path, migration, health)
                            .map(Some)
                    })
                    .await
                    .map_err(|err| {
                        let cause = if err.is_cancelled() {
                            crate::llm::PROVIDER_HEALTH_PERSIST_CANCELLED_CAUSE
                        } else {
                            "provider_health_persist_join_failure"
                        };
                        format!(
                            "persist vault key health for {logical_name}:{key_id}: {cause}: {err}"
                        )
                    })
                    .and_then(|inner| inner)
                };
                match result {
                    // Nothing was written by this task, so there is no attempt
                    // to record: the task that wrote the merged snapshot
                    // recorded its own result.
                    Ok(None) => {}
                    Ok(Some(())) => Self::record_key_health_persist_result(&persist_state, Ok(())),
                    Err(err) => Self::record_key_health_persist_result(&persist_state, Err(err)),
                }
            });
        } else {
            let result = Self::persist_key_health_blocking(&writer, db_path, migration, health);
            Self::record_key_health_persist_result(&self.provider_health_persist, result);
        }
    }

    pub(in crate::llm::provider_health::persistence) fn persist_key_health_now(
        &self,
        health: &VaultKeyHealth,
    ) {
        if provider_key_health_persist_disabled_for_tests() {
            return;
        }
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let mut health = health.clone();
        health.updated_at = Self::format_now_utc();
        let result = Self::persist_key_health_blocking(
            &self.provider_persist_writer,
            db_path,
            self.vault_db_migration.clone(),
            health,
        );
        Self::record_key_health_persist_result(&self.provider_health_persist, result);
    }

    fn persist_key_health_blocking(
        writer: &ProviderPersistWriter,
        db_path: PathBuf,
        migration: memcore::MigrationAuthority,
        health: VaultKeyHealth,
    ) -> Result<(), String> {
        let target = format!("{}:{}", health.logical_name, health.key_id);
        let Some(db_path_str) = db_path.to_str() else {
            return Err(format!(
                "persist vault key health for {target}: invalid db path"
            ));
        };
        let open_context = memcore::DbOpenContext {
            intent: memcore::OpenIntent::OpenExisting,
            migration,
            // #1585 D2: this writes `vault_key_health`, a product table.
            required_profile: memcore::ProfileRequirement::AtLeast(
                memcore::StoreProfile::TachiFull,
            ),
        };
        // #1680 D6 on both paths: a retained handle writes under startup
        // ownership, and a fresh open keeps open and upsert under one hold.
        writer
            .write(
                &db_path,
                |store| store.vault_upsert_key_health_with_startup_ownership(&health),
                || {
                    memcore::MemoryStore::open_and_vault_upsert_key_health_retaining_store(
                        db_path_str,
                        &open_context,
                        PROVIDER_HEALTH_PERSIST_SQLITE_BUSY_TIMEOUT,
                        &health,
                    )
                    .map(|store| (store, ()))
                },
            )
            .map_err(|err| Self::provider_health_persist_error(&target, err))?;
        #[cfg(test)]
        writer.note_key_health_written(&health);
        Ok(())
    }

    fn provider_health_persist_error(target: &str, error: memcore::MemoryError) -> String {
        let deadline_exhausted = matches!(
            &error,
            memcore::MemoryError::Sqlite(sqlite) if memcore::db::sqlite_error_is_locked(sqlite)
        );
        if deadline_exhausted {
            format!(
                "persist vault key health for {target}: {}: {error}",
                crate::llm::PROVIDER_HEALTH_PERSIST_SQLITE_DEADLINE_CAUSE
            )
        } else {
            format!("persist vault key health for {target}: {error}")
        }
    }

    fn record_key_health_persist_result(
        persist_state: &Arc<RwLock<ProviderHealthPersistState>>,
        result: Result<(), String>,
    ) {
        let terminal_error = result.as_ref().err().cloned();
        let now = Instant::now();
        let now_utc = Self::format_now_utc();
        let mut state = persist_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match result {
            Ok(()) => state.mark_success(now, now_utc),
            Err(err) => {
                tracing::warn!("[provider] {err}");
                state.mark_error(now_utc, err);
            }
        }
        if let Some(error) = terminal_error {
            state.tracker().record_error(error);
        }
    }
}

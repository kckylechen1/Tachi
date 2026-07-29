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
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let logical_name = health.logical_name.clone();
            let key_id = health.key_id.clone();
            let persist_state = Arc::clone(&self.provider_health_persist);
            let tracker = persist_state
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .tracker();
            tracker.begin();
            handle.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    Self::persist_key_health_blocking(db_path, migration, health)
                })
                .await
                .map_err(|err| {
                    let cause = if err.is_cancelled() {
                        crate::llm::PROVIDER_HEALTH_PERSIST_CANCELLED_CAUSE
                    } else {
                        "provider_health_persist_join_failure"
                    };
                    format!("persist vault key health for {logical_name}:{key_id}: {cause}: {err}")
                })
                .and_then(|inner| inner);
                Self::record_key_health_persist_result(&persist_state, result);
                tracker.complete();
            });
        } else {
            let result = Self::persist_key_health_blocking(db_path, migration, health);
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
        let result =
            Self::persist_key_health_blocking(db_path, self.vault_db_migration.clone(), health);
        Self::record_key_health_persist_result(&self.provider_health_persist, result);
    }

    fn persist_key_health_blocking(
        db_path: PathBuf,
        migration: memcore::MigrationAuthority,
        health: VaultKeyHealth,
    ) -> Result<(), String> {
        let target = format!("{}:{}", health.logical_name, health.key_id);
        let Some(db_path) = db_path.to_str() else {
            return Err(format!(
                "persist vault key health for {target}: invalid db path"
            ));
        };
        let open_context = memcore::DbOpenContext {
            intent: memcore::OpenIntent::OpenExisting,
            migration,
        };
        match memcore::MemoryStore::open_with_context_and_busy_timeout(
            db_path,
            &open_context,
            PROVIDER_HEALTH_PERSIST_SQLITE_BUSY_TIMEOUT,
        ) {
            Ok(store) => {
                store
                    .vault_upsert_key_health(&health)
                    .map_err(|err| Self::provider_health_persist_error(&target, err))?;
                Ok(())
            }
            Err(err) => Err(Self::provider_health_persist_error(&target, err)),
        }
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

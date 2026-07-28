use super::*;

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
                    format!("persist vault key health for {logical_name}:{key_id}: {err}")
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
        match memcore::MemoryStore::open_with_context(db_path, &open_context) {
            Ok(store) => {
                store
                    .vault_upsert_key_health(&health)
                    .map_err(|err| format!("persist vault key health for {target}: {err}"))?;
                Ok(())
            }
            Err(err) => Err(format!("persist vault key health for {target}: {err}")),
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

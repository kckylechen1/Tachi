use super::*;

impl super::super::super::LlmClient {
    pub(in crate::llm::provider_health) fn initial_key_health_from_db(
        vault_db_path: Option<&Path>,
    ) -> (
        HashMap<String, HashMap<String, VaultKeyHealth>>,
        ProviderHealthReloadState,
    ) {
        let Some(path) = vault_db_path else {
            return (HashMap::new(), ProviderHealthReloadState::memory_only());
        };
        let now = Instant::now();
        let now_utc = Self::format_now_utc();
        let mut reload = ProviderHealthReloadState::vault_db_attempt(now, now_utc.clone());
        match Self::load_key_health_from_db(path) {
            Ok(health) => {
                reload.mark_success(now, now_utc);
                (health, reload)
            }
            Err(err) => {
                reload.mark_error(now, now_utc, err);
                (HashMap::new(), reload)
            }
        }
    }

    fn load_key_health_from_db(
        path: &Path,
    ) -> Result<HashMap<String, HashMap<String, VaultKeyHealth>>, String> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let Some(db_path) = path.to_str() else {
            return Err("Invalid vault db path".to_string());
        };
        let store = memory_core::MemoryStore::open_read_only(db_path)
            .map_err(|e| format!("Open vault db failed: {e}"))?;
        let rows = store
            .vault_list_key_health(None)
            .map_err(|e| format!("Load vault key health failed: {e}"))?;

        Ok(rows.into_iter().fold(HashMap::new(), |mut map, row| {
            map.entry(row.logical_name.clone())
                .or_insert_with(HashMap::new)
                .insert(row.key_id.clone(), row);
            map
        }))
    }

    fn key_health_snapshot_is_newer_or_equal(
        incoming: &ProviderHealthSnapshot,
        existing: &ProviderHealthSnapshot,
    ) -> bool {
        match (incoming.updated_at, existing.updated_at) {
            (Some(incoming_ts), Some(existing_ts)) => incoming_ts >= existing_ts,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => true,
        }
    }

    fn merge_loaded_key_health(&self, loaded: HashMap<String, HashMap<String, VaultKeyHealth>>) {
        let now_utc = Self::now_utc();
        let loaded = loaded
            .into_iter()
            .flat_map(|(logical_name, members)| {
                members.into_iter().map(move |(key_id, health)| {
                    let snapshot = ProviderHealthSnapshot::from_health(&health);
                    (logical_name.clone(), key_id, health, snapshot)
                })
            })
            .collect::<Vec<_>>();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (logical_name, key_id, incoming, snapshot) in loaded {
            let should_apply = state
                .health_snapshots
                .get(&logical_name)
                .and_then(|target| target.get(&key_id))
                .map(|existing| Self::key_health_snapshot_is_newer_or_equal(&snapshot, existing))
                .unwrap_or(true);
            if should_apply {
                let (availability, remaining_seconds) = snapshot.availability_at(now_utc);
                let still_cooling =
                    availability == KeyAvailability::Cooldown && remaining_seconds.unwrap_or(0) > 0;
                if !still_cooling {
                    state.cooldowns.remove(&key_id);
                }
                state.set_health_entry_with_snapshot(logical_name, key_id, incoming, snapshot);
            }
        }
    }

    fn key_health_reload_due(&self, now: Instant) -> bool {
        if self.vault_db_path.is_none() {
            return false;
        }
        let reload = self
            .provider_health_reload
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reload
            .last_attempt
            .map(|last| now.duration_since(last) >= Self::KEY_HEALTH_RELOAD_TTL)
            .unwrap_or(true)
    }

    pub(in crate::llm::provider_health) async fn refresh_key_health_from_db_if_stale(&self) {
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let now = Instant::now();
        if !self.key_health_reload_due(now) {
            return;
        }
        {
            let mut reload = self
                .provider_health_reload
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if reload
                .last_attempt
                .map(|last| now.duration_since(last) < Self::KEY_HEALTH_RELOAD_TTL)
                .unwrap_or(false)
            {
                return;
            }
            reload.last_attempt = Some(now);
            reload.last_attempt_at = Some(Self::format_now_utc());
        }

        let loaded = tokio::task::spawn_blocking(move || Self::load_key_health_from_db(&db_path))
            .await
            .map_err(|err| format!("key health reload task failed: {err}"))
            .and_then(|inner| inner);
        let completed_at = Instant::now();
        let completed_at_utc = Self::format_now_utc();
        match loaded {
            Ok(health) => {
                self.merge_loaded_key_health(health);
                self.provider_health_reload
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .mark_success(completed_at, completed_at_utc);
            }
            Err(err) => {
                tracing::warn!("[provider] failed to reload vault key health: {err}");
                self.provider_health_reload
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .mark_error(completed_at, completed_at_utc, err);
            }
        }
    }
}

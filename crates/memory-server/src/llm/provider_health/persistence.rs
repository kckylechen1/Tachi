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

impl super::super::LlmClient {
    pub(super) fn initial_key_health_from_db(
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

    pub(super) fn key_health_snapshot_is_newer_or_equal(
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

    pub(super) fn merge_loaded_key_health(
        &self,
        loaded: HashMap<String, HashMap<String, VaultKeyHealth>>,
    ) {
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

    pub(super) fn key_health_reload_due(&self, now: Instant) -> bool {
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

    pub(super) async fn refresh_key_health_from_db_if_stale(&self) {
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

    pub(super) fn now_utc() -> DateTime<Utc> {
        Utc::now()
    }

    pub(super) fn format_now_utc() -> String {
        Self::now_utc().to_rfc3339()
    }

    pub(super) fn parse_timestamp(ts: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|parsed| parsed.with_timezone(&Utc))
    }

    pub(super) fn status_from_key_health_status(raw: &str) -> &'static str {
        match raw {
            HEALTH_COOLDOWN | HEALTH_RATE_LIMITED => HEALTH_RATE_LIMITED,
            HEALTH_AUTH_FAILED => HEALTH_AUTH_FAILED,
            HEALTH_DISABLED => HEALTH_DISABLED,
            HEALTH_EXHAUSTED => HEALTH_EXHAUSTED,
            _ => HEALTH_OK,
        }
    }

    pub(super) fn key_health_blocked_in_state(
        state: &ProviderState,
        logical_name: &str,
        key_id: &str,
        now: DateTime<Utc>,
    ) -> (KeyAvailability, Option<i64>) {
        state
            .health_snapshots
            .get(logical_name)
            .and_then(|members| members.get(key_id))
            .map(|snapshot| snapshot.availability_at(now))
            .unwrap_or((KeyAvailability::Available, None))
    }

    fn persist_key_health(&self, health: &VaultKeyHealth) {
        if provider_key_health_persist_disabled_for_tests() {
            return;
        }
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let mut health = health.clone();
        health.updated_at = Self::format_now_utc();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let logical_name = health.logical_name.clone();
            let key_id = health.key_id.clone();
            let persist_state = Arc::clone(&self.provider_health_persist);
            handle.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    Self::persist_key_health_blocking(db_path, health)
                })
                .await
                .map_err(|err| {
                    format!("persist vault key health for {logical_name}:{key_id}: {err}")
                })
                .and_then(|inner| inner);
                Self::record_key_health_persist_result(&persist_state, result);
            });
        } else {
            let result = Self::persist_key_health_blocking(db_path, health);
            Self::record_key_health_persist_result(&self.provider_health_persist, result);
        }
    }

    fn persist_key_health_now(&self, health: &VaultKeyHealth) {
        if provider_key_health_persist_disabled_for_tests() {
            return;
        }
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let mut health = health.clone();
        health.updated_at = Self::format_now_utc();
        let result = Self::persist_key_health_blocking(db_path, health);
        Self::record_key_health_persist_result(&self.provider_health_persist, result);
    }

    fn persist_key_health_blocking(db_path: PathBuf, health: VaultKeyHealth) -> Result<(), String> {
        let target = format!("{}:{}", health.logical_name, health.key_id);
        let Some(db_path) = db_path.to_str() else {
            return Err(format!(
                "persist vault key health for {target}: invalid db path"
            ));
        };
        match memory_core::MemoryStore::open(db_path) {
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
    }

    fn read_key_health_entry(&self, logical_name: &str, key_id: &str) -> Option<VaultKeyHealth> {
        self.provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .health
            .get(logical_name)
            .and_then(|members| members.get(key_id))
            .cloned()
    }

    fn with_key_health(
        &self,
        logical_name: &str,
        key_id: &str,
        mutator: impl FnOnce(&mut VaultKeyHealth),
    ) {
        let now = Self::now_utc();
        let now_utc = now.to_rfc3339();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let persisted = {
            let health = state.get_or_insert_health(logical_name, key_id);
            mutator(health);
            health.last_attempt = Some(now_utc.clone());
            health.updated_at = now_utc;
            health.clone()
        };
        state.set_health_snapshot(
            logical_name,
            key_id,
            ProviderHealthSnapshot::from_health_parts(&persisted, Some(now), None),
        );
        drop(state);
        self.persist_key_health(&persisted);
    }

    pub(in crate::llm) fn mark_secret_auth_failed(
        &self,
        selected: &SelectedProviderSecret,
        reason: Option<&str>,
    ) {
        self.with_key_health(&selected.logical_name, &selected.key_id, |health| {
            health.status = HEALTH_AUTH_FAILED.to_string();
            health.auth_failed = true;
            health.disabled = false;
            health.cooldown_until = None;
            health.last_error = reason.map(|value| value.to_string());
            health.error_count += 1;
        });
    }

    pub(in crate::llm) fn mark_secret_success(&self, selected: &SelectedProviderSecret) {
        let now = Self::now_utc();
        let now_utc = now.to_rfc3339();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let persisted = {
            let health = state.get_or_insert_health(&selected.logical_name, &selected.key_id);
            health.status = HEALTH_OK.to_string();
            health.auth_failed = false;
            health.last_success = Some(now_utc.clone());
            health.last_attempt = Some(now_utc.clone());
            health.last_error = None;
            health.error_count = 0;
            health.cooldown_until = None;
            health.updated_at = now_utc;
            health.clone()
        };
        state.cooldowns.remove(&selected.key_id);
        state.set_health_snapshot(
            &selected.logical_name,
            &selected.key_id,
            ProviderHealthSnapshot::from_health_parts(&persisted, Some(now), None),
        );
        drop(state);
        self.persist_key_health(&persisted);
    }

    pub(in crate::llm) fn mark_secret_rate_limited(
        &self,
        selected: &SelectedProviderSecret,
        retry_after: Option<u64>,
    ) {
        let now = Self::now_utc();
        let cooldown = retry_after.unwrap_or(60).clamp(1, 3600);
        let cooldown_until = now + chrono::Duration::seconds(cooldown as i64);
        let now_utc = now.to_rfc3339();
        let until = Instant::now() + Duration::from_secs(cooldown);
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.cooldowns.insert(selected.key_id.clone(), until);
        let persisted = {
            let health = state.get_or_insert_health(&selected.logical_name, &selected.key_id);
            health.status = HEALTH_RATE_LIMITED.to_string();
            health.cooldown_until = Some(cooldown_until.to_rfc3339());
            health.last_attempt = Some(now_utc.clone());
            health.last_error = Some(format!("rate limited; retry after {cooldown}s"));
            health.error_count += 1;
            health.updated_at = now_utc;
            health.clone()
        };
        state.set_health_snapshot(
            &selected.logical_name,
            &selected.key_id,
            ProviderHealthSnapshot::from_health_parts(&persisted, Some(now), Some(cooldown_until)),
        );
        drop(state);
        tracing::warn!(
            "[provider] key {} for {} is rate-limited; cooling down for {}s",
            selected.key_id,
            selected.logical_name,
            cooldown
        );
        self.persist_key_health(&persisted);
    }

    #[cfg(test)]
    pub(crate) fn mark_provider_key_rate_limited_for_tests(
        &self,
        key_id: &str,
        retry_after: Option<u64>,
    ) {
        let logical_name = crate::provider_config::parse_rotation_member_name(key_id)
            .map(|(prefix, _)| prefix)
            .unwrap_or(key_id);
        let selected = SelectedProviderSecret {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            value: String::new(),
        };
        self.mark_secret_rate_limited(&selected, retry_after);
        if let Some(health) = self.read_key_health_entry(&selected.logical_name, &selected.key_id) {
            self.persist_key_health_now(&health);
        }
    }

    #[cfg(test)]
    pub(crate) fn mark_provider_key_auth_failed_for_tests(&self, logical_name: &str, key_id: &str) {
        let selected = SelectedProviderSecret {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            value: String::new(),
        };
        self.mark_secret_auth_failed(&selected, Some("forced auth failure"));
        if let Some(health) = self.read_key_health_entry(logical_name, key_id) {
            self.persist_key_health_now(&health);
        }
    }

    pub(crate) fn record_provider_key_result(
        &self,
        logical_name: &str,
        key_id: &str,
        status_code: Option<u16>,
        outcome: Option<&str>,
        retry_after: Option<u64>,
        reason: Option<&str>,
    ) -> VaultKeyHealth {
        let selected = SelectedProviderSecret {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            value: String::new(),
        };
        let outcome = outcome.map(|value| value.to_ascii_lowercase());
        if status_code == Some(429)
            || matches!(outcome.as_deref(), Some("rate_limited" | "cooldown"))
        {
            self.mark_secret_rate_limited(&selected, retry_after);
        } else if matches!(status_code, Some(401 | 403))
            || matches!(outcome.as_deref(), Some("auth_failed"))
        {
            self.mark_secret_auth_failed(&selected, reason.or(Some("auth failure")));
        } else if matches!(outcome.as_deref(), Some("exhausted")) {
            self.with_key_health(logical_name, key_id, |health| {
                health.status = "exhausted".to_string();
                health.last_error = reason
                    .map(str::to_string)
                    .or_else(|| Some("key exhausted".to_string()));
                health.error_count += 1;
            });
        } else if status_code.is_some_and(|code| (200..300).contains(&code))
            || matches!(outcome.as_deref(), Some("success" | "ok"))
        {
            self.mark_secret_success(&selected);
        } else {
            self.with_key_health(logical_name, key_id, |health| {
                health.status = "error".to_string();
                health.last_error = reason
                    .map(str::to_string)
                    .or_else(|| status_code.map(|code| format!("provider returned HTTP {code}")));
                health.error_count += 1;
            });
        }
        self.read_key_health_entry(logical_name, key_id)
            .unwrap_or_else(|| VaultKeyHealth {
                logical_name: logical_name.to_string(),
                key_id: key_id.to_string(),
                ..VaultKeyHealth::default()
            })
    }

    pub(crate) fn record_provider_key_result_blocking(
        &self,
        logical_name: &str,
        key_id: &str,
        status_code: Option<u16>,
        outcome: Option<&str>,
        retry_after: Option<u64>,
        reason: Option<&str>,
    ) -> VaultKeyHealth {
        let health = self.record_provider_key_result(
            logical_name,
            key_id,
            status_code,
            outcome,
            retry_after,
            reason,
        );
        self.persist_key_health_now(&health);
        health
    }

    #[cfg(test)]
    pub(crate) fn force_provider_health_reload_due_for_tests(&self) {
        let mut reload = self
            .provider_health_reload
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reload.last_attempt =
            Instant::now().checked_sub(Self::KEY_HEALTH_RELOAD_TTL + Duration::from_secs(1));
    }
}

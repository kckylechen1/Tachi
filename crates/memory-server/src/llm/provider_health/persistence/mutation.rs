use super::*;

impl super::super::super::LlmClient {
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

use super::*;

impl super::super::LlmClient {
    #[cfg(test)]
    pub fn set_provider_secret(&self, name: &str, value: &str) -> bool {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            return false;
        }

        self.set_provider_secret_pool(
            name,
            vec![ProviderSecret {
                key_id: name.to_string(),
                value: value.to_string(),
            }],
        )
    }

    pub(crate) fn set_provider_secret_pool(
        &self,
        name: &str,
        entries: Vec<ProviderSecret>,
    ) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let entries: Vec<ProviderSecret> = entries
            .into_iter()
            .filter(|entry| !entry.key_id.trim().is_empty() && !entry.value.trim().is_empty())
            .collect();
        if entries.is_empty() {
            return false;
        }

        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.secrets.insert(name.to_string(), entries);
        state.indices.remove(name);
        true
    }

    pub(crate) fn set_provider_secret_pools<I>(&self, pools: I) -> usize
    where
        I: IntoIterator<Item = (String, Vec<ProviderSecret>)>,
    {
        pools
            .into_iter()
            .filter(|(name, entries)| self.set_provider_secret_pool(name, entries.clone()))
            .count()
    }

    pub fn clear_provider_secrets(&self) {
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.secrets.clear();
        state.cooldowns.clear();
        state.indices.clear();
    }

    pub fn provider_secret_count(&self) -> usize {
        self.provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .secrets
            .len()
    }

    pub(crate) fn provider_pool_statuses(&self) -> Vec<ProviderPoolStatus> {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let state = self
            .provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut statuses = state
            .secrets
            .iter()
            .map(|(logical_name, entries)| {
                let mut unavailable_keys = Vec::new();
                for entry in entries.iter() {
                    let (availability, remaining_seconds) = Self::key_health_blocked_in_state(
                        &state,
                        logical_name,
                        &entry.key_id,
                        now_utc,
                    );
                    let memory_blocked = state
                        .cooldowns
                        .get(&entry.key_id)
                        .is_some_and(|until| *until > now);

                    let is_blocked = memory_blocked
                        || matches!(
                            availability,
                            KeyAvailability::AuthFailed
                                | KeyAvailability::Disabled
                                | KeyAvailability::Exhausted
                        )
                        || matches!(availability, KeyAvailability::Cooldown)
                            && remaining_seconds.unwrap_or(0) > 0;

                    if is_blocked {
                        let remaining_seconds = if memory_blocked {
                            state
                                .cooldowns
                                .get(&entry.key_id)
                                .map(|until| {
                                    until.saturating_duration_since(now).as_secs().max(1) as i64
                                })
                                .unwrap_or(0)
                        } else {
                            remaining_seconds.unwrap_or(0)
                        };
                        let remaining_seconds = if remaining_seconds < 0 {
                            0
                        } else {
                            remaining_seconds as u64
                        };

                        unavailable_keys.push(ProviderKeyCooldownStatus {
                            key_id: entry.key_id.clone(),
                            remaining_seconds,
                        });
                    }
                }

                unavailable_keys.sort_by(|a, b| a.key_id.cmp(&b.key_id));
                ProviderPoolStatus {
                    logical_name: logical_name.to_string(),
                    total_keys: entries.len(),
                    available_keys: entries.len().saturating_sub(unavailable_keys.len()),
                    rate_limited_keys: unavailable_keys,
                    current_index: *state.indices.get(logical_name).unwrap_or(&0),
                    strategy: "round_robin_skip_cooldown",
                }
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
        statuses
    }

    pub(crate) fn provider_health_status(&self) -> ProviderHealthStatus {
        let reload = self
            .provider_health_reload
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let persist = self
            .provider_health_persist
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ProviderHealthStatus {
            source_of_truth: reload.source_of_truth,
            reload_ttl_secs: Self::KEY_HEALTH_RELOAD_TTL.as_secs(),
            last_attempt_at: reload.last_attempt_at.clone(),
            last_success_at: reload.last_success_at.clone(),
            last_success_age_secs: reload
                .last_success
                .map(|instant| instant.elapsed().as_secs()),
            last_error: reload.last_error.clone(),
            persist_last_attempt_at: persist.last_attempt_at.clone(),
            persist_last_success_at: persist.last_success_at.clone(),
            persist_last_success_age_secs: persist
                .last_success
                .map(|instant| instant.elapsed().as_secs()),
            persist_last_error: persist.last_error.clone(),
        }
    }

    /// Return the current in-memory provider key health map.
    ///
    /// This is used when loading API key pools so that tests (which may disable
    /// background persistence) still see health mutations made in the same
    /// process without requiring a DB round-trip.
    pub(crate) fn provider_health_memory_snapshot(
        &self,
    ) -> HashMap<String, HashMap<String, VaultKeyHealth>> {
        let state = self
            .provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.health.clone()
    }
}

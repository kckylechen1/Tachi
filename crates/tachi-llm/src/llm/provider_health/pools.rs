use super::*;
use std::collections::HashSet;

impl super::super::LlmClient {
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
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

    pub fn set_provider_secret_pool(&self, name: &str, entries: Vec<ProviderSecret>) -> bool {
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

    pub fn set_provider_secret_pools<I>(&self, pools: I) -> usize
    where
        I: IntoIterator<Item = (String, Vec<ProviderSecret>)>,
    {
        pools
            .into_iter()
            .filter(|(name, entries)| self.set_provider_secret_pool(name, entries.clone()))
            .count()
    }

    /// Clone one logical provider pool under a read lock. Missing-alias recovery
    /// calls this only for the affected key, avoiding a second full secret map.
    pub(crate) fn provider_secret_pool_snapshot(
        &self,
        logical_name: &str,
    ) -> Option<Vec<ProviderSecret>> {
        self.provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .secrets
            .get(logical_name)
            .cloned()
    }

    /// Replace the complete provider-secret cache after validating every pool.
    ///
    /// Validation happens before the write lock is acquired. Under one write
    /// lock, the final map replaces the old map while indices and member
    /// cooldowns and composite logical/member health survive only for pools
    /// explicitly marked retained. New, changed, and removed pools therefore
    /// cannot inherit stale runtime state, and a refused refresh cannot expose
    /// a partial cache.
    pub(crate) fn replace_provider_secret_pools(
        &self,
        pools: HashMap<String, Vec<ProviderSecret>>,
        retained_logical_names: &HashSet<String>,
    ) -> Result<usize, String> {
        let mut replacement = HashMap::with_capacity(pools.len());
        for (name, entries) in pools {
            let name = name.trim();
            if name.is_empty() {
                return Err(
                    "Provider refresh refused because a provider key name is empty; prior provider cache left unchanged"
                        .to_string(),
                );
            }
            if entries.is_empty()
                || entries
                    .iter()
                    .any(|entry| entry.key_id.trim().is_empty() || entry.value.trim().is_empty())
            {
                return Err(format!(
                    "Provider refresh refused because key '{name}' has an invalid secret pool; prior provider cache left unchanged"
                ));
            }
            replacement.insert(name.to_string(), entries);
        }

        let loaded = replacement.len();
        let mut retained_members_by_logical = HashMap::new();
        for logical_name in retained_logical_names {
            let Some(entries) = replacement.get(logical_name) else {
                continue;
            };
            let member_ids = entries
                .iter()
                .map(|entry| entry.key_id.clone())
                .collect::<HashSet<_>>();
            retained_members_by_logical.insert(logical_name.clone(), member_ids);
        }
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.indices.retain(|logical_name, _index| {
            retained_logical_names.contains(logical_name) && replacement.contains_key(logical_name)
        });
        state.cooldowns.retain(|logical_name, members| {
            let Some(retained_members) = retained_members_by_logical.get(logical_name) else {
                return false;
            };
            members.retain(|key_id, _until| retained_members.contains(key_id));
            !members.is_empty()
        });
        state.health.retain(|logical_name, members| {
            let Some(retained_members) = retained_members_by_logical.get(logical_name) else {
                return false;
            };
            members.retain(|key_id, _health| retained_members.contains(key_id));
            !members.is_empty()
        });
        state.health_snapshots.retain(|logical_name, members| {
            let Some(retained_members) = retained_members_by_logical.get(logical_name) else {
                return false;
            };
            members.retain(|key_id, _snapshot| retained_members.contains(key_id));
            !members.is_empty()
        });
        state.secrets = replacement;
        Ok(loaded)
    }

    pub fn clear_provider_secrets(&self) -> Result<(), String> {
        self.clear_provider_secrets_inner(|| Ok(()), None)
    }

    /// Make a durable-custody state transition and clear the provider cache
    /// under the same transaction boundary used by materialization.
    ///
    /// The closure runs only after the transaction lock is acquired and must
    /// make the durable source unavailable without re-entering provider
    /// materialization or cache-clear APIs. This ordering lets an explicit
    /// Vault lock linearize before both custody state and cache are cleared.
    pub fn clear_provider_secrets_with_custody<F>(&self, clear_custody: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        self.clear_provider_secrets_inner(clear_custody, None)
    }

    fn clear_provider_secrets_inner<F>(
        &self,
        clear_custody: F,
        before_materialization_guard: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        // An explicit Vault lock must order after any materialization that
        // already captured last-known-good state. Use the same transaction
        // boundary as materialization, then clear under the normal short-lived
        // provider-state write lock. No materialization path calls this method,
        // so the non-reentrant mutex cannot be acquired recursively.
        if let Some(hook) = before_materialization_guard {
            hook();
        }
        let _materialization_guard = self.provider_materialization_guard()?;
        clear_custody()?;
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.secrets.clear();
        state.cooldowns.clear();
        state.indices.clear();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn clear_provider_secrets_with_hook_for_tests(
        &self,
        before_materialization_guard: impl FnOnce() + Send + 'static,
    ) -> Result<(), String> {
        self.clear_provider_secrets_inner(|| Ok(()), Some(Box::new(before_materialization_guard)))
    }

    pub fn provider_secret_count(&self) -> usize {
        self.provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .secrets
            .len()
    }

    #[cfg(test)]
    pub(crate) fn seed_provider_operational_state_for_tests(
        &self,
        logical_name: &str,
        index: usize,
        cooldown_key_id: &str,
    ) {
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.indices.insert(logical_name.to_string(), index);
        state.set_cooldown(
            logical_name,
            cooldown_key_id,
            Instant::now() + Duration::from_secs(60),
        );
    }

    #[cfg(test)]
    pub(crate) fn provider_health_state_presence_for_tests(
        &self,
        logical_name: &str,
        key_id: &str,
    ) -> (bool, bool) {
        let state = self
            .provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let has_health = state
            .health
            .get(logical_name)
            .is_some_and(|members| members.contains_key(key_id));
        let has_snapshot = state
            .health_snapshots
            .get(logical_name)
            .is_some_and(|members| members.contains_key(key_id));
        (has_health, has_snapshot)
    }

    pub fn provider_pool_statuses(&self) -> Vec<ProviderPoolStatus> {
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
                        .cooldown_until(logical_name, &entry.key_id)
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
                                .cooldown_until(logical_name, &entry.key_id)
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

    pub fn provider_health_status(&self) -> ProviderHealthStatus {
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
            lane_outages: self.lane_outage_statuses(),
        }
    }

    /// Per-lane breaker + fallback-chain-outage snapshot (#1197). A lane
    /// whose provider is degraded shows up here with `breaker_state ==
    /// "open"` and/or a nonzero `consecutive_chain_failures` instead of
    /// only manifesting as a silent stall to whatever's calling the lane.
    pub fn lane_outage_statuses(&self) -> Vec<LaneOutageStatus> {
        [
            ChatLane::Extract,
            ChatLane::Distill,
            ChatLane::Reasoning,
            ChatLane::Summary,
        ]
        .into_iter()
        .map(|lane| {
            let breaker_key = format!("chat:{}", lane.as_str());
            let breaker_state = self.circuit_breakers.state_name(&breaker_key);
            let fallback_configured = self.fallback_lane(lane).is_some();
            let (consecutive_chain_failures, last_outage_at, last_error) =
                self.lane_outage.snapshot_for(lane.as_str());
            LaneOutageStatus {
                lane: lane.as_str().to_string(),
                breaker_state,
                fallback_configured,
                consecutive_chain_failures,
                last_outage_at,
                last_error,
            }
        })
        .collect()
    }

    /// Return the current in-memory provider key health map.
    ///
    /// This is used when loading API key pools so that tests (which may disable
    /// background persistence) still see health mutations made in the same
    /// process without requiring a DB round-trip.
    pub fn provider_health_memory_snapshot(
        &self,
    ) -> HashMap<String, HashMap<String, VaultKeyHealth>> {
        let state = self
            .provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.health.clone()
    }
}

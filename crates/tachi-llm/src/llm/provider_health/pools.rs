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

    /// Validate every provider pool before any state lock is acquired. The
    /// materialization snapshot owns this normalized map until its companion
    /// lane overlay and catalog projection are ready to publish.
    pub(crate) fn prepare_provider_secret_pools(
        &self,
        pools: HashMap<String, Vec<ProviderSecret>>,
    ) -> Result<HashMap<String, Vec<ProviderSecret>>, String> {
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
        Ok(replacement)
    }

    /// Publish the already-validated provider pools and, when supplied, the
    /// complete lane overlay under one provider-state write lock. A caller
    /// that supplies `Some` gets one linearization point for both surfaces;
    /// `None` preserves the existing overlay for legacy pool-only callers.
    pub(crate) fn publish_provider_secret_pools<P>(
        &self,
        replacement: HashMap<String, Vec<ProviderSecret>>,
        retained_logical_names: &HashSet<String>,
        lane_config_overlay: Option<LaneConfigOverlay>,
        commit_companion_projection: P,
    ) -> Result<usize, String>
    where
        P: FnOnce() -> Result<(), String>,
    {
        self.publish_provider_secret_pools_with_health_baseline(
            replacement,
            retained_logical_names,
            self.provider_health_memory_snapshot(),
            lane_config_overlay,
            commit_companion_projection,
        )
    }

    pub(crate) fn publish_provider_secret_pools_with_health_baseline<P>(
        &self,
        replacement: HashMap<String, Vec<ProviderSecret>>,
        retained_logical_names: &HashSet<String>,
        health_baseline: HashMap<String, HashMap<String, VaultKeyHealth>>,
        lane_config_overlay: Option<LaneConfigOverlay>,
        commit_companion_projection: P,
    ) -> Result<usize, String>
    where
        P: FnOnce() -> Result<(), String>,
    {
        self.publish_provider_secret_pools_inner(
            replacement,
            retained_logical_names,
            health_baseline,
            lane_config_overlay,
            None,
            commit_companion_projection,
        )
    }

    fn publish_provider_secret_pools_inner<P>(
        &self,
        replacement: HashMap<String, Vec<ProviderSecret>>,
        retained_logical_names: &HashSet<String>,
        health_baseline: HashMap<String, HashMap<String, VaultKeyHealth>>,
        lane_config_overlay: Option<LaneConfigOverlay>,
        before_companion_commit: Option<Box<dyn FnOnce() + Send>>,
        commit_companion_projection: P,
    ) -> Result<usize, String>
    where
        P: FnOnce() -> Result<(), String>,
    {
        let loaded = replacement.len();
        let replacement_logical_names = replacement.keys().cloned().collect::<HashSet<_>>();
        let replacement_member_ids = replacement
            .iter()
            .map(|(logical_name, entries)| {
                (
                    logical_name.clone(),
                    entries
                        .iter()
                        .map(|entry| entry.key_id.clone())
                        .collect::<HashSet<_>>(),
                )
            })
            .collect::<HashMap<_, _>>();
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
        // Stage the complete next state while readers are blocked, then commit
        // the companion transaction. Vault-backed callers keep their source
        // mutation fence inside that commit callback, so the in-memory
        // linearization happens before the fence releases a waiting revocation.
        // On failure, restore the complete prior state before readers resume.
        let previous = std::mem::take(&mut *state);
        let mut next = ProviderState {
            secrets: replacement,
            lane_config_overlay: lane_config_overlay
                .unwrap_or_else(|| previous.lane_config_overlay.clone()),
            cooldowns: previous.cooldowns.clone(),
            indices: previous.indices.clone(),
            health: previous.health.clone(),
            health_snapshots: previous.health_snapshots.clone(),
        };
        next.indices.retain(|logical_name, _index| {
            retained_logical_names.contains(logical_name)
                && replacement_logical_names.contains(logical_name)
        });
        next.cooldowns.retain(|logical_name, members| {
            let Some(retained_members) = retained_members_by_logical.get(logical_name) else {
                return false;
            };
            members.retain(|key_id, _until| retained_members.contains(key_id));
            !members.is_empty()
        });
        next.health.retain(|logical_name, members| {
            let Some(retained_members) = retained_members_by_logical.get(logical_name) else {
                return false;
            };
            members.retain(|key_id, _health| retained_members.contains(key_id));
            !members.is_empty()
        });
        next.health_snapshots.retain(|logical_name, members| {
            let Some(retained_members) = retained_members_by_logical.get(logical_name) else {
                return false;
            };
            members.retain(|key_id, _snapshot| retained_members.contains(key_id));
            !members.is_empty()
        });
        preserve_newer_health_observations(
            &mut next,
            &previous,
            &replacement_member_ids,
            &health_baseline,
            retained_logical_names,
        );
        *state = next;
        if let Some(hook) = before_companion_commit {
            hook();
        }
        if let Err(error) = commit_companion_projection() {
            *state = previous;
            return Err(error);
        }
        Ok(loaded)
    }

    #[cfg(test)]
    pub(crate) fn publish_provider_secret_pools_with_hook_for_tests<P>(
        &self,
        replacement: HashMap<String, Vec<ProviderSecret>>,
        retained_logical_names: &HashSet<String>,
        lane_config_overlay: Option<LaneConfigOverlay>,
        before_companion_commit: impl FnOnce() + Send + 'static,
        commit_companion_projection: P,
    ) -> Result<usize, String>
    where
        P: FnOnce() -> Result<(), String>,
    {
        self.publish_provider_secret_pools_inner(
            replacement,
            retained_logical_names,
            self.provider_health_memory_snapshot(),
            lane_config_overlay,
            Some(Box::new(before_companion_commit)),
            commit_companion_projection,
        )
    }

    pub fn clear_provider_secrets(&self) -> Result<(), String> {
        self.clear_provider_secrets_inner(|| Ok(()), false, None)
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
        self.clear_provider_secrets_inner(clear_custody, true, None)
    }

    fn clear_provider_secrets_inner<F>(
        &self,
        clear_custody: F,
        clear_lane_config_overlay: bool,
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
        if clear_lane_config_overlay {
            state.lane_config_overlay = LaneConfigOverlay::default();
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn clear_provider_secrets_with_hook_for_tests(
        &self,
        before_materialization_guard: impl FnOnce() + Send + 'static,
    ) -> Result<(), String> {
        self.clear_provider_secrets_inner(
            || Ok(()),
            false,
            Some(Box::new(before_materialization_guard)),
        )
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

/// Preserve a health outcome that arrived after the durable pool snapshot but
/// before publication acquired the provider-state write lock. Replaced pools
/// intentionally discard their old health, including when a member id is
/// reused; only an observation newer than the materialization-start baseline
/// crosses that boundary. Retained last-known-good pools remain governed by
/// `retained_logical_names` and keep their complete operational state.
fn preserve_newer_health_observations(
    next: &mut ProviderState,
    previous: &ProviderState,
    replacement_member_ids: &HashMap<String, HashSet<String>>,
    health_baseline: &HashMap<String, HashMap<String, VaultKeyHealth>>,
    retained_logical_names: &HashSet<String>,
) {
    for (logical_name, member_ids) in replacement_member_ids {
        if retained_logical_names.contains(logical_name) {
            continue;
        }
        for key_id in member_ids {
            let Some(current) = previous
                .health
                .get(logical_name)
                .and_then(|members| members.get(key_id))
            else {
                continue;
            };
            let baseline = health_baseline
                .get(logical_name)
                .and_then(|members| members.get(key_id));
            if !health_observation_is_newer(current, baseline) {
                continue;
            }

            next.health
                .entry(logical_name.clone())
                .or_default()
                .insert(key_id.clone(), current.clone());
            let snapshot = previous
                .health_snapshots
                .get(logical_name)
                .and_then(|members| members.get(key_id))
                .cloned()
                .unwrap_or_else(|| ProviderHealthSnapshot::from_health(current));
            next.health_snapshots
                .entry(logical_name.clone())
                .or_default()
                .insert(key_id.clone(), snapshot);
            if let Some(cooldown) = previous
                .cooldowns
                .get(logical_name)
                .and_then(|members| members.get(key_id))
            {
                next.cooldowns
                    .entry(logical_name.clone())
                    .or_default()
                    .insert(key_id.clone(), *cooldown);
            }
        }
    }
}

fn health_observation_is_newer(
    current: &VaultKeyHealth,
    baseline: Option<&VaultKeyHealth>,
) -> bool {
    let Some(baseline) = baseline else {
        return true;
    };
    if health_rows_equal(current, baseline) {
        return false;
    }
    match (
        chrono::DateTime::parse_from_rfc3339(&current.updated_at).ok(),
        chrono::DateTime::parse_from_rfc3339(&baseline.updated_at).ok(),
    ) {
        (Some(current_at), Some(baseline_at)) => current_at >= baseline_at,
        (Some(_), None) | (None, None) => true,
        (None, Some(_)) => false,
    }
}

fn health_rows_equal(left: &VaultKeyHealth, right: &VaultKeyHealth) -> bool {
    left.logical_name == right.logical_name
        && left.key_id == right.key_id
        && left.status == right.status
        && left.cooldown_until == right.cooldown_until
        && left.last_success == right.last_success
        && left.last_attempt == right.last_attempt
        && left.last_error == right.last_error
        && left.error_count == right.error_count
        && left.auth_failed == right.auth_failed
        && left.disabled == right.disabled
        && left.metadata == right.metadata
        && left.updated_at == right.updated_at
}

use super::*;

impl super::super::LlmClient {
    fn select_secret(&self, keys: &[&str]) -> Option<SelectedProviderSecret> {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune_expired_cooldowns(now);

        let vault_value = keys.iter().find_map(|key| {
            let entries = state.secrets.get(*key)?;
            if entries.is_empty() {
                return None;
            }
            let start = *state.indices.get(*key).unwrap_or(&0);
            let mut selected: Option<(usize, usize, ProviderSecret)> = None;
            for offset in 0..entries.len() {
                let idx = (start + offset) % entries.len();
                let entry = &entries[idx];
                if state.is_cooling_down(key, &entry.key_id) {
                    continue;
                }

                let (availability, remaining_seconds) =
                    Self::key_health_blocked_in_state(&state, key, &entry.key_id, now_utc);
                let unusable = match availability {
                    KeyAvailability::AuthFailed
                    | KeyAvailability::Disabled
                    | KeyAvailability::Exhausted => true,
                    KeyAvailability::Cooldown => remaining_seconds.unwrap_or(0) > 0,
                    KeyAvailability::Available => false,
                };
                if !unusable {
                    selected = Some((idx, entries.len(), entry.clone()));
                    break;
                }
            }

            selected
                .map(|(idx, entries_len, entry)| {
                    state
                        .indices
                        .insert((*key).to_string(), (idx + 1) % entries_len);
                    SelectedProviderSecret {
                        logical_name: (*key).to_string(),
                        key_id: entry.key_id,
                        value: entry.value.trim().to_string(),
                    }
                })
                .filter(|entry| !entry.value.is_empty())
        });

        vault_value.or_else(|| {
            keys.iter().find_map(|key| {
                let (availability, remaining_seconds) =
                    Self::key_health_blocked_in_state(&state, key, key, now_utc);
                let unusable = match availability {
                    KeyAvailability::AuthFailed
                    | KeyAvailability::Disabled
                    | KeyAvailability::Exhausted => true,
                    KeyAvailability::Cooldown => remaining_seconds.unwrap_or(0) > 0,
                    KeyAvailability::Available => false,
                };

                if unusable || state.is_cooling_down(key, key) {
                    return None;
                }
                Self::first_env(&[*key])
                    .filter(|value| !crate::provider_names::is_vault_alias(value))
                    .map(|value| SelectedProviderSecret {
                        logical_name: (*key).to_string(),
                        key_id: (*key).to_string(),
                        value,
                    })
            })
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    fn first_secret(&self, keys: &[&str]) -> Option<String> {
        self.select_secret(keys).map(|selected| selected.value)
    }

    #[cfg(test)]
    pub(in crate::llm) fn required_secret(&self, keys: &[&str]) -> Result<String, String> {
        self.first_secret(keys)
            .ok_or_else(|| self.provider_secret_unavailable_error(keys))
    }

    fn required_selected_secret(&self, keys: &[&str]) -> Result<SelectedProviderSecret, String> {
        self.select_secret(keys)
            .ok_or_else(|| self.provider_secret_unavailable_error(keys))
    }

    /// Select the same currently usable credential as the normal provider
    /// path without advancing its round-robin cursor, pruning state, or
    /// persisting health. This is reserved for the observation-only auth
    /// clearance probe.
    pub(in crate::llm) fn selected_secret_readonly(
        &self,
        keys: &[&str],
    ) -> Option<SelectedProviderSecret> {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let state = self
            .provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let vault_value = keys.iter().find_map(|key| {
            let entries = state.secrets.get(*key)?;
            if entries.is_empty() {
                return None;
            }
            let start = *state.indices.get(*key).unwrap_or(&0);
            (0..entries.len()).find_map(|offset| {
                let entry = &entries[(start + offset) % entries.len()];
                let in_active_cooldown = state
                    .cooldown_until(key, &entry.key_id)
                    .is_some_and(|until| *until > now);
                let (availability, remaining_seconds) =
                    Self::key_health_blocked_in_state(&state, key, &entry.key_id, now_utc);
                (!entry.value.trim().is_empty()
                    && !in_active_cooldown
                    && !Self::availability_is_unusable(availability, remaining_seconds))
                .then(|| SelectedProviderSecret {
                    logical_name: (*key).to_string(),
                    key_id: entry.key_id.clone(),
                    value: entry.value.trim().to_string(),
                })
            })
        });

        vault_value.or_else(|| {
            keys.iter().find_map(|key| {
                // Env-fallback path: the logical key IS its own member id.
                let in_active_cooldown = state
                    .cooldown_until(key, key)
                    .is_some_and(|until| *until > now);
                let (availability, remaining_seconds) =
                    Self::key_health_blocked_in_state(&state, key, key, now_utc);
                if in_active_cooldown
                    || Self::availability_is_unusable(availability, remaining_seconds)
                {
                    return None;
                }
                Self::first_env(&[*key])
                    .filter(|value| !crate::provider_names::is_vault_alias(value))
                    .map(|value| SelectedProviderSecret {
                        logical_name: (*key).to_string(),
                        key_id: (*key).to_string(),
                        value,
                    })
            })
        })
    }

    pub fn has_configured_secret(&self, keys: &[&str]) -> bool {
        self.select_secret(keys).is_some()
    }

    /// Read-only "is there still a usable key" check — unlike
    /// `has_configured_secret` (which calls the real `select_secret` and, as
    /// a side effect, advances the round-robin selection index even though
    /// the returned secret is discarded), this never mutates the round-robin
    /// `state.indices` (the only mutation is the routine expired-cooldown
    /// prune every selection path already does). Used inside the
    /// attempt-retry loop (#1197 BUG-1) to decide "does the primary pool
    /// have another key to try" without skewing which key actually gets
    /// picked next.
    ///
    /// Mirrors `select_secret`'s real two-pass structure (codex round-2
    /// review: a first version of this probe checked each key's vault pool
    /// *or* its env value, never both — so a key with a bad vault entry but
    /// a good env-var secret was under-reported as unusable, even though
    /// `select_secret` itself would have found it via the env fallback
    /// pass). Pass 1 scans every key's vault pool; only if *none* of them
    /// have a usable entry does pass 2 scan every key's plain env value —
    /// exactly the `vault_value.or_else(...)` shape below.
    pub(in crate::llm) fn has_usable_secret_readonly(&self, keys: &[&str]) -> bool {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune_expired_cooldowns(now);

        let vault_usable = keys.iter().any(|key| {
            state.secrets.get(*key).is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| Self::entry_is_usable(&state, key, entry, now_utc))
            })
        });
        if vault_usable {
            return true;
        }

        keys.iter().any(|key| {
            let has_env_value = Self::first_env(&[*key])
                .filter(|value| !value.trim().is_empty())
                .filter(|value| !crate::provider_names::is_vault_alias(value))
                .is_some();
            if !has_env_value {
                return false;
            }
            if state.is_cooling_down(key, key) {
                return false;
            }
            let (availability, remaining_seconds) =
                Self::key_health_blocked_in_state(&state, key, key, now_utc);
            !Self::availability_is_unusable(availability, remaining_seconds)
        })
    }

    fn entry_is_usable(
        state: &ProviderState,
        logical_key: &str,
        entry: &ProviderSecret,
        now_utc: DateTime<Utc>,
    ) -> bool {
        if entry.value.trim().is_empty() {
            return false;
        }
        if state.is_cooling_down(logical_key, &entry.key_id) {
            return false;
        }
        let (availability, remaining_seconds) =
            Self::key_health_blocked_in_state(state, logical_key, &entry.key_id, now_utc);
        !Self::availability_is_unusable(availability, remaining_seconds)
    }

    fn availability_is_unusable(
        availability: KeyAvailability,
        remaining_seconds: Option<i64>,
    ) -> bool {
        match availability {
            KeyAvailability::AuthFailed
            | KeyAvailability::Disabled
            | KeyAvailability::Exhausted => true,
            KeyAvailability::Cooldown => remaining_seconds.unwrap_or(0) > 0,
            KeyAvailability::Available => false,
        }
    }

    fn provider_secret_unavailable_error(&self, keys: &[&str]) -> String {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune_expired_cooldowns(now);

        let mut configured = 0usize;
        let mut empty = 0usize;
        let mut status_counts: HashMap<&'static str, usize> = HashMap::new();
        let mut retry_after = None;

        for key in keys {
            if let Some(entries) = state.secrets.get(*key) {
                for entry in entries {
                    if entry.value.trim().is_empty() {
                        empty += 1;
                        continue;
                    }
                    configured += 1;
                    if let Some(until) = state
                        .cooldown_until(key, &entry.key_id)
                        .filter(|until| **until > now)
                    {
                        Self::remember_min_retry_delay(
                            &mut retry_after,
                            until.saturating_duration_since(now),
                        );
                        *status_counts.entry("cooldown").or_default() += 1;
                        continue;
                    }
                    let (availability, remaining_seconds) =
                        Self::key_health_blocked_in_state(&state, key, &entry.key_id, now_utc);
                    if availability == KeyAvailability::Cooldown {
                        let delay =
                            Duration::from_secs(remaining_seconds.unwrap_or(1).max(1) as u64);
                        Self::remember_min_retry_delay(&mut retry_after, delay);
                    }
                    *status_counts.entry(availability.label()).or_default() += 1;
                }
            }

            if let Some(value) = Self::first_env(&[*key])
                .filter(|value| !value.trim().is_empty())
                .filter(|value| !crate::provider_names::is_vault_alias(value))
            {
                if value.trim().is_empty() {
                    empty += 1;
                    continue;
                }
                configured += 1;
                if let Some(until) = state.cooldown_until(key, key).filter(|until| **until > now) {
                    Self::remember_min_retry_delay(
                        &mut retry_after,
                        until.saturating_duration_since(now),
                    );
                    *status_counts.entry("cooldown").or_default() += 1;
                    continue;
                }
                let (availability, remaining_seconds) =
                    Self::key_health_blocked_in_state(&state, key, key, now_utc);
                if availability == KeyAvailability::Cooldown {
                    let delay = Duration::from_secs(remaining_seconds.unwrap_or(1).max(1) as u64);
                    Self::remember_min_retry_delay(&mut retry_after, delay);
                }
                *status_counts.entry(availability.label()).or_default() += 1;
            }
        }

        if configured == 0 {
            return "Missing API key. Add one to Tachi Vault or set the appropriate env var."
                .to_string();
        }

        let names = keys.join(", ");
        if let Some(delay) = retry_after {
            return format!(
                "API key unavailable for [{names}]: all configured provider keys are temporarily unavailable; retry after about {}s",
                delay.as_secs().max(1)
            );
        }

        let mut reasons = status_counts
            .into_iter()
            .filter(|(status, _)| *status != "available")
            .map(|(status, count)| format!("{status}: {count}"))
            .collect::<Vec<_>>();
        if empty > 0 {
            reasons.push(format!("empty: {empty}"));
        }
        reasons.sort();
        let reason = if reasons.is_empty() {
            "no usable configured key was selected".to_string()
        } else {
            reasons.join(", ")
        };
        format!("API key unavailable for [{names}]: all configured provider keys are unusable ({reason})")
    }

    fn remember_min_retry_delay(slot: &mut Option<Duration>, delay: Duration) {
        if delay.is_zero() {
            return;
        }
        match slot {
            Some(existing) if *existing <= delay => {}
            _ => *slot = Some(delay),
        }
    }

    fn key_retry_status(
        &self,
        logical_name: &str,
        key_id: &str,
        now: Instant,
        now_utc: DateTime<Utc>,
        state: &ProviderState,
    ) -> KeyRetryStatus {
        let mut retry_after = None;
        if let Some(until) = state
            .cooldown_until(logical_name, key_id)
            .filter(|until| **until > now)
        {
            Self::remember_min_retry_delay(&mut retry_after, until.saturating_duration_since(now));
        }

        let (availability, remaining_seconds) =
            Self::key_health_blocked_in_state(state, logical_name, key_id, now_utc);
        match availability {
            KeyAvailability::Available => match retry_after {
                Some(delay) => KeyRetryStatus::RetryAfter(delay),
                None => KeyRetryStatus::Available,
            },
            KeyAvailability::Cooldown => {
                let remaining_seconds = remaining_seconds.unwrap_or(1).max(1) as u64;
                Self::remember_min_retry_delay(
                    &mut retry_after,
                    Duration::from_secs(remaining_seconds),
                );
                retry_after
                    .map(KeyRetryStatus::RetryAfter)
                    .unwrap_or(KeyRetryStatus::Unavailable)
            }
            KeyAvailability::AuthFailed
            | KeyAvailability::Disabled
            | KeyAvailability::Exhausted => KeyRetryStatus::Unavailable,
        }
    }

    pub(in crate::llm) fn selected_secret_retry_delay(&self, keys: &[&str]) -> Option<Duration> {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune_expired_cooldowns(now);

        let mut saw_configured_key = false;
        let mut retry_after = None;

        for key in keys {
            if let Some(entries) = state.secrets.get(*key) {
                for entry in entries
                    .iter()
                    .filter(|entry| !entry.value.trim().is_empty())
                {
                    saw_configured_key = true;
                    match self.key_retry_status(key, &entry.key_id, now, now_utc, &state) {
                        KeyRetryStatus::Available => return None,
                        KeyRetryStatus::RetryAfter(delay) => {
                            Self::remember_min_retry_delay(&mut retry_after, delay);
                        }
                        KeyRetryStatus::Unavailable => {}
                    }
                }
            }

            if Self::first_env(&[*key])
                .filter(|value| !value.trim().is_empty())
                .is_some_and(|value| !crate::provider_names::is_vault_alias(&value))
            {
                saw_configured_key = true;
                match self.key_retry_status(key, key, now, now_utc, &state) {
                    KeyRetryStatus::Available => return None,
                    KeyRetryStatus::RetryAfter(delay) => {
                        Self::remember_min_retry_delay(&mut retry_after, delay);
                    }
                    KeyRetryStatus::Unavailable => {}
                }
            }
        }

        if saw_configured_key {
            retry_after
        } else {
            None
        }
    }

    pub(in crate::llm) async fn required_selected_secret_or_wait(
        &self,
        keys: &[&str],
        attempt: usize,
        context: &str,
    ) -> Result<Option<SelectedProviderSecret>, String> {
        self.refresh_key_health_from_db_if_stale().await;
        match self.required_selected_secret(keys) {
            Ok(selected) => Ok(Some(selected)),
            Err(err) => {
                if attempt < Self::MAX_ATTEMPTS {
                    if let Some(retry_after) = self.selected_secret_retry_delay(keys) {
                        let wait = retry_after.min(Self::retry_delay(attempt));
                        tracing::warn!(
                            "[provider] {context} keys are temporarily unavailable; retrying selection in {}ms",
                            wait.as_millis()
                        );
                        tokio::time::sleep(wait).await;
                        return Ok(None);
                    }
                }
                Err(err)
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn provider_key_id_for_tests(&self, keys: &[&str]) -> Option<String> {
        self.select_secret(keys).map(|selected| selected.key_id)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn provider_secret_for_tests(&self, keys: &[&str]) -> Option<String> {
        self.first_secret(keys)
    }
}

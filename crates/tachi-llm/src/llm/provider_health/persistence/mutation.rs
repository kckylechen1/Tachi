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

    /// The one place this client turns an outcome into a `vault_key_health`
    /// row (#1680 D6). The row transition itself belongs to
    /// [`memcore::vault::health::record_key_outcome`] — the single writer all
    /// channels share; what stays here is the in-process bookkeeping only this
    /// client has: the ephemeral `Instant` cooldown mirror, the availability
    /// snapshot, and the debounced persist.
    ///
    /// `evidence` is never inferred: the invocation path and the caller-facing
    /// `record_provider_key_result` are [`EvidenceKind::SelfReported`] (a
    /// consumer of the key describing its own usage), while the auth probe —
    /// a deliberate request Tachi made and read itself — is
    /// [`EvidenceKind::Probed`].
    pub(in crate::llm) fn apply_key_outcome(
        &self,
        selected: &SelectedProviderSecret,
        outcome: TypedOutcome,
        evidence: EvidenceKind,
        reason: Option<&str>,
    ) -> VaultKeyHealth {
        let now = Self::now_utc();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let existing = state
            .health
            .get(&selected.logical_name)
            .and_then(|members| members.get(&selected.key_id))
            .cloned();
        let write = memcore::vault::health::record_key_outcome(
            existing.as_ref(),
            &selected.logical_name,
            &selected.key_id,
            outcome,
            evidence,
            reason,
            now,
        );

        if let Some(cooldown_until) = write.cooldown_until {
            let seconds = (cooldown_until - now).num_seconds().max(0) as u64;
            state.set_cooldown(
                &selected.logical_name,
                &selected.key_id,
                Instant::now() + Duration::from_secs(seconds),
            );
        } else if write.clear_cooldown {
            state.remove_cooldown(&selected.logical_name, &selected.key_id);
        }

        let persisted = write.health.clone();
        // A non-destructive outcome must not drop a cooldown the row still
        // carries: when this write did not set one, the snapshot keeps
        // reading it off the row rather than asserting "no cooldown".
        let snapshot_cooldown = write.cooldown_until.or_else(|| {
            persisted
                .cooldown_until
                .as_deref()
                .and_then(Self::parse_timestamp)
        });
        state.set_health_entry_with_snapshot(
            selected.logical_name.clone(),
            selected.key_id.clone(),
            write.health,
            ProviderHealthSnapshot::from_health_parts(&persisted, Some(now), snapshot_cooldown),
        );
        drop(state);
        self.persist_key_health(&persisted);
        persisted
    }

    pub(in crate::llm) fn mark_secret_auth_failed(
        &self,
        selected: &SelectedProviderSecret,
        reason: Option<&str>,
    ) {
        self.apply_key_outcome(
            selected,
            TypedOutcome::AuthFailed,
            EvidenceKind::SelfReported,
            reason,
        );
    }

    pub(in crate::llm) fn mark_secret_success(&self, selected: &SelectedProviderSecret) {
        self.apply_key_outcome(
            selected,
            TypedOutcome::Success,
            EvidenceKind::SelfReported,
            None,
        );
    }

    pub(in crate::llm) fn mark_secret_rate_limited(
        &self,
        selected: &SelectedProviderSecret,
        retry_after: Option<u64>,
    ) {
        self.apply_key_outcome(
            selected,
            TypedOutcome::RateLimited {
                retry_after_secs: retry_after,
            },
            EvidenceKind::SelfReported,
            None,
        );
        tracing::warn!(
            "[provider] key {} for {} is rate-limited; cooling down for {}s",
            selected.key_id,
            selected.logical_name,
            memcore::vault::health::rate_limit_cooldown_secs(retry_after)
        );
    }

    pub(in crate::llm) fn mark_secret_exhausted(
        &self,
        selected: &SelectedProviderSecret,
        reason: Option<&str>,
    ) {
        self.apply_key_outcome(
            selected,
            TypedOutcome::Exhausted,
            EvidenceKind::SelfReported,
            reason,
        );
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn mark_provider_key_rate_limited_for_tests(
        &self,
        logical_name: &str,
        key_id: &str,
        retry_after: Option<u64>,
    ) {
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

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn expire_provider_key_cooldown_for_tests(&self, logical_name: &str, key_id: &str) {
        let now = Self::now_utc();
        let past = now - chrono::Duration::seconds(1);
        let persisted = {
            let mut state = self
                .provider_state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.remove_cooldown(logical_name, key_id);
            let health = state.get_or_insert_health(logical_name, key_id);
            health.status = HEALTH_RATE_LIMITED.to_string();
            health.cooldown_until = Some(past.to_rfc3339());
            health.auth_failed = false;
            health.disabled = false;
            health.updated_at = now.to_rfc3339();
            let persisted = health.clone();
            state.set_health_snapshot(
                logical_name,
                key_id,
                ProviderHealthSnapshot::from_health_parts(&persisted, Some(now), Some(past)),
            );
            persisted
        };
        self.persist_key_health_now(&persisted);
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn mark_provider_key_auth_failed_for_tests(&self, logical_name: &str, key_id: &str) {
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

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn expire_provider_key_auth_failure_for_tests(&self, logical_name: &str, key_id: &str) {
        let now = Self::now_utc();
        let stale = now - chrono::Duration::seconds(AUTH_FAILED_RETRY_TTL_SECS + 1);
        let persisted = {
            let mut state = self
                .provider_state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let health = state.get_or_insert_health(logical_name, key_id);
            health.status = HEALTH_AUTH_FAILED.to_string();
            health.auth_failed = true;
            health.disabled = false;
            health.cooldown_until = None;
            health.last_error = Some("forced stale auth failure".to_string());
            health.updated_at = stale.to_rfc3339();
            let persisted = health.clone();
            state.set_health_snapshot(
                logical_name,
                key_id,
                ProviderHealthSnapshot::from_health_parts(&persisted, Some(stale), None),
            );
            persisted
        };
        self.persist_key_health_now(&persisted);
    }

    pub fn record_provider_key_result(
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
        // #1680 D6: the status/outcome ladder this function used to carry is
        // now `TypedOutcome::classify`, shared with the CLI/MCP channel that
        // had drifted from it. The wire contract is unchanged; the outcome is
        // recorded as self-reported because the caller, not Tachi, observed it.
        let outcome = TypedOutcome::classify(status_code, outcome, retry_after);
        let reason = match outcome {
            // Rate-limit reports keep their generated "retry after Ns" text
            // rather than the caller's free-text reason, as before.
            TypedOutcome::RateLimited { .. } => None,
            TypedOutcome::Error => reason
                .map(str::to_string)
                .or_else(|| status_code.map(|code| format!("provider returned HTTP {code}"))),
            _ => reason.map(str::to_string),
        };
        match outcome {
            TypedOutcome::RateLimited { retry_after_secs } => {
                self.mark_secret_rate_limited(&selected, retry_after_secs)
            }
            TypedOutcome::Exhausted => self.mark_secret_exhausted(&selected, reason.as_deref()),
            TypedOutcome::AuthFailed => self.mark_secret_auth_failed(&selected, reason.as_deref()),
            TypedOutcome::Success => self.mark_secret_success(&selected),
            TypedOutcome::Error | TypedOutcome::Unknown => {
                self.apply_key_outcome(
                    &selected,
                    outcome,
                    EvidenceKind::SelfReported,
                    reason.as_deref(),
                );
            }
        }
        self.read_key_health_entry(logical_name, key_id)
            .unwrap_or_else(|| VaultKeyHealth {
                logical_name: logical_name.to_string(),
                key_id: key_id.to_string(),
                ..VaultKeyHealth::default()
            })
    }

    pub fn record_provider_key_result_blocking(
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

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn force_provider_health_reload_due_for_tests(&self) {
        let mut reload = self
            .provider_health_reload
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reload.last_attempt =
            Instant::now().checked_sub(Self::KEY_HEALTH_RELOAD_TTL + Duration::from_secs(1));
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn provider_key_health_for_tests(
        &self,
        logical_name: &str,
        key_id: &str,
    ) -> Option<VaultKeyHealth> {
        self.read_key_health_entry(logical_name, key_id)
    }
}

use super::persist::KeyHealthPersistIntent;
use super::*;

use crate::llm::catalog_import::DeploymentAttribution;

impl super::super::super::LlmClient {
    #[cfg(any(test, feature = "test-support"))]
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
    /// snapshot, and queueing the row's background persist. That persist is
    /// not debounced: every outcome schedules a tracked write, pending
    /// plain-success snapshots of one key may be merged into the next one, and
    /// the write reuses the client's retained vault handle while it is valid
    /// (see `provider_health/writer.rs`).
    ///
    /// `evidence` is never inferred: the invocation path and the caller-facing
    /// `record_provider_key_result` are [`EvidenceKind::SelfReported`] (a
    /// consumer of the key describing its own usage), while the auth probe —
    /// a deliberate request Tachi made and read itself — is
    /// [`EvidenceKind::Probed`].
    ///
    /// `attribution` says which catalog deployment (if any) served the request
    /// this outcome describes (#1681 D4, PR-C). It is a parameter rather than
    /// something derived here because it *cannot* be derived here:
    /// `logical_name` is an API-key env var shared by several lanes, so the
    /// credential identity this function is keyed on does not determine the
    /// deployment. The deployment write hangs off this seam — see
    /// `persistence::deployment` — so a 429 is dual-recorded (credential and
    /// deployment) from one observation, and 401/403 reaches the credential
    /// surfaces only.
    pub(in crate::llm) fn apply_key_outcome(
        &self,
        selected: &SelectedProviderSecret,
        outcome: TypedOutcome,
        evidence: EvidenceKind,
        reason: Option<&str>,
        attribution: DeploymentAttribution<'_>,
    ) -> VaultKeyHealth {
        let (health, _persist_result) = self.apply_key_outcome_with_persist(
            selected,
            outcome,
            evidence,
            reason,
            attribution,
            KeyHealthPersistIntent::Async,
        );
        health
    }

    fn apply_key_outcome_with_persist(
        &self,
        selected: &SelectedProviderSecret,
        outcome: TypedOutcome,
        evidence: EvidenceKind,
        reason: Option<&str>,
        attribution: DeploymentAttribution<'_>,
        persist_intent: KeyHealthPersistIntent,
    ) -> (VaultKeyHealth, Result<(), String>) {
        let prepared_async = self.prepare_key_health_persist(persist_intent);
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
        let write = memcore::vault::health::record_key_outcome_for_generation(
            existing.as_ref(),
            &selected.logical_name,
            &selected.key_id,
            outcome,
            evidence,
            reason,
            now,
            selected.credential_generation,
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
        let persist_ticket =
            self.enqueue_key_health_for_persist(&persisted, persist_intent, prepared_async);
        drop(state);
        let persist_result = self.complete_key_health_persist(persist_ticket);
        // The deployment half of the same observation (#1681 D4). Deliberately
        // after the credential write and deliberately returning nothing: the
        // two authorities never interfere, and a health row that cannot be
        // written must not change what this function does.
        self.note_deployment_outcome(attribution, outcome, evidence);
        (persisted, persist_result)
    }

    pub(in crate::llm) fn mark_secret_auth_failed(
        &self,
        selected: &SelectedProviderSecret,
        reason: Option<&str>,
        attribution: DeploymentAttribution<'_>,
    ) {
        self.apply_key_outcome(
            selected,
            TypedOutcome::AuthFailed,
            EvidenceKind::SelfReported,
            reason,
            attribution,
        );
    }

    pub(in crate::llm) fn mark_secret_success(
        &self,
        selected: &SelectedProviderSecret,
        attribution: DeploymentAttribution<'_>,
    ) {
        self.apply_key_outcome(
            selected,
            TypedOutcome::Success,
            EvidenceKind::SelfReported,
            None,
            attribution,
        );
    }

    pub(in crate::llm) fn mark_secret_rate_limited(
        &self,
        selected: &SelectedProviderSecret,
        retry_after: Option<u64>,
        attribution: DeploymentAttribution<'_>,
    ) {
        self.apply_key_outcome(
            selected,
            TypedOutcome::RateLimited {
                retry_after_secs: retry_after,
            },
            EvidenceKind::SelfReported,
            None,
            attribution,
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
        attribution: DeploymentAttribution<'_>,
    ) {
        self.apply_key_outcome(
            selected,
            TypedOutcome::Exhausted,
            EvidenceKind::SelfReported,
            reason,
            attribution,
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
            credential_generation: self
                .provider_state
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .source_generation,
        };
        let (_health, _persist) = self.apply_key_outcome_with_persist(
            &selected,
            TypedOutcome::RateLimited {
                retry_after_secs: retry_after,
            },
            EvidenceKind::SelfReported,
            None,
            DeploymentAttribution::Unattributed,
            KeyHealthPersistIntent::BlockingAck,
        );
        tracing::warn!(
            "[provider] key {} for {} is rate-limited; cooling down for {}s",
            selected.key_id,
            selected.logical_name,
            memcore::vault::health::rate_limit_cooldown_secs(retry_after)
        );
    }

    /// Test scaffold, not a fourth writer (#1680 D6). The two `expire_*`
    /// helpers forge a row *in the past* — an already-elapsed cooldown, an
    /// auth failure older than the retry TTL — which is precisely the one
    /// thing `record_key_outcome` cannot express: every write it makes is
    /// stamped `now`. They stay hand-written for that reason, and they are
    /// compiled out of production builds.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn expire_provider_key_cooldown_for_tests(&self, logical_name: &str, key_id: &str) {
        let now = Self::now_utc();
        let past = now - chrono::Duration::seconds(1);
        let persist_ticket = {
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
            self.enqueue_key_health_for_persist(
                &persisted,
                KeyHealthPersistIntent::BlockingAck,
                None,
            )
        };
        let _ = self.complete_key_health_persist(persist_ticket);
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn mark_provider_key_auth_failed_for_tests(&self, logical_name: &str, key_id: &str) {
        let selected = SelectedProviderSecret {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            value: String::new(),
            credential_generation: self
                .provider_state
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .source_generation,
        };
        let _ = self.apply_key_outcome_with_persist(
            &selected,
            TypedOutcome::AuthFailed,
            EvidenceKind::SelfReported,
            Some("forced auth failure"),
            DeploymentAttribution::Unattributed,
            KeyHealthPersistIntent::BlockingAck,
        );
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn expire_provider_key_auth_failure_for_tests(&self, logical_name: &str, key_id: &str) {
        let now = Self::now_utc();
        let stale = now - chrono::Duration::seconds(AUTH_FAILED_RETRY_TTL_SECS + 1);
        let persist_ticket = {
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
            self.enqueue_key_health_for_persist(
                &persisted,
                KeyHealthPersistIntent::BlockingAck,
                None,
            )
        };
        let _ = self.complete_key_health_persist(persist_ticket);
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
            credential_generation: None,
        };
        let (health, _persist_result) = self.record_selected_provider_key_result(
            selected,
            status_code,
            outcome,
            retry_after,
            reason,
            KeyHealthPersistIntent::Async,
        );
        health
    }

    fn record_selected_provider_key_result(
        &self,
        selected: SelectedProviderSecret,
        status_code: Option<u16>,
        outcome: Option<&str>,
        retry_after: Option<u64>,
        reason: Option<&str>,
        persist_intent: KeyHealthPersistIntent,
    ) -> (VaultKeyHealth, Result<(), String>) {
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
        let (health, persist_result) = self.apply_key_outcome_with_persist(
            &selected,
            outcome,
            EvidenceKind::SelfReported,
            reason.as_deref(),
            DeploymentAttribution::Unattributed,
            persist_intent,
        );
        if let TypedOutcome::RateLimited { retry_after_secs } = outcome {
            tracing::warn!(
                "[provider] key {} for {} is rate-limited; cooling down for {}s",
                selected.key_id,
                selected.logical_name,
                memcore::vault::health::rate_limit_cooldown_secs(retry_after_secs)
            );
        }
        (health, persist_result)
    }

    /// Record a result and acknowledge this call's durable write.
    pub fn record_provider_key_result_blocking(
        &self,
        logical_name: &str,
        key_id: &str,
        status_code: Option<u16>,
        outcome: Option<&str>,
        retry_after: Option<u64>,
        reason: Option<&str>,
    ) -> Result<VaultKeyHealth, String> {
        let (health, persist_result) = self.record_selected_provider_key_result(
            SelectedProviderSecret {
                logical_name: logical_name.to_string(),
                key_id: key_id.to_string(),
                value: String::new(),
                credential_generation: None,
            },
            status_code,
            outcome,
            retry_after,
            reason,
            KeyHealthPersistIntent::BlockingAck,
        );
        persist_result?;
        Ok(health)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn record_provider_key_result_for_generation_tests(
        &self,
        logical_name: &str,
        key_id: &str,
        status_code: Option<u16>,
        credential_generation: u64,
    ) -> VaultKeyHealth {
        let (health, _persist_result) = self.record_selected_provider_key_result(
            SelectedProviderSecret {
                logical_name: logical_name.to_string(),
                key_id: key_id.to_string(),
                value: String::new(),
                credential_generation: Some(credential_generation),
            },
            status_code,
            None,
            None,
            None,
            KeyHealthPersistIntent::BlockingAck,
        );
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

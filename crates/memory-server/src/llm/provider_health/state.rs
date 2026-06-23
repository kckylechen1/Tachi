use super::super::LlmClient;
use super::*;

#[derive(Debug, Clone)]
pub(in crate::llm) struct ProviderHealthSnapshot {
    pub(in crate::llm) availability: KeyAvailability,
    pub(in crate::llm) cooldown_until: Option<DateTime<Utc>>,
    pub(in crate::llm) updated_at: Option<DateTime<Utc>>,
}

impl ProviderHealthSnapshot {
    pub(in crate::llm) fn from_health(health: &VaultKeyHealth) -> Self {
        Self::from_health_parts(
            health,
            LlmClient::parse_timestamp(&health.updated_at),
            health
                .cooldown_until
                .as_deref()
                .and_then(LlmClient::parse_timestamp),
        )
    }

    pub(in crate::llm) fn from_health_parts(
        health: &VaultKeyHealth,
        updated_at: Option<DateTime<Utc>>,
        cooldown_until: Option<DateTime<Utc>>,
    ) -> Self {
        let mut snapshot = Self {
            availability: KeyAvailability::Available,
            cooldown_until: None,
            updated_at,
        };

        if health.disabled {
            snapshot.availability = KeyAvailability::Disabled;
            return snapshot;
        }

        if health.auth_failed {
            snapshot.availability = KeyAvailability::AuthFailed;
            return snapshot;
        }

        match LlmClient::status_from_key_health_status(health.status.as_str()) {
            HEALTH_RATE_LIMITED => {
                snapshot.cooldown_until = cooldown_until;
                snapshot.availability = if snapshot.cooldown_until.is_some() {
                    KeyAvailability::Cooldown
                } else {
                    KeyAvailability::Available
                };
            }
            HEALTH_EXHAUSTED => {
                snapshot.availability = KeyAvailability::Exhausted;
            }
            HEALTH_AUTH_FAILED => {
                snapshot.availability = KeyAvailability::AuthFailed;
            }
            HEALTH_DISABLED => {
                snapshot.availability = KeyAvailability::Disabled;
            }
            _ => {}
        }

        snapshot
    }

    pub(in crate::llm) fn availability_at(
        &self,
        now: DateTime<Utc>,
    ) -> (KeyAvailability, Option<i64>) {
        if self.availability == KeyAvailability::Cooldown {
            if let Some(until) = self.cooldown_until {
                let remaining_seconds = (until - now).num_seconds().max(0);
                if until > now {
                    return (KeyAvailability::Cooldown, Some(remaining_seconds));
                }
            }
            return (KeyAvailability::Available, None);
        }

        (self.availability, None)
    }
}

#[derive(Default)]
pub(in crate::llm) struct ProviderState {
    pub(in crate::llm) secrets: HashMap<String, Vec<ProviderSecret>>,
    pub(in crate::llm) cooldowns: HashMap<String, Instant>,
    pub(in crate::llm) indices: HashMap<String, usize>,
    pub(in crate::llm) health: HashMap<String, HashMap<String, VaultKeyHealth>>,
    pub(in crate::llm) health_snapshots: HashMap<String, HashMap<String, ProviderHealthSnapshot>>,
}

#[derive(Debug, Clone)]
pub(in crate::llm) struct ProviderHealthReloadState {
    pub(in crate::llm) source_of_truth: &'static str,
    pub(in crate::llm) last_attempt: Option<Instant>,
    pub(in crate::llm) last_attempt_at: Option<String>,
    pub(in crate::llm) last_success: Option<Instant>,
    pub(in crate::llm) last_success_at: Option<String>,
    pub(in crate::llm) last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(in crate::llm) struct ProviderHealthPersistState {
    pub(in crate::llm) last_attempt_at: Option<String>,
    pub(in crate::llm) last_success: Option<Instant>,
    pub(in crate::llm) last_success_at: Option<String>,
    pub(in crate::llm) last_error: Option<String>,
}

impl ProviderHealthReloadState {
    pub(in crate::llm) fn memory_only() -> Self {
        Self {
            source_of_truth: "memory_only",
            last_attempt: None,
            last_attempt_at: None,
            last_success: None,
            last_success_at: None,
            last_error: None,
        }
    }

    pub(in crate::llm) fn vault_db_attempt(now: Instant, now_utc: String) -> Self {
        Self {
            source_of_truth: "vault_db",
            last_attempt: Some(now),
            last_attempt_at: Some(now_utc),
            last_success: None,
            last_success_at: None,
            last_error: None,
        }
    }

    pub(in crate::llm) fn mark_success(&mut self, now: Instant, now_utc: String) {
        self.last_attempt = Some(now);
        self.last_attempt_at = Some(now_utc.clone());
        self.last_success = Some(now);
        self.last_success_at = Some(now_utc);
        self.last_error = None;
    }

    pub(in crate::llm) fn mark_error(&mut self, now: Instant, now_utc: String, error: String) {
        self.last_attempt = Some(now);
        self.last_attempt_at = Some(now_utc);
        self.last_error = Some(error);
    }
}

impl ProviderHealthPersistState {
    pub(in crate::llm) fn mark_success(&mut self, now: Instant, now_utc: String) {
        self.last_attempt_at = Some(now_utc.clone());
        self.last_success = Some(now);
        self.last_success_at = Some(now_utc);
        self.last_error = None;
    }

    pub(in crate::llm) fn mark_error(&mut self, now_utc: String, error: String) {
        self.last_attempt_at = Some(now_utc);
        self.last_error = Some(error);
    }
}

impl ProviderState {
    pub(in crate::llm) fn with_health(
        health: HashMap<String, HashMap<String, VaultKeyHealth>>,
    ) -> Self {
        let mut state = Self::default();
        for (logical_name, members) in health {
            for (key_id, health) in members {
                state.set_health_entry(logical_name.clone(), key_id, health);
            }
        }
        state
    }

    pub(in crate::llm) fn get_or_insert_health(
        &mut self,
        logical_name: &str,
        key_id: &str,
    ) -> &mut VaultKeyHealth {
        self.health
            .entry(logical_name.to_string())
            .or_default()
            .entry(key_id.to_string())
            .or_insert_with(|| VaultKeyHealth {
                logical_name: logical_name.to_string(),
                key_id: key_id.to_string(),
                status: HEALTH_OK.to_string(),
                cooldown_until: None,
                last_success: None,
                last_attempt: None,
                last_error: None,
                error_count: 0,
                auth_failed: false,
                disabled: false,
                metadata: "{}".to_string(),
                updated_at: Utc::now().to_rfc3339(),
            })
    }

    fn set_health_entry(&mut self, logical_name: String, key_id: String, health: VaultKeyHealth) {
        let snapshot = ProviderHealthSnapshot::from_health(&health);
        self.set_health_entry_with_snapshot(logical_name, key_id, health, snapshot);
    }

    pub(in crate::llm) fn set_health_entry_with_snapshot(
        &mut self,
        logical_name: String,
        key_id: String,
        health: VaultKeyHealth,
        snapshot: ProviderHealthSnapshot,
    ) {
        self.health
            .entry(logical_name.clone())
            .or_default()
            .insert(key_id.clone(), health);
        self.health_snapshots
            .entry(logical_name)
            .or_default()
            .insert(key_id, snapshot);
    }

    pub(in crate::llm) fn set_health_snapshot(
        &mut self,
        logical_name: &str,
        key_id: &str,
        snapshot: ProviderHealthSnapshot,
    ) {
        self.health_snapshots
            .entry(logical_name.to_string())
            .or_default()
            .insert(key_id.to_string(), snapshot);
    }

    pub(in crate::llm) fn prune_expired_cooldowns(&mut self, now: Instant) {
        self.cooldowns.retain(|_, until| *until > now);
    }
}

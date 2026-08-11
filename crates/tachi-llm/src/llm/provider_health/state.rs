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
        if self.availability == KeyAvailability::AuthFailed {
            if let Some(updated_at) = self.updated_at {
                if (now - updated_at).num_seconds() >= AUTH_FAILED_RETRY_TTL_SECS {
                    return (KeyAvailability::Available, None);
                }
            }
        }

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
    /// Ephemeral cooldowns use the same exact identity as persisted health:
    /// logical provider name plus member key id. Member ids may legitimately
    /// collide across independent logical pools.
    pub(in crate::llm) cooldowns: HashMap<String, HashMap<String, Instant>>,
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
    tracker: Arc<ProviderHealthPersistTracker>,
}

#[derive(Debug, Default)]
pub(in crate::llm) struct ProviderHealthPersistTracker {
    pending: std::sync::atomic::AtomicUsize,
    terminal: tokio::sync::Notify,
    /// A terminal failure is a phase fact, not a one-consumer message. Every
    /// waiter joining the same persistence boundary must see it; taking this
    /// value would let the first waiter authorize a later waiter incorrectly.
    first_terminal_error: std::sync::Mutex<Option<String>>,
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
    pub(in crate::llm) fn tracker(&self) -> Arc<ProviderHealthPersistTracker> {
        Arc::clone(&self.tracker)
    }

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

impl ProviderHealthPersistTracker {
    pub(in crate::llm) fn begin(&self) {
        self.pending
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    pub(in crate::llm) fn record_error(&self, error: String) {
        let mut first_error = self
            .first_terminal_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if first_error.is_none() {
            *first_error = Some(error);
        }
    }

    pub(in crate::llm) fn complete(&self) {
        let previous = self
            .pending
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        debug_assert!(previous > 0, "provider-health persist tracker underflow");
        if previous == 1 {
            self.terminal.notify_waiters();
        }
    }

    pub(in crate::llm) async fn wait_until_terminal(&self) -> Result<(), String> {
        loop {
            // Register before reading `pending` so the final completion cannot
            // race between the zero check and waiter registration.
            let notified = self.terminal.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.pending.load(std::sync::atomic::Ordering::Acquire) == 0 {
                let error = self
                    .first_terminal_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                return error.map_or(Ok(()), Err);
            }
            notified.await;
        }
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
            // #1680 D6: the fresh-row shape (including `metadata`) belongs to
            // the single writer, not to a literal repeated per construction
            // site.
            .or_insert_with(|| {
                memcore::vault::health::new_key_health(logical_name, key_id, Utc::now())
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
        self.cooldowns.retain(|_, members| {
            members.retain(|_, until| *until > now);
            !members.is_empty()
        });
    }

    pub(in crate::llm) fn cooldown_until(
        &self,
        logical_name: &str,
        key_id: &str,
    ) -> Option<&Instant> {
        self.cooldowns
            .get(logical_name)
            .and_then(|members| members.get(key_id))
    }

    pub(in crate::llm) fn is_cooling_down(&self, logical_name: &str, key_id: &str) -> bool {
        self.cooldown_until(logical_name, key_id).is_some()
    }

    pub(in crate::llm) fn set_cooldown(
        &mut self,
        logical_name: &str,
        key_id: &str,
        until: Instant,
    ) {
        self.cooldowns
            .entry(logical_name.to_string())
            .or_default()
            .insert(key_id.to_string(), until);
    }

    pub(in crate::llm) fn remove_cooldown(&mut self, logical_name: &str, key_id: &str) {
        let remove_logical = self.cooldowns.get_mut(logical_name).is_some_and(|members| {
            members.remove(key_id);
            members.is_empty()
        });
        if remove_logical {
            self.cooldowns.remove(logical_name);
        }
    }
}

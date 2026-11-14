use super::*;

// This is deliberately below doctor's 10-second join deadline. A one-shot
// doctor process may not time out a waiter and then leave a `spawn_blocking`
// SQLite writer alive; the writer itself must return before the process may
// emit its terminal receipt. The two-second SQLite budget leaves room for the
// bounded MemCore startup retry to return its typed BUSY/LOCKED error and for
// the supervisor to join the exact blocking task.
const PROVIDER_HEALTH_PERSIST_SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

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

#[derive(Clone, Copy)]
pub(in crate::llm::provider_health::persistence) enum KeyHealthPersistIntent {
    Async,
    BlockingAck,
}

pub(in crate::llm::provider_health::persistence) enum KeyHealthPersistTicket {
    Skipped,
    MissingDb,
    AsyncTask {
        handle: tokio::runtime::Handle,
        completion: ProviderHealthPersistCompletion,
        logical_name: String,
        key_id: String,
    },
    SyncFlush {
        logical_name: String,
        key_id: String,
        seq: u64,
    },
    BlockingAck {
        logical_name: String,
        key_id: String,
        seq: u64,
    },
}

impl super::super::super::LlmClient {
    pub async fn await_provider_health_persistence(&self) -> Result<(), String> {
        let tracker = self
            .provider_health_persist
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tracker();
        tracker.wait_until_terminal().await
    }

    /// Prepare any async completion token before acquiring `provider_state.write()`
    /// so `provider_state` only ever nests the leaf `pending_key_health` mutex.
    pub(in crate::llm::provider_health::persistence) fn prepare_key_health_persist(
        &self,
        intent: KeyHealthPersistIntent,
    ) -> Option<(tokio::runtime::Handle, ProviderHealthPersistCompletion)> {
        if provider_key_health_persist_disabled_for_tests() || self.vault_db_path.is_none() {
            return None;
        }
        match intent {
            KeyHealthPersistIntent::Async => {
                let handle = tokio::runtime::Handle::try_current().ok()?;
                let tracker = self
                    .provider_health_persist
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .tracker();
                Some((handle, tracker.track()))
            }
            KeyHealthPersistIntent::BlockingAck => None,
        }
    }

    /// Enqueue `health` while holding `provider_state.write()` so the queue's
    /// sequence order matches in-memory state transitions one-for-one (#2338).
    pub(in crate::llm::provider_health::persistence) fn enqueue_key_health_for_persist(
        &self,
        health: &VaultKeyHealth,
        intent: KeyHealthPersistIntent,
        prepared_async: Option<(tokio::runtime::Handle, ProviderHealthPersistCompletion)>,
    ) -> KeyHealthPersistTicket {
        if provider_key_health_persist_disabled_for_tests() {
            return KeyHealthPersistTicket::Skipped;
        }
        if self.vault_db_path.is_none() {
            return match intent {
                KeyHealthPersistIntent::Async => KeyHealthPersistTicket::Skipped,
                KeyHealthPersistIntent::BlockingAck => KeyHealthPersistTicket::MissingDb,
            };
        }
        let mut enqueued = health.clone();
        let now = Self::now_utc();
        enqueued.updated_at = match Self::parse_timestamp(&enqueued.updated_at) {
            Some(existing) if existing > now => existing.to_rfc3339(),
            _ => now.to_rfc3339(),
        };
        let logical_name = enqueued.logical_name.clone();
        let key_id = enqueued.key_id.clone();
        match (intent, prepared_async) {
            (KeyHealthPersistIntent::Async, Some((handle, completion))) => {
                self.provider_persist_writer
                    .enqueue_key_health(enqueued, true, false);
                KeyHealthPersistTicket::AsyncTask {
                    handle,
                    completion,
                    logical_name,
                    key_id,
                }
            }
            (KeyHealthPersistIntent::Async, None) => {
                let seq = self
                    .provider_persist_writer
                    .enqueue_key_health(enqueued, false, false);
                KeyHealthPersistTicket::SyncFlush {
                    logical_name,
                    key_id,
                    seq,
                }
            }
            (KeyHealthPersistIntent::BlockingAck, _) => {
                let seq = self
                    .provider_persist_writer
                    .enqueue_key_health(enqueued, false, true);
                KeyHealthPersistTicket::BlockingAck {
                    logical_name,
                    key_id,
                    seq,
                }
            }
        }
    }

    /// Execute the persistence action after releasing `provider_state.write()`.
    pub(in crate::llm::provider_health::persistence) fn complete_key_health_persist(
        &self,
        ticket: KeyHealthPersistTicket,
    ) -> Result<(), String> {
        match ticket {
            KeyHealthPersistTicket::Skipped => Ok(()),
            KeyHealthPersistTicket::MissingDb => {
                Err("Vault health persistence requires a configured database".to_string())
            }
            KeyHealthPersistTicket::AsyncTask {
                handle,
                completion,
                logical_name,
                key_id,
            } => {
                let Some(db_path) = self.vault_db_path.clone() else {
                    return Ok(());
                };
                let migration = self.vault_db_migration.clone();
                let writer = Arc::clone(&self.provider_persist_writer);
                let persist_state = Arc::clone(&self.provider_health_persist);
                let background_persist_lock = Arc::clone(&self.background_persist_lock);
                handle.spawn(async move {
                    let _completion = completion;
                    let join_result = {
                        let _persist_guard = background_persist_lock.lock().await;
                        let task_logical_name = logical_name.clone();
                        let task_key_id = key_id.clone();
                        let task_persist_state = Arc::clone(&persist_state);
                        tokio::task::spawn_blocking(move || {
                            let mut drain = writer.lock_key_health_drain();
                            while let Some(entry) =
                                writer.take_key_health(&task_logical_name, &task_key_id)
                            {
                                let write_result = Self::persist_key_health_blocking(
                                    &writer,
                                    db_path.clone(),
                                    migration.clone(),
                                    entry.health,
                                );
                                Self::record_key_health_persist_result(
                                    &task_persist_state,
                                    write_result.clone(),
                                );
                                if entry.needs_ack {
                                    drain.record_ack(entry.seq, write_result);
                                }
                            }
                        })
                        .await
                    };
                    if let Err(err) = join_result {
                        let cause = if err.is_cancelled() {
                            crate::llm::PROVIDER_HEALTH_PERSIST_CANCELLED_CAUSE
                        } else {
                            "provider_health_persist_join_failure"
                        };
                        Self::record_key_health_persist_result(
                            &persist_state,
                            Err(format!(
                                "persist vault key health for {logical_name}:{key_id}: {cause}: {err}"
                            )),
                        );
                    }
                });
                Ok(())
            }
            KeyHealthPersistTicket::SyncFlush {
                logical_name,
                key_id,
                seq,
            } => {
                let _ = self.flush_key_health_through(&logical_name, &key_id, seq, false);
                Ok(())
            }
            KeyHealthPersistTicket::BlockingAck {
                logical_name,
                key_id,
                seq,
            } => self.flush_key_health_through(&logical_name, &key_id, seq, true),
        }
    }

    fn flush_key_health_through(
        &self,
        logical_name: &str,
        key_id: &str,
        target_seq: u64,
        require_ack: bool,
    ) -> Result<(), String> {
        let Some(db_path) = self.vault_db_path.clone() else {
            return Err("Vault health persistence requires a configured database".to_string());
        };
        let migration = self.vault_db_migration.clone();
        // Hold `key_health_drain` across both the SQLite write and
        // `record_key_health_persist_result` so a background task whose entry
        // was drained here cannot observe an empty queue and finish its
        // completion token before a delegated write failure is published (#2338).
        let mut drain = self.provider_persist_writer.lock_key_health_drain();
        let mut own_result = None;
        while let Some(entry) =
            self.provider_persist_writer
                .take_key_health_through(logical_name, key_id, target_seq)
        {
            let write_result = Self::persist_key_health_blocking(
                &self.provider_persist_writer,
                db_path.clone(),
                migration.clone(),
                entry.health,
            );
            Self::record_key_health_persist_result(
                &self.provider_health_persist,
                write_result.clone(),
            );
            if require_ack && entry.seq == target_seq {
                own_result = Some(write_result);
            } else if entry.needs_ack {
                drain.record_ack(entry.seq, write_result);
            }
        }
        if require_ack {
            own_result.unwrap_or_else(|| {
                drain.take_ack(target_seq).unwrap_or_else(|| {
                    Err(format!(
                        "persist vault key health for {logical_name}:{key_id}: missing durable acknowledgement for seq {target_seq}"
                    ))
                })
            })
        } else {
            Ok(())
        }
    }

    fn persist_key_health_blocking(
        writer: &ProviderPersistWriter,
        db_path: PathBuf,
        migration: memcore::MigrationAuthority,
        health: VaultKeyHealth,
    ) -> Result<(), String> {
        let target = format!("{}:{}", health.logical_name, health.key_id);
        let Some(db_path_str) = db_path.to_str() else {
            return Err(format!(
                "persist vault key health for {target}: invalid db path"
            ));
        };
        let open_context = memcore::DbOpenContext {
            migration,
            // #1585 D2: this writes `vault_key_health`, a product table.
            required_profile: memcore::ProfileRequirement::AtLeast(
                memcore::StoreProfile::TachiFull,
            ),
            ..memcore::DbOpenContext::open_existing_deny()
        };
        // #1680 D6 on both paths: a retained handle writes under startup
        // ownership, and a fresh open keeps open and upsert under one hold.
        writer
            .write(
                &db_path,
                |store| store.vault_upsert_key_health_with_startup_ownership(&health),
                || {
                    memcore::MemoryStore::open_and_vault_upsert_key_health_retaining_store(
                        db_path_str,
                        &open_context,
                        PROVIDER_HEALTH_PERSIST_SQLITE_BUSY_TIMEOUT,
                        &health,
                    )
                    .map(|store| (store, ()))
                },
            )
            .map_err(|err| Self::provider_health_persist_error(&target, err))?;
        #[cfg(test)]
        writer.note_key_health_written(&health);
        Ok(())
    }

    fn provider_health_persist_error(target: &str, error: memcore::MemoryError) -> String {
        let deadline_exhausted = matches!(
            &error,
            memcore::MemoryError::Sqlite(sqlite) if memcore::db::sqlite_error_is_locked(sqlite)
        );
        if deadline_exhausted {
            format!(
                "persist vault key health for {target}: {}: {error}",
                crate::llm::PROVIDER_HEALTH_PERSIST_SQLITE_DEADLINE_CAUSE
            )
        } else {
            format!("persist vault key health for {target}: {error}")
        }
    }

    fn record_key_health_persist_result(
        persist_state: &Arc<RwLock<ProviderHealthPersistState>>,
        result: Result<(), String>,
    ) {
        let terminal_error = result.as_ref().err().cloned();
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
        if let Some(error) = terminal_error {
            state.tracker().record_error(error);
        }
    }
}

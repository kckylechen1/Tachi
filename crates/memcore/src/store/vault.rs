//! Encrypted-secret vault + key-rotation methods on [`MemoryStore`].

use crate::db;
use crate::error::MemoryError;
use crate::vault::{
    api_key_pool_member_index, VaultConfig, VaultEntry, VaultKeyHealth, VaultKeyRotation,
    SECRET_TYPE_API_KEY,
};
use crate::MemoryStore;

impl MemoryStore {
    // ─── Vault Entries ───────────────────────────────────────────────────────

    /// Get vault configuration (returns None if not initialized).
    pub fn vault_get_config(&self) -> Result<Option<VaultConfig>, MemoryError> {
        db::vault_get_config(&self.conn)
    }

    /// Set vault configuration.
    pub fn vault_set_config(&self, config: &VaultConfig) -> Result<(), MemoryError> {
        db::vault_set_config(&self.conn, config)
    }

    /// Store or update an encrypted secret.
    pub fn vault_upsert_entry(&self, entry: &VaultEntry) -> Result<(), MemoryError> {
        db::vault_upsert_entry(&self.conn, entry)
    }

    /// Replace a logical API-key pool and its rotation row in one transaction.
    pub fn vault_replace_api_key_pool(
        &mut self,
        prefix: &str,
        entries: &[VaultEntry],
        rotation: &VaultKeyRotation,
    ) -> Result<Vec<String>, MemoryError> {
        let tx = self.conn.transaction()?;
        let existing_entries = db::vault_list_entries_by_type(&tx, SECRET_TYPE_API_KEY)?;
        let mut surplus_members: Vec<String> = existing_entries
            .iter()
            .filter(|entry| {
                api_key_pool_member_index(&entry.name, prefix)
                    .is_some_and(|idx| idx > entries.len())
            })
            .map(|entry| entry.name.clone())
            .collect();
        surplus_members.sort();
        if !surplus_members.is_empty() {
            return Err(MemoryError::InvalidArg(format!(
                "refusing API-key pool shrink for '{prefix}': default replacement would delete surplus members [{}]; archive or remove those named members explicitly",
                surplus_members.join(", ")
            )));
        }

        for entry in entries {
            let mut entry = entry.clone();
            if db::vault_entry_exists(&tx, &entry.name)? {
                entry.created_at.clear();
            }
            db::vault_upsert_entry(&tx, &entry)?;
        }

        db::vault_set_rotation(&tx, rotation)?;
        tx.commit()?;
        Ok(Vec::new())
    }

    /// Import a Vault sync bundle atomically.
    ///
    /// A sync bundle spans the singleton config row, encrypted entries, and
    /// rotation rows. Import callers must not leave a target Vault half-initialized
    /// if a later row fails validation or persistence.
    ///
    /// # Unchecked primitive — caller contract
    ///
    /// The `_unchecked` suffix is load-bearing (tachi#1110): this is a
    /// low-level storage primitive that writes `config` verbatim and does
    /// **not** validate `kdf_params`/`kdf_algorithm` for support. This crate
    /// (memcore) is a storage leaf and intentionally has no dependency on
    /// `vault-kit`'s KDF-parameter validation, so that check cannot live
    /// here (Refs kckylechen1/tachi#1106 layering ruling — memcore treats
    /// `kdf_params` as an opaque `String`).
    ///
    /// **Callers must validate the incoming `VaultConfig`'s `kdf_params`/
    /// `kdf_algorithm` via `vault-kit`'s `KdfParams` before calling this
    /// method.** Skipping that step can persist a Vault whose KDF is never
    /// unlockable — a day-one brick with no recovery path.
    ///
    /// The sole current caller, `tachi-server`'s
    /// `bootstrap::vault_sync::import_validated_vault_bundle` (the
    /// crypto-aware layer's single validating import wrapper), performs this
    /// validation before this method is ever reached (Refs
    /// kckylechen1/Hyperion-HyperTachi#28, tachi#1080, tachi#1110 — this
    /// method was named `vault_import_bundle` before #1110 renamed it to put
    /// the unvalidated nature in the name itself, rather than relying on
    /// caller discipline alone).
    pub fn vault_import_bundle_unchecked(
        &mut self,
        config: &VaultConfig,
        entries: &[VaultEntry],
        rotations: &[VaultKeyRotation],
    ) -> Result<(), MemoryError> {
        let tx = self.conn.transaction()?;
        db::vault_set_config(&tx, config)?;
        for entry in entries {
            db::vault_upsert_entry(&tx, entry)?;
        }
        for rotation in rotations {
            db::vault_set_rotation(&tx, rotation)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Get a secret by name (returns None if not found).
    pub fn vault_get_entry(&self, name: &str) -> Result<Option<VaultEntry>, MemoryError> {
        db::vault_get_entry(&self.conn, name)
    }

    /// List all secrets.
    pub fn vault_list_entries(&self) -> Result<Vec<VaultEntry>, MemoryError> {
        db::vault_list_entries(&self.conn)
    }

    /// List vault entry names with `updated_at` only (no ciphertext/nonce).
    pub fn vault_list_entry_timestamps(&self) -> Result<Vec<(String, String)>, MemoryError> {
        db::vault_list_entry_timestamps(&self.conn)
    }

    /// List secrets filtered by type.
    pub fn vault_list_entries_by_type(
        &self,
        secret_type: &str,
    ) -> Result<Vec<VaultEntry>, MemoryError> {
        db::vault_list_entries_by_type(&self.conn, secret_type)
    }

    /// Delete a secret by name. Returns true if deleted, false if not found.
    pub fn vault_delete_entry(&self, name: &str) -> Result<bool, MemoryError> {
        db::vault_delete_entry(&self.conn, name)
    }

    /// Touch a secret (update accessed_at and increment access_count). Returns
    /// the post-touch access_count from a single UPDATE...RETURNING so callers
    /// don't race a follow-up SELECT.
    pub fn vault_touch_entry(&self, name: &str) -> Result<i64, MemoryError> {
        db::vault_touch_entry(&self.conn, name)
    }

    /// Touch all named secrets in one atomic SQLite transaction. The bounded
    /// loop deliberately lives inside that single transaction so a missing
    /// entry or database failure rolls back every access-count increment.
    pub fn vault_touch_entries_atomic(&mut self, names: &[String]) -> Result<(), MemoryError> {
        let tx = self.conn.transaction()?;
        for name in names {
            db::vault_touch_entry(&tx, name)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Insert a vault audit record.
    pub fn vault_insert_audit(
        &self,
        timestamp: &str,
        operation: &str,
        secret_name: Option<&str>,
        success: bool,
        detail: Option<&str>,
    ) -> Result<(), MemoryError> {
        db::vault_insert_audit(
            &self.conn,
            timestamp,
            operation,
            secret_name,
            success,
            detail,
        )
    }

    /// Count total number of secrets.
    pub fn vault_count_entries(&self) -> Result<i64, MemoryError> {
        db::vault_count_entries(&self.conn)
    }

    /// Check if a secret exists.
    pub fn vault_entry_exists(&self, name: &str) -> Result<bool, MemoryError> {
        db::vault_entry_exists(&self.conn, name)
    }

    // ─── Key Rotation ────────────────────────────────────────────────────────

    /// Get rotation state for a prefix.
    pub fn vault_get_rotation(
        &self,
        prefix: &str,
    ) -> Result<Option<VaultKeyRotation>, MemoryError> {
        db::vault_get_rotation(&self.conn, prefix)
    }

    /// Set rotation state for a prefix.
    pub fn vault_set_rotation(&self, rotation: &VaultKeyRotation) -> Result<(), MemoryError> {
        db::vault_set_rotation(&self.conn, rotation)
    }

    /// List all rotation configurations.
    pub fn vault_list_rotations(&self) -> Result<Vec<VaultKeyRotation>, MemoryError> {
        db::vault_list_rotations(&self.conn)
    }

    // ─── Key Health Ledger ────────────────────────────────────────────────────

    /// Insert or update one key-health row in the runtime ledger.
    pub fn vault_upsert_key_health(&self, health: &VaultKeyHealth) -> Result<(), MemoryError> {
        db::vault_upsert_key_health(&self.conn, health)
    }

    /// Read one key-health row.
    pub fn vault_get_key_health(
        &self,
        logical_name: &str,
        key_id: &str,
    ) -> Result<Option<VaultKeyHealth>, MemoryError> {
        db::vault_get_key_health(&self.conn, logical_name, key_id)
    }

    /// List key-health rows, optionally scoped by logical key name.
    pub fn vault_list_key_health(
        &self,
        logical_name: Option<&str>,
    ) -> Result<Vec<VaultKeyHealth>, MemoryError> {
        db::vault_list_key_health(&self.conn, logical_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(name: &str) -> VaultEntry {
        VaultEntry {
            name: name.to_string(),
            encrypted_value: format!("{name}-ciphertext"),
            nonce: format!("{name}-nonce"),
            secret_type: SECRET_TYPE_API_KEY.to_string(),
            description: "test pool member".to_string(),
            allowed_agents: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            accessed_at: String::new(),
            access_count: 0,
        }
    }

    fn test_rotation(prefix: &str, total_keys: i64) -> VaultKeyRotation {
        VaultKeyRotation {
            prefix: prefix.to_string(),
            current_index: 1,
            total_keys,
            rotation_strategy: "round_robin".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// Metadata-only timestamp listing must not require ciphertext fields and
    /// must preserve the same ordering and timestamps as the full entry list.
    #[test]
    fn vault_list_entry_timestamps_returns_name_and_updated_at_only() {
        let store = MemoryStore::open_in_memory().expect("open test store");
        let mut a = test_entry("ALPHA_KEY");
        a.encrypted_value = "ciphertext-must-not-be-required".to_string();
        a.nonce = "nonce-must-not-be-required".to_string();
        let b = test_entry("BETA_KEY");
        store.vault_upsert_entry(&a).expect("upsert a");
        store.vault_upsert_entry(&b).expect("upsert b");

        let timestamps = store
            .vault_list_entry_timestamps()
            .expect("list timestamps");
        assert_eq!(
            timestamps
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["ALPHA_KEY", "BETA_KEY"]
        );
        assert!(
            timestamps
                .iter()
                .all(|(_, updated_at)| !updated_at.is_empty()),
            "updated_at must be populated"
        );

        // Discrimination: narrow listing must agree with full-list metadata
        // without forcing callers to materialize ciphertext fields.
        let full = store.vault_list_entries().expect("full list");
        let from_full: Vec<(String, String)> =
            full.into_iter().map(|e| (e.name, e.updated_at)).collect();
        assert_eq!(timestamps, from_full);
    }

    /// tachi#1110: `vault_import_bundle_unchecked` is a storage-leaf
    /// primitive with no `vault-kit` dependency (the #1106 layering ruling),
    /// so it has no opinion on whether `kdf_params`/`kdf_algorithm` are a
    /// *supported* KDF profile — it persists whatever `VaultConfig` it is
    /// given verbatim. That is the documented contract the `_unchecked`
    /// suffix names; the validating gate lives one layer up, in
    /// `tachi-server`'s `bootstrap::vault_sync::import_validated_vault_bundle`
    /// (which memcore cannot see or depend on).
    ///
    /// Structural-discrimination note: this is a rename, not new persistence
    /// logic — the primitive had this exact no-validation behavior under its
    /// pre-#1110 name `vault_import_bundle` too, so there is no prior
    /// revision of this method that behaved differently to diff against
    /// (behavioral-red-then-green is not applicable to a pure rename). This
    /// test instead pins the contract the new name asserts, so a future
    /// change that quietly adds validation here (which would violate the
    /// #1106 layering ruling by requiring a `vault-kit` dependency) goes red.
    #[test]
    fn vault_import_bundle_unchecked_persists_unsupported_kdf_params_raw() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let config = VaultConfig {
            salt: "salt".to_string(),
            verifier: "verifier".to_string(),
            kdf_algorithm: "not-a-real-algorithm".to_string(),
            kdf_params: r#"{"m":1,"t":1,"p":1}"#.to_string(),
            cipher: crate::vault::VaultCipher::Aes256Gcm,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };

        store
            .vault_import_bundle_unchecked(&config, &[], &[])
            .expect(
                "unchecked primitive must persist an unsupported KDF profile without validating it",
            );

        let stored = store
            .vault_get_config()
            .expect("read back config")
            .expect("config row must exist after import");
        assert_eq!(
            stored.kdf_algorithm, "not-a-real-algorithm",
            "unchecked primitive must write kdf_algorithm verbatim, no validation"
        );
        assert_eq!(
            stored.kdf_params, r#"{"m":1,"t":1,"p":1}"#,
            "unchecked primitive must write kdf_params verbatim, no validation"
        );
    }

    #[test]
    fn vault_replace_api_key_pool_rolls_back_when_rotation_write_fails() {
        let dir = tempfile::tempdir().expect("vault rollback temp dir");
        let path = dir.path().join("memory.db");
        let mut store = MemoryStore::open(&path.to_string_lossy()).expect("open test store");
        let offline = rusqlite::Connection::open(&path).expect("open offline trigger fixture");
        offline
            .execute_batch(
                "CREATE TRIGGER fail_pool_rotation
                 BEFORE INSERT ON vault_key_rotations
                 WHEN NEW.prefix = 'FAIL_API_KEY'
                 BEGIN
                   SELECT RAISE(ABORT, 'forced rotation failure');
                 END;",
            )
            .expect("install failure trigger");
        drop(offline);

        let err = store
            .vault_replace_api_key_pool(
                "FAIL_API_KEY",
                &[test_entry("FAIL_API_KEY_1"), test_entry("FAIL_API_KEY_2")],
                &test_rotation("FAIL_API_KEY", 2),
            )
            .expect_err("rotation failure should abort replacement");

        assert!(
            err.to_string().contains("forced rotation failure"),
            "unexpected error: {err}"
        );
        assert!(
            store
                .vault_get_entry("FAIL_API_KEY_1")
                .expect("read first member")
                .is_none(),
            "pool member inserted before the failing rotation must roll back"
        );
        assert!(
            store
                .vault_get_entry("FAIL_API_KEY_2")
                .expect("read second member")
                .is_none(),
            "pool member inserted before the failing rotation must roll back"
        );
        assert!(
            store
                .vault_get_rotation("FAIL_API_KEY")
                .expect("read rotation")
                .is_none(),
            "failed replacement must not leave a rotation row"
        );
    }

    #[test]
    fn vault_replace_api_key_pool_refuses_to_delete_surplus_members() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .vault_replace_api_key_pool(
                "SHRINK_API_KEY",
                &[
                    test_entry("SHRINK_API_KEY_1"),
                    test_entry("SHRINK_API_KEY_2"),
                ],
                &test_rotation("SHRINK_API_KEY", 2),
            )
            .expect("seed pool");

        let err = store
            .vault_replace_api_key_pool(
                "SHRINK_API_KEY",
                &[test_entry("SHRINK_API_KEY_1")],
                &test_rotation("SHRINK_API_KEY", 1),
            )
            .expect_err("default pool replacement must refuse to delete a surplus member");

        assert!(err.to_string().contains("SHRINK_API_KEY_2"), "{err}");
        assert!(store
            .vault_get_entry("SHRINK_API_KEY_1")
            .expect("read retained member")
            .is_some());
        assert!(store
            .vault_get_entry("SHRINK_API_KEY_2")
            .expect("read protected surplus member")
            .is_some());
        assert_eq!(
            store
                .vault_get_rotation("SHRINK_API_KEY")
                .expect("read rotation")
                .expect("rotation exists")
                .total_keys,
            2,
            "refused shrink must leave the existing rotation untouched"
        );
    }

    /// `vault_touch_entries_atomic` is the batch that #1393 introduced so a
    /// provider-pool load records every member's access in one transaction.
    /// Its whole point is all-or-nothing, so the rollback needs a test that can
    /// actually run: injecting the failure with `CREATE TEMP TRIGGER` cannot,
    /// because `install_reserved_reference_authorizer` denies DDL on these
    /// connections — a fixture built that way fails with `not authorized`
    /// before it ever reaches the code under test.
    ///
    /// A missing name is a deterministic failure with no DDL at all:
    /// `vault_touch_entry` reads its post-touch count via `RETURNING`, so an
    /// UPDATE that matches no row surfaces as `QueryReturnedNoRows`.
    #[test]
    fn vault_touch_entries_atomic_rolls_back_every_touch_when_one_name_is_missing() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        for name in ["TOUCH_ROLLBACK_1", "TOUCH_ROLLBACK_2"] {
            store
                .vault_upsert_entry(&test_entry(name))
                .expect("seed entry");
        }
        store
            .vault_touch_entry("TOUCH_ROLLBACK_1")
            .expect("first touch succeeds");
        let baseline = |store: &MemoryStore, name: &str| {
            store
                .vault_get_entry(name)
                .expect("read entry")
                .expect("entry exists")
                .access_count
        };
        let before_1 = baseline(&store, "TOUCH_ROLLBACK_1");
        let before_2 = baseline(&store, "TOUCH_ROLLBACK_2");
        assert_eq!(before_1, 1, "fixture: first member was touched once");
        assert_eq!(before_2, 0);

        let err = store
            .vault_touch_entries_atomic(&[
                "TOUCH_ROLLBACK_1".to_string(),
                "TOUCH_ROLLBACK_2".to_string(),
                "TOUCH_ROLLBACK_ABSENT".to_string(),
            ])
            .expect_err("a name with no row must fail the batch");
        assert!(
            !err.to_string().is_empty(),
            "the batch must surface the underlying failure"
        );

        assert_eq!(
            baseline(&store, "TOUCH_ROLLBACK_1"),
            before_1,
            "the successful earlier touch in the same batch must roll back"
        );
        assert_eq!(
            baseline(&store, "TOUCH_ROLLBACK_2"),
            before_2,
            "no member may keep a partial increment"
        );
    }
}

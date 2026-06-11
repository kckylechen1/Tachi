//! Encrypted-secret vault + key-rotation methods on [`MemoryStore`].

use crate::db;
use crate::error::MemoryError;
use crate::vault::{
    api_key_pool_member_index, VaultConfig, VaultEntry, VaultKeyHealth, VaultKeyRotation,
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
        let existing_entries = db::vault_list_entries_by_type(&tx, "api_key")?;
        let mut removed_members = Vec::new();

        for entry in entries {
            let mut entry = entry.clone();
            if db::vault_entry_exists(&tx, &entry.name)? {
                entry.created_at.clear();
            }
            db::vault_upsert_entry(&tx, &entry)?;
        }

        for entry in existing_entries {
            if api_key_pool_member_index(&entry.name, prefix).is_some_and(|idx| idx > entries.len())
                && db::vault_delete_entry(&tx, &entry.name)?
            {
                removed_members.push(entry.name);
            }
        }

        db::vault_set_rotation(&tx, rotation)?;
        tx.commit()?;
        Ok(removed_members)
    }

    /// Get a secret by name (returns None if not found).
    pub fn vault_get_entry(&self, name: &str) -> Result<Option<VaultEntry>, MemoryError> {
        db::vault_get_entry(&self.conn, name)
    }

    /// List all secrets.
    pub fn vault_list_entries(&self) -> Result<Vec<VaultEntry>, MemoryError> {
        db::vault_list_entries(&self.conn)
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

    /// Delete rotation configuration for a prefix.
    pub fn vault_delete_rotation(&self, prefix: &str) -> Result<bool, MemoryError> {
        db::vault_delete_rotation(&self.conn, prefix)
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
            secret_type: "api_key".to_string(),
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

    #[test]
    fn vault_replace_api_key_pool_rolls_back_when_rotation_write_fails() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_pool_rotation
                 BEFORE INSERT ON vault_key_rotations
                 WHEN NEW.prefix = 'FAIL_API_KEY'
                 BEGIN
                   SELECT RAISE(ABORT, 'forced rotation failure');
                 END;",
            )
            .expect("install failure trigger");

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
    fn vault_replace_api_key_pool_removes_orphaned_members_atomically() {
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

        let removed = store
            .vault_replace_api_key_pool(
                "SHRINK_API_KEY",
                &[test_entry("SHRINK_API_KEY_1")],
                &test_rotation("SHRINK_API_KEY", 1),
            )
            .expect("shrink pool");

        assert_eq!(removed, vec!["SHRINK_API_KEY_2"]);
        assert!(store
            .vault_get_entry("SHRINK_API_KEY_1")
            .expect("read retained member")
            .is_some());
        assert!(store
            .vault_get_entry("SHRINK_API_KEY_2")
            .expect("read removed member")
            .is_none());
        assert_eq!(
            store
                .vault_get_rotation("SHRINK_API_KEY")
                .expect("read rotation")
                .expect("rotation exists")
                .total_keys,
            1
        );
    }
}

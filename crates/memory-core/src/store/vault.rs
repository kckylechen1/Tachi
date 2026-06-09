//! Encrypted-secret vault + key-rotation methods on [`MemoryStore`].

use crate::db;
use crate::error::MemoryError;
use crate::vault::{VaultConfig, VaultEntry, VaultKeyHealth, VaultKeyRotation};
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

//! Deterministic key-value state methods on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryStore};

impl MemoryStore {
    /// Set a deterministic key-value state.
    pub fn set_state(
        &self,
        namespace: &str,
        key: &str,
        value_json: &str,
    ) -> Result<u32, MemoryError> {
        db::set_state(&self.conn, namespace, key, value_json)
    }

    /// Insert a deterministic key-value state only when absent.
    pub fn insert_state_if_absent(
        &self,
        namespace: &str,
        key: &str,
        value_json: &str,
    ) -> Result<bool, MemoryError> {
        db::insert_state_if_absent(&self.conn, namespace, key, value_json)
    }

    /// Update a deterministic key-value state only when the version matches.
    pub fn set_state_if_version(
        &self,
        namespace: &str,
        key: &str,
        value_json: &str,
        expected_version: u32,
    ) -> Result<bool, MemoryError> {
        db::set_state_if_version(&self.conn, namespace, key, value_json, expected_version)
    }

    /// Get a deterministic key-value state.
    pub fn get_state_kv(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<(String, u32)>, MemoryError> {
        db::get_state(&self.conn, namespace, key)
    }

    /// List deterministic key-value state rows in a namespace, newest first.
    pub fn list_state(&self, namespace: &str) -> Result<Vec<db::StateRow>, MemoryError> {
        db::list_state(&self.conn, namespace)
    }

    /// Delete a single deterministic key-value state row. Returns whether a
    /// row was actually removed.
    pub fn delete_state(&self, namespace: &str, key: &str) -> Result<bool, MemoryError> {
        db::delete_state(&self.conn, namespace, key)
    }

    /// Reap `hard_state` rows across all namespaces whose JSON payload
    /// declares an `expires_at` that has passed. Returns the number of rows
    /// deleted. See `db::reap_expired_state` for what "expired" means and
    /// which rows are deliberately exempt.
    pub fn reap_expired_state(&self, now_rfc3339: &str) -> Result<usize, MemoryError> {
        db::reap_expired_state(&self.conn, now_rfc3339)
    }
}

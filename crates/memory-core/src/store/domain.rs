//! Domain configuration methods on [`MemoryStore`].

use crate::{db, error::MemoryError, types::DomainConfig, MemoryStore};

impl MemoryStore {
    /// Register or update a domain configuration.
    pub fn register_domain(&self, domain: &DomainConfig) -> Result<(), MemoryError> {
        db::register_domain(&self.conn, domain)
    }

    /// Get a domain configuration by name.
    pub fn get_domain(&self, name: &str) -> Result<Option<DomainConfig>, MemoryError> {
        db::get_domain(&self.conn, name)
    }

    /// List all registered domain configurations.
    pub fn list_domains(&self) -> Result<Vec<DomainConfig>, MemoryError> {
        db::list_domains(&self.conn)
    }

    /// Delete a domain configuration by name. Returns true if deleted.
    pub fn delete_domain(&self, name: &str) -> Result<bool, MemoryError> {
        db::delete_domain(&self.conn, name)
    }
}

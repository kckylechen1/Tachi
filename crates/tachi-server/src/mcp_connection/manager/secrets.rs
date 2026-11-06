use super::super::*;

impl MemoryServer {
    pub(super) fn resolve_vault_secret_for_capability(
        &self,
        capability_id: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        match read_unlocked_vault_secret(self, key, Some(capability_id), true) {
            Ok(value) => Ok(Some(value)),
            // An unavailable slot may try the next explicit Vault reference.
            // Environment references use their own syntax and resolver.
            Err(err)
                if crate::vault_ops::is_env_fallback_eligible(
                    crate::vault_ops::classify_vault_read_error(&err),
                ) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    pub(super) fn resolve_env_map_for_capability(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<HashMap<String, String>, String> {
        resolve_env_map_with_secret_resolver(def, &|key| {
            self.resolve_vault_secret_for_capability(capability_id, key)
        })
    }

    pub(super) fn resolve_header_map_for_capability(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<HashMap<HeaderName, HeaderValue>, String> {
        resolve_header_map_with_secret_resolver(def, &|key| {
            self.resolve_vault_secret_for_capability(capability_id, key)
        })
    }

    pub(super) fn resolve_auth_header_for_capability(
        &self,
        capability_id: &str,
        value: &str,
    ) -> Result<String, String> {
        expand_placeholders_with_secret_resolver(value, &|key| {
            self.resolve_vault_secret_for_capability(capability_id, key)
        })
    }

    pub(crate) fn resolve_remote_mcp_url_for_capability(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<String, String> {
        resolve_remote_mcp_url_with_secret_resolver(def, &|key| {
            self.resolve_vault_secret_for_capability(capability_id, key)
        })
    }
}

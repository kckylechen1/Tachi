use super::access::{ensure_agent_allowed, resolve_vault_acl_agent_id};
use super::audit::record_vault_audit;
use super::ctx::*;
use super::session::with_vault_key;
use crate::vault_crypto as crypto;
use memcore::vault::{VaultEntry, SECRET_TYPE_OAUTH_TOKEN};
use rmcp::transport::auth::{
    AuthError, CredentialRefreshGuard, CredentialStore, StoredCredentials,
};
use std::fs::OpenOptions;

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredMcpOAuthCredentials {
    resource: String,
    credentials: StoredCredentials,
}

/// The SDK owns OAuth; this adapter owns encrypted persistence and existing Vault ACLs.
#[derive(Clone)]
pub(crate) struct OAuthCredentialStore<C> {
    server: C,
    name: String,
    resource: String,
    consumer: Option<String>,
}

impl<C: VaultCtx> OAuthCredentialStore<C> {
    pub(crate) fn new(
        server: C,
        name: String,
        resource: &str,
        consumer: Option<String>,
    ) -> Result<Self, String> {
        crypto::validate_secret_name(&name)?;
        if super::is_lane_slot_secret_name(&name) || memcore::is_lane_config_url_name(&name) {
            return Err("MCP OAuth credentials require a dedicated Vault name".into());
        }
        let resource = url::Url::parse(resource)
            .map_err(|_| "Invalid MCP OAuth resource URL")?
            .to_string();
        Ok(Self {
            server,
            name,
            resource,
            consumer,
        })
    }

    fn authorize(&self, entry: &VaultEntry, consumer: Option<&str>) -> Result<(), String> {
        ensure_agent_allowed(entry, consumer).map_err(|error| error.to_string())?;
        if entry.secret_type != SECRET_TYPE_OAUTH_TOKEN {
            return Err("MCP OAuth reference is not an oauth_token Vault entry".into());
        }
        Ok(())
    }

    pub(crate) fn save_with<T>(
        &self,
        credentials: StoredCredentials,
        update: impl FnOnce(&memcore::store::vault::VaultTransaction<'_>) -> Result<T, String>,
    ) -> Result<T, AuthError> {
        let result = (|| {
            let consumer = resolve_vault_acl_agent_id(&self.server, self.consumer.as_deref())?;
            let plaintext = crypto::ZeroizingString::new(
                serde_json::to_string(&StoredMcpOAuthCredentials {
                    resource: self.resource.clone(),
                    credentials,
                })
                .map_err(|_| "Cannot encode OAuth credentials")?,
            );
            with_vault_key(&self.server, |key| {
                self.server.with_global_store(|store| {
                    let transaction = store
                        .begin_vault_transaction()
                        .map_err(|_| "Cannot write OAuth Vault entry")?;
                    let updated = update(&transaction)?;
                    let existing = transaction
                        .vault_get_entry(&self.name)
                        .map_err(|_| "Cannot read OAuth Vault entry")?;
                    if let Some(entry) = &existing {
                        self.authorize(entry, consumer.as_deref())?;
                    }
                    let (encrypted_value, nonce) = crypto::encrypt(key, plaintext.as_bytes())
                        .map_err(|_| "Cannot encrypt OAuth credentials")?;
                    let now = chrono::Utc::now().to_rfc3339();
                    let entry = VaultEntry {
                        name: self.name.clone(),
                        encrypted_value,
                        nonce,
                        secret_type: SECRET_TYPE_OAUTH_TOKEN.into(),
                        description: "MCP OAuth credentials".into(),
                        allowed_agents: existing
                            .as_ref()
                            .and_then(|entry| entry.allowed_agents.clone()),
                        created_at: existing
                            .as_ref()
                            .map(|entry| entry.created_at.clone())
                            .unwrap_or_else(|| now.clone()),
                        updated_at: now,
                        accessed_at: String::new(),
                        access_count: 0,
                    };
                    transaction
                        .vault_upsert_entry(&entry)
                        .map_err(|_| "Cannot write OAuth Vault entry")?;
                    transaction
                        .commit()
                        .map_err(|_| "Cannot commit OAuth Vault credentials")
                        .map_err(str::to_string)?;
                    Ok(updated)
                })
            })
        })();
        self.audited("mcp_oauth_save", result)
    }

    fn audited<T>(&self, operation: &str, result: Result<T, String>) -> Result<T, AuthError> {
        let audit = record_vault_audit(
            &self.server,
            operation,
            Some(&self.name),
            result.is_ok(),
            result
                .as_ref()
                .err()
                .map(|_| "OAuth credential operation failed"),
        );
        let value = result.map_err(AuthError::CredentialStoreError)?;
        audit.map_err(|_| {
            AuthError::CredentialStoreError("Cannot record OAuth Vault audit".into())
        })?;
        Ok(value)
    }
}

#[async_trait::async_trait]
impl<C: VaultCtx + Send + Sync> CredentialStore for OAuthCredentialStore<C> {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let result = (|| {
            let consumer = resolve_vault_acl_agent_id(&self.server, self.consumer.as_deref())?;
            with_vault_key(&self.server, |key| {
                self.server.with_global_store(|store| {
                    let transaction = store
                        .begin_vault_transaction()
                        .map_err(|_| "Cannot read OAuth Vault entry")?;
                    let Some(entry) = transaction
                        .vault_get_entry(&self.name)
                        .map_err(|_| "Cannot read OAuth Vault entry")?
                    else {
                        return Ok(None);
                    };
                    self.authorize(&entry, consumer.as_deref())?;
                    let mut bytes = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)
                        .map_err(|_| "Cannot decrypt OAuth Vault entry")?;
                    let credentials = serde_json::from_slice::<StoredMcpOAuthCredentials>(&bytes)
                        .map_err(|_| "Invalid OAuth Vault credentials".to_string());
                    crypto::zero_bytes(&mut bytes);
                    let credentials = credentials?;
                    if credentials.resource != self.resource {
                        return Ok(None);
                    }
                    transaction
                        .vault_touch_entry(&self.name)
                        .map_err(|_| "Cannot record OAuth Vault access")?;
                    transaction
                        .commit()
                        .map_err(|_| "Cannot commit OAuth Vault access")?;
                    Ok(Some(credentials.credentials))
                })
            })
        })();
        self.audited("mcp_oauth_load", result)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        self.save_with(credentials, |_| Ok(()))
    }

    async fn clear(&self) -> Result<(), AuthError> {
        let result = (|| {
            let consumer = resolve_vault_acl_agent_id(&self.server, self.consumer.as_deref())?;
            with_vault_key(&self.server, |key| {
                self.server.with_global_store(|store| {
                    let transaction = store
                        .begin_vault_transaction()
                        .map_err(|_| "Cannot clear OAuth Vault entry")?;
                    if let Some(entry) = transaction
                        .vault_get_entry(&self.name)
                        .map_err(|_| "Cannot read OAuth Vault entry")?
                    {
                        self.authorize(&entry, consumer.as_deref())?;
                        let mut bytes = crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)
                            .map_err(|_| "Cannot decrypt OAuth Vault entry")?;
                        let stored = serde_json::from_slice::<StoredMcpOAuthCredentials>(&bytes)
                            .map_err(|_| "Invalid OAuth Vault credentials".to_string());
                        crypto::zero_bytes(&mut bytes);
                        if stored?.resource != self.resource {
                            return Ok(());
                        }
                        transaction
                            .vault_delete_entry(&self.name)
                            .map_err(|_| "Cannot clear OAuth Vault entry")?;
                    }
                    transaction
                        .commit()
                        .map_err(|_| "Cannot commit OAuth Vault removal".to_string())
                })
            })
        })();
        self.audited("mcp_oauth_clear", result)
    }

    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        let db = self.server.global_db_path_buf();
        let db = db.canonicalize().map_err(|_| {
            AuthError::CredentialStoreError("Cannot locate OAuth Vault database".into())
        })?;
        let mut path = db.as_os_str().to_os_string();
        path.push(format!(".oauth-{}.lock", self.name));
        let file = tokio::task::spawn_blocking(move || {
            let path = std::path::PathBuf::from(path);
            if std::fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err("OAuth credential lock must not be a symlink");
            }
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let file = options
                .open(path)
                .map_err(|_| "Cannot open OAuth credential lock")?;
            let metadata = file
                .metadata()
                .map_err(|_| "Cannot inspect OAuth credential lock")?;
            if !metadata.is_file() {
                return Err("OAuth credential lock must be a regular file");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                // SAFETY: geteuid has no memory safety preconditions.
                if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                    return Err("OAuth credential lock must be owner-only");
                }
            }
            // The stable lock inode is shared by CLI login and daemon refresh. Never unlink it.
            file.lock().map_err(|_| "Cannot lock OAuth credentials")?;
            Ok(file)
        })
        .await
        .map_err(|_| AuthError::CredentialStoreError("OAuth credential lock task failed".into()))?
        .map_err(|error| AuthError::CredentialStoreError(error.into()))?;
        Ok(Some(CredentialRefreshGuard::new(file)))
    }
}

#[cfg(test)]
mod tests;

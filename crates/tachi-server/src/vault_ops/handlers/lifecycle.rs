use super::*;

pub(crate) async fn handle_vault_init(
    server: &MemoryServer,
    params: VaultInitParams,
) -> Result<String, String> {
    let result = (|| {
        if is_vault_initialized(server)? {
            return Err("Vault already initialized. Use vault_unlock to unlock.".into());
        }

        if params.password.len() < 8 {
            return Err("Password must be at least 8 characters long.".into());
        }

        let salt = crypto::generate_salt();
        let salt_b64 = B64.encode(salt);
        let key = crypto::DerivedVaultKey::derive(&params.password, &salt)?;
        let verifier = crypto::create_verifier(key.bytes())?;
        let now = Utc::now().to_rfc3339();
        let config = VaultConfig {
            salt: salt_b64,
            verifier,
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: crypto::active_kdf_params_json().to_string(),
            cipher: VaultCipher::Aes256Gcm,
            created_at: now.clone(),
            updated_at: now,
        };

        server
            .with_global_store(|store| store.vault_set_config(&config).map_err(|e| e.to_string()))
            .map_err(|e| format!("Failed to save vault config: {e}"))?;

        {
            let mut v = server.vault_write();
            v.key = Some(CachedVaultKey::copy_from(key.bytes()));
            v.unlock_time = Some(Instant::now());
            v.failed_attempts = (0, None);
        }

        serde_json::to_string(&json!({
            "initialized": true,
            "locked": false,
            "message": "Vault initialized and unlocked"
        }))
        .map_err(|e| format!("serialize: {e}"))
    })();

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));
    if result.is_ok() {
        server.requeue_auth_failed_enrichment_retries("vault init");
    }

    let audit_result = record_vault_audit(
        server,
        "vault_init",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_unlock(
    server: &MemoryServer,
    params: VaultUnlockParams,
) -> Result<String, String> {
    let result = async {
        ensure_vault_unlock_allowed(server)?;

        let config = server
            .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))
            .map_err(|e| format!("Failed to load vault config: {e}"))?
            .ok_or_else(|| "Vault not initialized. Call vault_init first.".to_string())?;

        let salt = B64
            .decode(&config.salt)
            .map_err(|e| format!("Invalid salt in vault config: {e}"))?;
        let fifo_path = params
            .password_fifo_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if fifo_path.is_some() && !params.password.is_empty() {
            return Err(
                "vault_unlock accepts either password or password_fifo_path, not both".to_string(),
            );
        }
        let mut fifo_password = match fifo_path {
            Some(path) => Some(
                tokio::task::spawn_blocking(move || read_unlock_password_fifo(&path))
                    .await
                    .map_err(|e| format!("unlock FIFO reader task failed: {e}"))??,
            ),
            None => None,
        };
        let password = match fifo_password.as_deref() {
            Some(password) => password,
            None => {
                if params.password.is_empty() {
                    return Err("vault_unlock requires password or password_fifo_path".to_string());
                }
                &params.password
            }
        };
        // tachi#1080: derive using the STORED `vault_config.kdf_params`, not a
        // compile-time constant. A malformed or unsupported stored value fails
        // loud and versioned here — before `verify_password` — so it is never
        // misread as "wrong password" and never counts against the lockout
        // counter (mirrors the corrupted-verifier discipline).
        let key_result = match crypto::parse_stored_kdf_params(&config.kdf_params) {
            Ok(params) => crypto::DerivedVaultKey::derive_with_params(password, &salt, &params)
                .map_err(|e| e.to_string()),
            Err(err) => Err(err),
        };
        let key = match key_result {
            Ok(key) => key,
            Err(err) => {
                if let Some(password) = fifo_password.as_mut() {
                    crypto::zero_string(password);
                }
                return Err(err);
            }
        };
        if let Some(password) = fifo_password.as_mut() {
            crypto::zero_string(password);
        }

        if !crypto::verify_password(key.bytes(), &config.verifier)? {
            return record_vault_unlock_failure(server);
        }

        {
            let mut v = server.vault_write();
            v.key = Some(CachedVaultKey::copy_from(key.bytes()));
            v.unlock_time = Some(Instant::now());
            v.failed_attempts = (0, None);
        }

        serde_json::to_string(&json!({
            "unlocked": true
        }))
        .map_err(|e| format!("serialize: {e}"))
    }
    .await;

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));
    if result.is_ok() {
        server.requeue_auth_failed_enrichment_retries("vault unlock");
    }

    let audit_result = record_vault_audit(
        server,
        "vault_unlock",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_lock(server: &MemoryServer) -> Result<String, String> {
    let result = {
        clear_cached_vault_state(server);
        serde_json::to_string(&json!({
            "locked": true
        }))
        .map_err(|e| format!("serialize: {e}"))
    };

    let audit_result = record_vault_audit(
        server,
        "vault_lock",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result_with_vault_audit_warning(result, audit_result)
}

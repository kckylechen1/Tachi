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

        let fifo_path = params
            .password_fifo_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let sources_given = [
            fifo_path.is_some(),
            !params.password.is_empty(),
            params.use_keychain,
        ]
        .into_iter()
        .filter(|given| *given)
        .count();
        if sources_given > 1 {
            return Err(
                "vault_unlock accepts exactly one of password, password_fifo_path, or use_keychain"
                    .to_string(),
            );
        }
        let mut fifo_password = match fifo_path {
            Some(path) => {
                let home = server.tachi_home_dir();
                Some(
                    tokio::task::spawn_blocking(move || read_unlock_password_fifo(&home, &path))
                        .await
                        .map_err(|e| format!("unlock FIFO reader task failed: {e}"))??,
                )
            }
            None => None,
        };
        // tachi#1175: same low-level Keychain-read primitive as the CLI's
        // `tachi vault unlock --keychain` (`crypto::read_password_from_macos_keychain`,
        // shared with `vault_cli::read_vault_password`) — so an agent can
        // unlock without the password ever appearing in the tool call
        // arguments or shell history. `spawn_blocking` because it shells out
        // to `security`, mirroring the FIFO reader above.
        let mut keychain_password = if params.use_keychain {
            Some(
                tokio::task::spawn_blocking(crypto::read_password_from_macos_keychain)
                    .await
                    .map_err(|e| format!("Keychain reader task failed: {e}"))?
                    .map_err(|e| format!("use_keychain unlock failed: {e}"))?,
            )
        } else {
            None
        };
        let password = match (fifo_password.as_deref(), keychain_password.as_deref()) {
            (Some(password), None) => password,
            (None, Some(password)) => password,
            (Some(_), Some(_)) => {
                unreachable!("sources_given check above rejects fifo + keychain together")
            }
            (None, None) => {
                if params.password.is_empty() {
                    return Err(
                        "vault_unlock requires one of password, password_fifo_path, or use_keychain"
                            .to_string(),
                    );
                }
                &params.password
            }
        };
        // tachi#1080/#1187 fix-round (attack-pass finding A1; leader
        // adjudication, Option B): derive+verify through the shared seam
        // `crypto::derive_verified_key_from_stored_config`, which checks the
        // stored `kdf_algorithm` fail-closed BEFORE parsing `kdf_params` or
        // deriving anything — not by re-implementing parse+derive+verify
        // inline. Pre-fix this handler never read `config.kdf_algorithm` at
        // all, so algorithm-axis drift (a fork writing a different KDF with
        // the same `{m,t,p}` param shape) derived with Argon2id anyway,
        // `verify_password` returned `Ok(false)`, and this handler matched
        // that as a plain wrong-password miss — feeding
        // `record_vault_unlock_failure`'s brute-force lockout counter for a
        // config-integrity problem, not a password problem. Only the seam's
        // `WrongPassword` variant reaches the lockout counter below; every
        // other variant (algorithm mismatch, kdf_params format, invalid
        // salt, corrupt verifier) returns before it, exactly as the seam's
        // own doc comment and discrimination tests require.
        let key = match crypto::derive_verified_key_from_stored_config(&config, password) {
            Ok(key) => key,
            Err(crypto::StoredVaultKeyDerivationError::WrongPassword) => {
                if let Some(password) = fifo_password.as_mut() {
                    crypto::zero_string(password);
                }
                if let Some(password) = keychain_password.as_mut() {
                    crypto::zero_string(password);
                }
                return record_vault_unlock_failure(server);
            }
            Err(err) => {
                if let Some(password) = fifo_password.as_mut() {
                    crypto::zero_string(password);
                }
                if let Some(password) = keychain_password.as_mut() {
                    crypto::zero_string(password);
                }
                return Err(err.to_string());
            }
        };
        if let Some(password) = fifo_password.as_mut() {
            crypto::zero_string(password);
        }
        if let Some(password) = keychain_password.as_mut() {
            crypto::zero_string(password);
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
    let result = clear_cached_vault_state(server).and_then(|()| {
        serde_json::to_string(&json!({
            "locked": true
        }))
        .map_err(|e| format!("serialize: {e}"))
    });

    let audit_result = record_vault_audit(
        server,
        "vault_lock",
        None,
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result_with_vault_audit_warning(result, audit_result)
}

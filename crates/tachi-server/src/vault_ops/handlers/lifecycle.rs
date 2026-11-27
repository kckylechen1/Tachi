use super::*;

pub(crate) async fn handle_vault_unlock(
    server: &(impl VaultCtx + Clone + Send + 'static),
    params: VaultUnlockParams,
) -> Result<String, String> {
    let result = async {
        ensure_vault_unlock_allowed(server)?;

        let config = server
            .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))
            .map_err(|e| format!("Failed to load vault config: {e}"))?
            .ok_or_else(|| "Vault not initialized. Run `tachi vault init` first.".to_string())?;

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
        let mut password = match (fifo_password.take(), keychain_password.take()) {
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
                params.password.clone()
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
        // Only serialize the CPU-heavy KDF and lockout-state update after any
        // interactive FIFO or Keychain password read completes.
        let turn = UNLOCK_ADMISSION.lock().await;
        admit_unlock_turn(server, &mut password)?;

        // The KDF is deliberately slow. It runs off the async worker threads;
        // the password travels with it and is zeroed there. The turn travels
        // with it too, and a miss is recorded where the derivation ends: a
        // caller that goes away mid-derivation neither frees the turn for a
        // second derivation nor loses the failure.
        let accounting = server.clone();
        let key = match tokio::task::spawn_blocking(move || {
            let _turn = turn;
            let mut password = password;
            let derived = crypto::derive_verified_key_from_stored_config(&config, &password);
            crypto::zero_string(&mut password);
            match derived {
                Ok(key) => Ok(key),
                Err(err @ crypto::StoredVaultKeyDerivationError::WrongPassword) => {
                    // The recorder always refuses; its message names a new lockout.
                    match record_vault_unlock_failure(&accounting) {
                        Err(recorded) => Err(recorded),
                        Ok(_) => Err(err.to_string()),
                    }
                }
                Err(err) => Err(err.to_string()),
            }
        })
        .await
        {
            Ok(Ok(key)) => key,
            Ok(Err(err)) => return Err(err),
            Err(err) => return Err(format!("vault unlock derivation task failed: {err}")),
        };

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

pub(crate) async fn handle_vault_lock(server: &impl VaultCtx) -> Result<String, String> {
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

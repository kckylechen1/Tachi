use super::keys::decrypt_profile_secret_values;
use crate::bootstrap::{open_cli_store, open_cli_store_read_only};
use std::path::PathBuf;
use tachi_bootstrap::cli::VaultAction;

fn validate_provider_doctor_password_sources(
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&std::path::Path>,
) -> Result<(), String> {
    let selected =
        usize::from(stdin_password) + usize::from(keychain) + usize::from(password_file.is_some());
    if selected > 1 {
        return Err(
            "provider doctor accepts exactly one password source: --stdin-password, --keychain, or --password-file"
                .to_string(),
        );
    }
    Ok(())
}

pub(super) fn run_credential_action(
    global_db_path: &PathBuf,
    action: VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        VaultAction::Materialize {
            profile,
            consumer,
            config,
            dry_run: _,
            apply,
            allow_existing,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let (config_path, profile_def) = if let Some(path) = config {
                let profile_def =
                    tachi_credential_profile::load_credential_profile_from_path(&path, &profile)?;
                (path, profile_def)
            } else {
                tachi_credential_profile::find_credential_profile(
                    &tachi_credential_profile::default_credentials_dir(),
                    &profile,
                )?
            };

            let store = if apply {
                open_cli_store(global_db_path)?
            } else {
                open_cli_store_read_only(global_db_path)?
            };
            let report = tachi_credential_profile::plan_credential_materialization(
                &profile,
                &profile_def,
                &consumer,
                &store,
            )?;
            let mut body = if apply {
                let mut secret_values = decrypt_profile_secret_values(
                    global_db_path,
                    &profile_def,
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                    insecure_password_file,
                )?;
                let result = tachi_credential_profile::apply_credential_materialization(
                    &profile,
                    &profile_def,
                    &consumer,
                    &store,
                    &secret_values,
                    &tachi_credential_profile::CredentialApplyOptions {
                        allow_existing,
                        run_dir: None,
                    },
                );
                for value in secret_values.values_mut() {
                    crate::vault_crypto::zero_string(value);
                }
                let result = result?;
                let mut value =
                    tachi_credential_profile::credential_materialize_report_json(&result.report);
                value["env_outputs"] = serde_json::json!(result
                    .env
                    .keys()
                    .map(|key| format!("env:{key}"))
                    .collect::<Vec<_>>());
                value
            } else {
                tachi_credential_profile::credential_materialize_report_json(&report)
            };
            body["config_path"] = serde_json::json!(config_path.to_string_lossy());
            println!("{}", serde_json::to_string_pretty(&body)?);
            Ok(())
        }
        VaultAction::Cleanup {
            run_dir,
            profile,
            consumer,
            dry_run: _,
            apply,
            mark_only,
        } => {
            let store = if apply {
                open_cli_store(global_db_path)?
            } else {
                open_cli_store_read_only(global_db_path)?
            };
            let report = tachi_credential_profile::cleanup_managed_credential_materializations(
                &store,
                &tachi_credential_profile::CredentialCleanupOptions {
                    run_dir,
                    profile,
                    consumer,
                    dry_run: !apply,
                    mark_only,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        VaultAction::Doctor {
            providers,
            opencode_config,
            profile,
            consumer,
            config,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            if providers {
                validate_provider_doctor_password_sources(
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                )?;
                let store = open_cli_store_read_only(global_db_path)?;
                let compare_values = stdin_password || keychain || password_file.is_some();
                let vault_key = if compare_values {
                    let vault_config = store
                        .vault_get_config()
                        .map_err(|e| format!("vault_get_config: {e}"))?
                        .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
                    Some(super::keys::read_verified_vault_key(
                        &vault_config,
                        stdin_password,
                        keychain,
                        password_file.as_deref(),
                        insecure_password_file,
                    )?)
                } else {
                    None
                };
                return super::providers_doctor::run_providers_doctor(
                    &store,
                    opencode_config,
                    vault_key.as_ref().map(|key| key.bytes()),
                );
            }
            let profile = profile.ok_or_else(|| {
                "`tachi vault doctor` requires --profile (unless --providers)".to_string()
            })?;
            let consumer = consumer.ok_or_else(|| {
                "`tachi vault doctor` requires --consumer (unless --providers)".to_string()
            })?;
            let (config_path, profile_def) = if let Some(path) = config {
                let profile_def =
                    tachi_credential_profile::load_credential_profile_from_path(&path, &profile)?;
                (path, profile_def)
            } else {
                tachi_credential_profile::find_credential_profile(
                    &tachi_credential_profile::default_credentials_dir(),
                    &profile,
                )?
            };
            let store = open_cli_store_read_only(global_db_path)?;
            let mut body = serde_json::json!(tachi_credential_profile::doctor_credential_profile(
                &profile,
                &profile_def,
                &consumer,
                &store,
            )?);
            body["config_path"] = serde_json::json!(config_path.to_string_lossy());
            println!("{}", serde_json::to_string_pretty(&body)?);
            Ok(())
        }
        _ => unreachable!("credential action router received non-credential action"),
    }
}

#[cfg(test)]
mod tests {
    use super::validate_provider_doctor_password_sources;
    use std::path::Path;

    #[test]
    fn provider_doctor_runtime_rejects_multiple_password_sources() {
        let err = validate_provider_doctor_password_sources(
            true,
            false,
            Some(Path::new("fixture-password")),
        )
        .expect_err("runtime defense must reject ambiguous password sources");
        assert!(err.contains("exactly one password source"), "{err}");
    }
}

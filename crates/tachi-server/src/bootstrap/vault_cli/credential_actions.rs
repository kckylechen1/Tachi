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
            run_dir,
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
            let report = tachi_credential_profile::plan_credential_materialization_with_run_dir(
                &profile,
                &profile_def,
                &consumer,
                &store,
                run_dir.as_deref(),
            )?;
            let mut body = if apply {
                report.ensure_ready()?;
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
                        run_dir,
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
            compare,
            opencode_config,
            profile,
            consumer,
            config,
            run_dir,
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
                let compare_values =
                    compare || stdin_password || keychain || password_file.is_some();
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
            if profile.is_none() && consumer.is_none() {
                return run_default_vault_doctor(global_db_path);
            }
            let profile = profile.ok_or_else(|| {
                "`tachi vault doctor` profile mode requires both --profile and --consumer"
                    .to_string()
            })?;
            let consumer = consumer.ok_or_else(|| {
                "`tachi vault doctor` profile mode requires both --profile and --consumer"
                    .to_string()
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
                run_dir.as_deref(),
            )?);
            body["config_path"] = serde_json::json!(config_path.to_string_lossy());
            println!("{}", serde_json::to_string_pretty(&body)?);
            Ok(())
        }
        _ => unreachable!("credential action router received non-credential action"),
    }
}

fn run_default_vault_doctor(global_db_path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    println!("Tachi Vault & Project Credential Doctor:");
    println!("  vault_db: {}", global_db_path.display());

    if !global_db_path.exists() {
        println!("  initialized: false");
        println!("  next_step: run `tachi vault init` to initialize the Vault");
        return Ok(());
    }

    let store = open_cli_store_read_only(global_db_path)?;
    let initialized = store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .is_some();
    println!("  initialized: {initialized}");
    if !initialized {
        println!("  next_step: run `tachi vault init` to initialize the Vault");
        return Ok(());
    }

    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?;
    let rotations = store
        .vault_list_rotations()
        .map_err(|e| format!("vault_list_rotations: {e}"))?;
    println!(
        "  stored_secrets: {} (rotation_pools: {})",
        entries.len(),
        rotations.len()
    );

    if cfg!(target_os = "macos") {
        let keychain_status = match crate::vault_crypto::read_password_from_macos_keychain() {
            Ok(mut pw) => {
                crate::vault_crypto::zero_string(&mut pw);
                "available (auto-unlock active)"
            }
            Err(err) if err.starts_with("no vault password found in Keychain") => "not configured",
            Err(_) => "unavailable",
        };
        println!("  macos_keychain: {keychain_status}");
    }

    let cwd = std::env::current_dir()?;
    let plan = crate::bootstrap::env_cmd::build_project_env_plan(&store, &cwd)?;
    println!("  project_cwd: {}", plan.cwd);
    match plan.bindings_path.as_deref() {
        Some(path) => {
            println!(
                "  project_bindings: {} ({} bound, {} missing)",
                path, plan.binding_count, plan.missing_count
            );
            for b in &plan.bindings {
                if b.status != "ok" {
                    println!("    ! {} <= vault:{} [missing]", b.env_name, b.secret_name);
                }
            }
        }
        None => {
            println!("  project_bindings: none (.tachi/vault.env not found)");
        }
    }

    let creds_file = cwd.join(".env.credentials");
    let mut unsealed_keys: Vec<String> = Vec::new();
    if creds_file.is_file() {
        if let Ok(raw) = std::fs::read_to_string(&creds_file) {
            for raw_line in raw.lines() {
                let line = raw_line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let line = line.strip_prefix("export ").unwrap_or(line).trim();
                let Some((k, v)) = line.split_once('=') else {
                    continue;
                };
                let key = k.trim();
                let val = v
                    .split(" #")
                    .next()
                    .unwrap_or(v)
                    .trim()
                    .trim_matches(['"', '\''])
                    .trim();
                if !key.is_empty()
                    && !val.is_empty()
                    && !(key.ends_with("_COUNT") && val.chars().all(|c| c.is_ascii_digit()))
                    && crate::provider_config::parse_vault_alias(val).is_none()
                    && !val
                        .get(..6)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("vault:"))
                {
                    unsealed_keys.push(key.to_string());
                }
            }
        }
    }

    if !unsealed_keys.is_empty() {
        println!(
            "  unsealed_credentials: {} plaintext key(s) in .env.credentials ({})",
            unsealed_keys.len(),
            unsealed_keys.join(", ")
        );
        println!(
            "  next_step: run `tachi env seal .env.credentials` to move plaintext keys into Vault"
        );
    } else if plan.missing_count > 0 {
        if let Some(first_missing) = plan.bindings.iter().find(|b| b.status != "ok") {
            println!(
                "  next_step: run `tachi env set {}` to store the missing secret",
                first_missing.env_name
            );
        }
    } else {
        println!("  status: OK (use `tachi env set <KEY>` to add a new project secret, or `tachi vault doctor --providers` for OpenCode diagnostics)");
    }
    Ok(())
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

use super::keys::{read_vault_config_for_key, read_verified_vault_key, run_vault_setup_keys};
use crate::bootstrap::vault_sync;
use std::path::PathBuf;
use tachi_bootstrap::cli::VaultAction;

pub(super) fn run_sync_action(
    global_db_path: &PathBuf,
    action: VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        VaultAction::SyncStatus { path } => {
            let path = vault_sync::resolve_vault_sync_path(path)?;
            let status = vault_sync::vault_sync_status(&path)?;
            vault_sync::print_status(&status);
            Ok(())
        }
        VaultAction::SetupKeys {
            stdin_password,
            keychain,
            password_file,
            confirm_password_file,
            insecure_password_file,
            include_deprecated,
        } => run_vault_setup_keys(
            global_db_path,
            stdin_password,
            keychain,
            password_file.as_deref(),
            confirm_password_file.as_deref(),
            insecure_password_file,
            include_deprecated,
        ),
        VaultAction::SyncExport {
            output,
            allow_cloud,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let output = vault_sync::resolve_vault_sync_path(output)?;
            let config = read_vault_config_for_key(global_db_path)?;
            let key = read_verified_vault_key(
                &config,
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;
            let status =
                vault_sync::export_vault_bundle(global_db_path, &output, allow_cloud, key.bytes())?;
            println!("Vault sync export complete.");
            vault_sync::print_status(&status);
            println!(
                "  contents: signed Vault ciphertext, verifier material, and key-rotation metadata"
            );
            println!(
                "  risk: possession of this bundle permits offline password guessing; keep it local or accept cloud-sync risk explicitly"
            );
            Ok(())
        }
        VaultAction::SyncImport {
            input,
            allow_unsigned,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let input = vault_sync::resolve_vault_sync_path(input)?;
            let key = if allow_unsigned && !vault_sync::bundle_has_signature(&input)? {
                None
            } else {
                let config = vault_sync::read_bundle_vault_config(&input)?;
                Some(read_verified_vault_key(
                    &config,
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                    insecure_password_file,
                )?)
            };
            let report = vault_sync::import_vault_bundle(
                global_db_path,
                &input,
                key.as_ref().map(|key| key.bytes()),
                allow_unsigned,
            )?;
            println!("Vault sync import complete.");
            println!("  path: {}", report.path);
            println!("  initialized_vault: {}", report.initialized_vault);
            println!("  entries_imported: {}", report.entries_imported);
            println!("  rotations_imported: {}", report.rotations_imported);
            println!("  note: import only upserts encrypted rows; it does not delete local extras");
            Ok(())
        }
        _ => unreachable!("sync action router received non-sync action"),
    }
}

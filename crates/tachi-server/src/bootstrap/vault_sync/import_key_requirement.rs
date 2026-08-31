use super::{validate_bundle, VaultSyncBundle};
use crate::bootstrap::open_cli_store_read_only;
use std::path::Path;

/// Decide whether an explicitly unsigned import needs a verified Vault key
/// before the CLI wrapper calls the validating import path. This is a
/// metadata-only read: it never decrypts bundle or local entry ciphertext.
///
/// The lower import path remains authoritative. This preflight only avoids an
/// unnecessary password/Keychain read for untracked opaque entries while
/// preserving the key requirement for lane bindings, new lane URLs, and
/// entries that currently own durable account custody.
pub(in crate::bootstrap) fn unsigned_import_requires_vault_key(
    global_db_path: &Path,
    input: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(input)
        .map_err(|e| format!("read sync bundle {}: {e}", input.display()))?;
    let bundle: VaultSyncBundle = serde_json::from_str(&raw)
        .map_err(|e| format!("parse sync bundle {}: {e}", input.display()))?;
    validate_bundle(&bundle)?;

    if bundle
        .entries
        .iter()
        .any(|entry| crate::vault_ops::is_lane_slot_secret_name(&entry.name))
    {
        return Ok(true);
    }

    // A new lane URL is checked for embedded credentials by the validating
    // import path, so it needs the key even when it is not account-bound. A
    // missing target DB means every incoming URL is new; no store open is
    // needed for the common fresh-target case.
    if !global_db_path.exists() {
        return Ok(bundle
            .entries
            .iter()
            .any(|entry| memcore::is_lane_config_url_name(&entry.name)));
    }

    let store = open_cli_store_read_only(&global_db_path.to_path_buf())?;
    let transaction = store
        .begin_vault_read_transaction_shared()
        .map_err(|e| format!("begin vault import classification snapshot: {e}"))?;
    for entry in &bundle.entries {
        if memcore::is_lane_config_url_name(&entry.name)
            && !transaction
                .vault_entry_exists(&entry.name)
                .map_err(|e| format!("read imported lane URL metadata: {e}"))?
        {
            return Ok(true);
        }
        if transaction
            .vault_account_entry_is_tracked(&entry.name)
            .map_err(|e| format!("read imported account custody: {e}"))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

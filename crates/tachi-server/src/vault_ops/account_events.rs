//! Bind the existing account ledger to Vault mutations, never to response text.

use memcore::store::vault::VaultTransaction;
use memcore::vault::fingerprint::FingerprintKey;

pub(crate) fn prepare_entry_removal(
    transaction: &VaultTransaction<'_>,
    key: &[u8; 32],
    name: &str,
) -> Result<(), String> {
    transaction
        .vault_prepare_account_entry_removal(name)
        .map_err(|error| format!("protect provider account custody: {error}"))?;
    if super::is_lane_slot_secret_name(name)
        || crate::status_ops::status_health::account_class_for_env_name(name)
            != Some(memcore::AccountClass::ModelApi)
    {
        return Ok(());
    }
    // Legacy or imported pointers can predate canonical account observation.
    // Validate their actual encrypted references inside the same write lock.
    for slot in transaction
        .vault_list_entries()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|entry| super::is_lane_slot_secret_name(&entry.name))
    {
        let value =
            crate::vault_crypto::ZeroizingString::new(crate::vault_crypto::decode_utf8_zeroizing(
                crate::vault_crypto::decrypt(key, &slot.encrypted_value, &slot.nonce).map_err(
                    |error| {
                        format!(
                            "cannot verify lane-slot references before account deletion: {error}"
                        )
                    },
                )?,
                "cannot verify non-UTF8 lane-slot references before account deletion",
            )?);
        if crate::provider_config::parse_vault_alias(&value) == Some(name) {
            return Err(format!(
                "Vault account '{name}' is referenced by lane slot '{}'; rebind or remove the slot before deleting its target",
                slot.name
            ));
        }
    }
    Ok(())
}

pub(crate) fn observe_account_entry(
    transaction: &VaultTransaction<'_>,
    key: &[u8; 32],
    name: &str,
    secret_type: &str,
    value: &str,
    create_if_missing: bool,
) -> Result<Option<memcore::ProviderAccount>, String> {
    if super::is_lane_slot_secret_name(name)
        || memcore::effective_vault_secret_type(name, secret_type) != memcore::SECRET_TYPE_API_KEY
        || crate::status_ops::status_health::account_class_for_env_name(name)
            != Some(memcore::AccountClass::ModelApi)
        || crate::provider_config::parse_vault_alias(value).is_some()
        || value.trim().is_empty()
    {
        if transaction
            .vault_account_entry_is_tracked(name)
            .map_err(|error| format!("read provider account custody: {error}"))?
        {
            return Err(format!(
                "Vault account '{name}' owns a durable ModelApi identity; refusing a non-API-key, empty, or alias replacement that would invalidate its custody"
            ));
        }
        return Ok(None);
    }
    let kind = crate::status_ops::status_health::provider_kind_for_env_name(name)
        .ok_or("registered account has no provider kind")?;
    let fingerprint_key = FingerprintKey::derive_from_master_key(key);
    let member = fingerprint_key.key_fingerprint(kind, value.trim());
    let fingerprint = fingerprint_key.account_fingerprint_from_members([&member]);
    transaction
        .vault_observe_model_account_entry(name, kind, &fingerprint, create_if_missing)
        .map_err(|error| format!("record provider account: {error}"))
}

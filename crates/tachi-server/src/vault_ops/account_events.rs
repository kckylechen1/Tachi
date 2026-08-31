//! Bind the existing account ledger to Vault mutations, never to response text.

use memcore::store::vault::VaultTransaction;
use memcore::vault::fingerprint::FingerprintKey;

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

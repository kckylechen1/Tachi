use memcore::store::vault::VaultTransaction;
use memcore::vault::VaultEntry;

/// Account-only bundles still rotate an existing identity. Untracked opaque
/// rows keep their historical no-unlock import behavior; tracked custody may
/// not change without verified fingerprint and event evidence.
pub(super) fn record_imported_account_updates(
    transaction: &VaultTransaction<'_>,
    incoming: &[VaultEntry],
    verification_key: Option<&[u8; 32]>,
) -> Result<(), String> {
    for entry in incoming {
        if !transaction
            .vault_account_entry_is_tracked(&entry.name)
            .map_err(|error| format!("read imported account custody: {error}"))?
        {
            continue;
        }
        let key = verification_key.ok_or_else(|| {
            format!(
                "Imported account '{}' owns a durable identity; a vault password is required to record its fingerprint before replacement",
                entry.name
            )
        })?;
        let value =
            crate::vault_crypto::ZeroizingString::new(crate::vault_crypto::decode_utf8_zeroizing(
                crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?,
                format!("Imported account '{}' is not valid UTF-8", entry.name),
            )?);
        crate::vault_ops::account_events::observe_account_entry(
            transaction,
            key,
            &entry.name,
            &entry.secret_type,
            &value,
            false,
        )?
        .ok_or("imported tracked account lost its ModelApi identity")?;
    }
    Ok(())
}

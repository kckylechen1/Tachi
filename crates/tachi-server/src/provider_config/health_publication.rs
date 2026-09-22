use crate::vault_ops::VaultMaterializationRevision;

pub(super) fn fenced_provider_health_recheck(
    connection: &rusqlite::Connection,
    expected: VaultMaterializationRevision,
) -> Result<Option<Vec<memcore::vault::VaultKeyHealth>>, String> {
    let entries = memcore::db::vault_list_entries(connection)
        .map_err(|error| format!("Failed to read Vault entries for revision check: {error}"))?;
    let rotations = memcore::db::vault_list_rotations(connection)
        .map_err(|error| format!("Failed to read Vault rotations for revision check: {error}"))?;
    let mut key_health = memcore::db::vault_list_key_health(connection, None)
        .map_err(|error| format!("Failed to read Vault key health for revision check: {error}"))?;
    let actual = crate::vault_ops::vault_materialization_acl_revision_from_rows(
        &entries,
        &rotations,
        &key_health,
    );
    // A coherent already-captured epoch may publish after ciphertext-only
    // replacement. Its content generation stays old, so listing cannot attach
    // that epoch's outcomes to the new durable credentials.
    if actual.authority != expected.authority {
        return Err(
            "Vault ACL, type, rotation, or entry revision changed before publication; retry provider refresh"
                .to_string(),
        );
    }
    // A different row must not make an unchanged, older DB observation defeat
    // the memory health merged by the original scan. Compare each identity's
    // observation, not only a digest of the whole health table.
    key_health.retain(|row| {
        let identity = (row.logical_name.clone(), row.key_id.clone());
        actual.health.get(&identity) != expected.health.get(&identity)
    });
    Ok((!key_health.is_empty()).then_some(key_health))
}

pub(super) fn validate_admitted_provider_health(
    snapshot: &tachi_llm::ProviderMaterializationSnapshot,
    health_recheck: Option<&[memcore::vault::VaultKeyHealth]>,
) -> Result<(), String> {
    let now = chrono::Utc::now();
    if health_recheck.is_some_and(|rows| {
        rows.iter().any(|row| {
            snapshot.uses_health_identity(&row.logical_name, &row.key_id)
                && crate::vault_ops::unusable_skip_class(row, now).is_some()
        })
    }) {
        return Err(
            "Vault health revision changed before publication and an admitted credential is unusable; retry provider refresh"
                .to_string(),
        );
    }
    // Recovery of a dropped row cannot add it to this snapshot or rewrite its
    // scan-time drop reason. Unrelated health writes cannot reject the refresh.
    Ok(())
}

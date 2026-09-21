use memcore::vault::{VaultEntry, VaultKeyHealth, VaultKeyRotation};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Both observations come from the transaction that supplied the plaintext.
/// Authority metadata must still match at publication. Credentials and lane
/// values publish as one captured generation; health drift requires a check
/// of only the credentials that generation would publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VaultMaterializationRevision {
    /// Publication authority retains the ACL/type/rotation/revision contract.
    pub authority: u64,
    /// Observation provenance also binds the exact captured encrypted bytes.
    pub contents: u64,
    pub health: HashMap<(String, String), u64>,
}

fn vault_key_health_revision(row: &VaultKeyHealth) -> u64 {
    let mut observation = std::collections::hash_map::DefaultHasher::new();
    row.status.hash(&mut observation);
    row.cooldown_until.hash(&mut observation);
    row.last_success.hash(&mut observation);
    row.last_attempt.hash(&mut observation);
    row.last_error.hash(&mut observation);
    row.error_count.hash(&mut observation);
    row.auth_failed.hash(&mut observation);
    row.disabled.hash(&mut observation);
    row.metadata.hash(&mut observation);
    row.updated_at.hash(&mut observation);
    observation.finish()
}

pub(crate) fn vault_materialization_acl_revision_from_rows(
    entries: &[VaultEntry],
    rotations: &[VaultKeyRotation],
    key_health: &[VaultKeyHealth],
) -> VaultMaterializationRevision {
    let mut entries = entries.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let mut rotations = rotations.iter().collect::<Vec<_>>();
    rotations.sort_by(|left, right| left.prefix.cmp(&right.prefix));

    let mut authority = std::collections::hash_map::DefaultHasher::new();
    let mut contents = std::collections::hash_map::DefaultHasher::new();
    entries.len().hash(&mut authority);
    for entry in entries {
        entry.name.hash(&mut authority);
        entry.secret_type.hash(&mut authority);
        entry.allowed_agents.hash(&mut authority);
        entry.updated_at.hash(&mut authority);
        // The database timestamp has millisecond precision. A replacement can
        // retain every metadata field while changing credentials in that tick.
        // Bind the opaque internal revision to stored ciphertext, without
        // decrypting it or exposing ciphertext, nonce, or revision in listing.
        entry.encrypted_value.hash(&mut contents);
        entry.nonce.hash(&mut contents);
    }
    rotations.len().hash(&mut authority);
    for rotation in rotations {
        rotation.prefix.hash(&mut authority);
        rotation.current_index.hash(&mut authority);
        rotation.total_keys.hash(&mut authority);
        rotation.rotation_strategy.hash(&mut authority);
    }
    let authority = authority.finish();
    authority.hash(&mut contents);

    let mut health = HashMap::with_capacity(key_health.len());
    for row in key_health {
        health.insert(
            (row.logical_name.clone(), row.key_id.clone()),
            vault_key_health_revision(row),
        );
    }
    VaultMaterializationRevision {
        authority,
        contents: contents.finish(),
        health,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_generation_changes_with_ciphertext_or_nonce_at_identical_timestamp() {
        let entry = VaultEntry {
            name: "DEEPSEEK_API_KEY".into(),
            encrypted_value: "synthetic-ciphertext-a".into(),
            nonce: "synthetic-nonce-a".into(),
            secret_type: "api_key".into(),
            description: String::new(),
            allowed_agents: None,
            created_at: "2026-09-21T00:00:00.000Z".into(),
            updated_at: "2026-09-21T00:00:00.000Z".into(),
            accessed_at: String::new(),
            access_count: 0,
        };
        let revision = |row| vault_materialization_acl_revision_from_rows(&[row], &[], &[]);
        let original = revision(entry.clone());
        let mut ciphertext_changed = entry.clone();
        ciphertext_changed.encrypted_value = "synthetic-ciphertext-b".into();
        let mut nonce_changed = entry.clone();
        nonce_changed.nonce = "synthetic-nonce-b".into();
        for replacement in [ciphertext_changed, nonce_changed] {
            let changed = revision(replacement);
            assert_ne!(
                original.contents, changed.contents,
                "ciphertext or nonce replacement must change generation"
            );
            assert_eq!(
                original.authority, changed.authority,
                "captured coherent epoch may still publish"
            );
        }
        for change in ["type", "acl", "updated_at"] {
            let mut changed = entry.clone();
            match change {
                "type" => changed.secret_type = "other".into(),
                "acl" => changed.allowed_agents = Some(vec!["restricted-agent".into()]),
                _ => changed.updated_at = "2026-09-21T00:00:00.001Z".into(),
            }
            assert_ne!(
                original.authority,
                revision(changed).authority,
                "{change} remains a publication fence"
            );
        }
        let rotation = VaultKeyRotation {
            prefix: entry.name.clone(),
            current_index: 1,
            total_keys: 1,
            rotation_strategy: "round_robin".into(),
            created_at: entry.created_at.clone(),
            updated_at: entry.updated_at.clone(),
        };
        assert_ne!(
            original.authority,
            vault_materialization_acl_revision_from_rows(&[entry], &[rotation], &[]).authority
        );
    }
}

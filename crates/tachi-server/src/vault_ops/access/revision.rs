use memcore::vault::{VaultEntry, VaultKeyHealth, VaultKeyRotation};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Both observations come from the transaction that supplied the plaintext.
/// Authority metadata must still match at publication. Credentials and lane
/// values publish as one captured generation; health drift requires a check
/// of only the credentials that generation would publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VaultMaterializationRevision {
    pub contents: u64,
    pub health: HashMap<(String, String), u64>,
}

pub(crate) fn vault_key_health_revision(row: &VaultKeyHealth) -> u64 {
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

    let mut contents = std::collections::hash_map::DefaultHasher::new();
    entries.len().hash(&mut contents);
    for entry in entries {
        entry.name.hash(&mut contents);
        entry.secret_type.hash(&mut contents);
        entry.allowed_agents.hash(&mut contents);
        entry.updated_at.hash(&mut contents);
    }
    rotations.len().hash(&mut contents);
    for rotation in rotations {
        rotation.prefix.hash(&mut contents);
        rotation.current_index.hash(&mut contents);
        rotation.total_keys.hash(&mut contents);
        rotation.rotation_strategy.hash(&mut contents);
    }

    let mut health = HashMap::with_capacity(key_health.len());
    for row in key_health {
        health.insert(
            (row.logical_name.clone(), row.key_id.clone()),
            vault_key_health_revision(row),
        );
    }
    VaultMaterializationRevision {
        contents: contents.finish(),
        health,
    }
}

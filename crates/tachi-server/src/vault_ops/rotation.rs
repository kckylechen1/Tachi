use memcore::vault::VaultEntry;

pub(super) fn normalize_rotation_strategy(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "round_robin" | "round-robin" => "round_robin".to_string(),
        "random" => "random".to_string(),
        "least_recently_used" | "lru" | "least-recently-used" => "least_recently_used".to_string(),
        _ => "round_robin".to_string(),
    }
}

/// Canonical daemon grouping for configured Vault rotation members.
/// Report-only consumers reuse this so suffix parsing cannot drift from
/// provider materialization.
pub(crate) fn collect_rotation_entries(
    entries: Vec<VaultEntry>,
    prefix: &str,
) -> Vec<(usize, VaultEntry)> {
    let mut matching: Vec<(usize, VaultEntry)> = entries
        .into_iter()
        .filter_map(|entry| {
            memcore::api_key_pool_member_index(&entry.name, prefix).map(|index| (index, entry))
        })
        .collect();
    matching.sort_by_key(|(index, _)| *index);
    matching
}

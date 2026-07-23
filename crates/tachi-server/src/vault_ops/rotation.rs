use memcore::vault::VaultEntry;

pub(super) fn normalize_rotation_strategy(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "round_robin" | "round-robin" => "round_robin".to_string(),
        "random" => "random".to_string(),
        "least_recently_used" | "lru" | "least-recently-used" => "least_recently_used".to_string(),
        _ => "round_robin".to_string(),
    }
}

pub(super) fn normalize_allowed_agents(allowed_agents: Option<Vec<String>>) -> Option<Vec<String>> {
    allowed_agents.and_then(|agents| {
        let normalized: Vec<String> = agents
            .into_iter()
            .map(|agent| agent.trim().to_string())
            .filter(|agent| !agent.is_empty())
            .collect();
        if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        }
    })
}

fn rotation_index(name: &str, prefix: &str) -> Option<u32> {
    let suffix = name.strip_prefix(prefix)?.strip_prefix('_')?;
    suffix.parse::<u32>().ok()
}

/// Canonical daemon grouping for configured Vault rotation members.
/// Report-only consumers reuse this so suffix parsing cannot drift from
/// provider materialization.
pub(crate) fn collect_rotation_entries(
    entries: Vec<VaultEntry>,
    prefix: &str,
) -> Vec<(u32, VaultEntry)> {
    let mut matching: Vec<(u32, VaultEntry)> = entries
        .into_iter()
        .filter_map(|entry| rotation_index(&entry.name, prefix).map(|index| (index, entry)))
        .collect();
    matching.sort_by_key(|(index, _)| *index);
    matching
}

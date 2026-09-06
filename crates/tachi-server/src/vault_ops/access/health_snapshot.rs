use crate::server_state::MemoryServer;
use memcore::vault::VaultKeyHealth;
use std::collections::HashMap;

pub(super) fn merged_provider_key_health(
    server: &MemoryServer,
    rows: Vec<VaultKeyHealth>,
) -> HashMap<String, HashMap<String, VaultKeyHealth>> {
    let mut snapshot: HashMap<String, HashMap<String, VaultKeyHealth>> = HashMap::new();
    for row in rows {
        snapshot
            .entry(row.logical_name.clone())
            .or_default()
            .insert(row.key_id.clone(), row);
    }
    // A runtime outcome must affect both direct-account and bound-slot reads
    // even before its asynchronous persistence completes.
    for (logical_name, members) in server.llm.provider_health_memory_snapshot() {
        let target = snapshot.entry(logical_name).or_default();
        for (key_id, health) in members {
            let keep_in_memory = target
                .get(&key_id)
                .and_then(|persisted| {
                    let persisted_at =
                        chrono::DateTime::parse_from_rfc3339(&persisted.updated_at).ok()?;
                    let memory_at =
                        chrono::DateTime::parse_from_rfc3339(&health.updated_at).ok()?;
                    Some(memory_at >= persisted_at)
                })
                .unwrap_or(true);
            if keep_in_memory {
                target.insert(key_id, health);
            }
        }
    }
    snapshot
}

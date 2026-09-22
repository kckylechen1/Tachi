use crate::server_state::MemoryServer;
use memcore::vault::VaultKeyHealth;
use std::collections::HashMap;

pub(crate) fn merge_provider_key_health(
    rows: Vec<VaultKeyHealth>,
    memory: HashMap<String, HashMap<String, VaultKeyHealth>>,
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
    for (logical_name, members) in memory {
        let target = snapshot.entry(logical_name).or_default();
        for (key_id, health) in members {
            let selected = match target.get(&key_id) {
                None => health,
                Some(persisted) => reconcile_same_identity(persisted, &health),
            };
            target.insert(key_id, selected);
        }
    }
    snapshot
}

fn reconcile_same_identity(persisted: &VaultKeyHealth, memory: &VaultKeyHealth) -> VaultKeyHealth {
    let persisted_at = chrono::DateTime::parse_from_rfc3339(&persisted.updated_at).ok();
    let memory_at = chrono::DateTime::parse_from_rfc3339(&memory.updated_at).ok();
    if let (Some(persisted_at), Some(memory_at)) = (persisted_at, memory_at) {
        match memory_at.cmp(&persisted_at) {
            std::cmp::Ordering::Greater => return memory.clone(),
            std::cmp::Ordering::Less => return persisted.clone(),
            std::cmp::Ordering::Equal => {}
        }
    }

    // Equal (or unorderable) observations have no source precedence. Preserve
    // any veto; otherwise contradictory snapshots cannot certify success.
    let now = chrono::Utc::now();
    let persisted_veto = crate::vault_ops::unusable_skip_class(persisted, now).is_some();
    let memory_veto = crate::vault_ops::unusable_skip_class(memory, now).is_some();
    match (persisted_veto, memory_veto) {
        (true, false) => return persisted.clone(),
        (false, true) => return memory.clone(),
        _ => {}
    }
    let persisted_json = serde_json::to_string(persisted).expect("health row serializes");
    let memory_json = serde_json::to_string(memory).expect("health row serializes");
    if persisted_json == memory_json || persisted_veto {
        return if persisted_json <= memory_json {
            persisted.clone()
        } else {
            memory.clone()
        };
    }

    // This is a read snapshot, never a persisted outcome. No observation-time
    // generation can be attributed to two conflicting usable rows.
    VaultKeyHealth {
        logical_name: persisted.logical_name.clone(),
        key_id: persisted.key_id.clone(),
        updated_at: persisted_at.map(|at| at.to_rfc3339()).unwrap_or_default(),
        ..VaultKeyHealth::default()
    }
}

pub(super) fn merged_provider_key_health(
    server: &MemoryServer,
    rows: Vec<VaultKeyHealth>,
) -> HashMap<String, HashMap<String, VaultKeyHealth>> {
    merge_provider_key_health(rows, server.llm.provider_health_memory_snapshot())
}

use super::*;

pub(super) fn has_newly_unusable_health(
    state: &ProviderState,
    replacement: &HashMap<String, Vec<ProviderSecret>>,
    baseline: &HashMap<String, HashMap<String, VaultKeyHealth>>,
) -> bool {
    let now = Utc::now();
    replacement.iter().any(|(logical_name, entries)| {
        entries.iter().any(|entry| {
            crate::LlmClient::provider_member_health_identities(logical_name, &entry.key_id).any(
                |(logical, key_id)| {
                    let Some(current) = state.health.get(logical).and_then(|rows| rows.get(key_id))
                    else {
                        return false;
                    };
                    if !observation_changed(
                        current,
                        baseline.get(logical).and_then(|rows| rows.get(key_id)),
                    ) {
                        return false;
                    }
                    let (availability, remaining) = state
                        .health_snapshots
                        .get(logical)
                        .and_then(|rows| rows.get(key_id))
                        .map(|snapshot| snapshot.availability_at(now))
                        .unwrap_or_else(|| {
                            ProviderHealthSnapshot::from_health(current).availability_at(now)
                        });
                    availability != KeyAvailability::Available
                        && (availability != KeyAvailability::Cooldown || remaining.unwrap_or(0) > 0)
                },
            )
        })
    })
}

/// The state lock orders observations against publication. Preserve every
/// observation changed since the pre-scan baseline, including outcomes for a
/// member absent from the replacement; unchanged health still follows the
/// existing replacement/reset and last-known-good retention rules.
pub(super) fn preserve_newer_health_observations(
    next: &mut ProviderState,
    previous: &ProviderState,
    baseline: &HashMap<String, HashMap<String, VaultKeyHealth>>,
) {
    for (logical_name, members) in &previous.health {
        for (key_id, current) in members {
            if !observation_changed(
                current,
                baseline.get(logical_name).and_then(|rows| rows.get(key_id)),
            ) {
                continue;
            }
            next.health
                .entry(logical_name.clone())
                .or_default()
                .insert(key_id.clone(), current.clone());
            let snapshot = previous
                .health_snapshots
                .get(logical_name)
                .and_then(|rows| rows.get(key_id))
                .cloned()
                .unwrap_or_else(|| ProviderHealthSnapshot::from_health(current));
            next.health_snapshots
                .entry(logical_name.clone())
                .or_default()
                .insert(key_id.clone(), snapshot);
            if let Some(until) = previous
                .cooldowns
                .get(logical_name)
                .and_then(|rows| rows.get(key_id))
            {
                next.cooldowns
                    .entry(logical_name.clone())
                    .or_default()
                    .insert(key_id.clone(), *until);
            }
        }
    }
}

fn observation_changed(current: &VaultKeyHealth, baseline: Option<&VaultKeyHealth>) -> bool {
    // These are ordered in-memory snapshots, not independent wall-clock
    // samples. A clock adjustment cannot turn a later mutation into old state.
    baseline.is_none_or(|baseline| {
        current.logical_name != baseline.logical_name
            || current.key_id != baseline.key_id
            || current.status != baseline.status
            || current.cooldown_until != baseline.cooldown_until
            || current.last_success != baseline.last_success
            || current.last_attempt != baseline.last_attempt
            || current.last_error != baseline.last_error
            || current.error_count != baseline.error_count
            || current.auth_failed != baseline.auth_failed
            || current.disabled != baseline.disabled
            || current.metadata != baseline.metadata
            || current.updated_at != baseline.updated_at
    })
}

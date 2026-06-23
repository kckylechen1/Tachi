use memory_core::vault::VaultKeyHealth;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::types::ProviderRotationGroupProbe;
use crate::status_ops::{ApiKeyRotationMemberStatus, ApiKeyRotationStatus};

#[derive(Debug, Clone)]
pub(super) struct RotationSourceStatus {
    pub(super) total_keys: i64,
    pub(super) current_index: i64,
    pub(super) strategy: String,
    pub(super) members: Vec<String>,
}

pub(super) fn rotation_member_names(vault_names: &HashSet<String>, prefix: &str) -> Vec<String> {
    let mut members = vault_names
        .iter()
        .filter_map(|name| {
            crate::provider_config::parse_rotation_member_name(name)
                .filter(|(member_prefix, _)| *member_prefix == prefix)
                .map(|(_, idx)| (idx, name.clone()))
        })
        .collect::<Vec<_>>();
    members.sort_by(|(left_idx, left_name), (right_idx, right_name)| {
        left_idx
            .cmp(right_idx)
            .then_with(|| left_name.cmp(right_name))
    });
    members.into_iter().map(|(_, name)| name).collect()
}

pub(super) fn build_rotation_status(
    source: &RotationSourceStatus,
    probe: Option<&ProviderRotationGroupProbe>,
    health_members: Option<&HashMap<String, VaultKeyHealth>>,
    now: chrono::DateTime<chrono::Utc>,
) -> ApiKeyRotationStatus {
    let probe_present = probe.is_some();
    let probed_members = probe.map(|probe| {
        probe
            .keys
            .iter()
            .map(|member| (member.name.as_str(), member))
            .collect::<HashMap<_, _>>()
    });
    let mut saw_runtime_health = false;
    let mut healthy_keys = 0;
    let mut rate_limited_keys = 0;
    let mut auth_failed_keys = 0;
    let members = source
        .members
        .iter()
        .map(|name| {
            if let Some(member) = probed_members
                .as_ref()
                .and_then(|members| members.get(name.as_str()))
                .map(|member| (*member).clone())
            {
                saw_runtime_health = true;
                match member.status.as_str() {
                    "ok" => healthy_keys += 1,
                    "rate_limited" => rate_limited_keys += 1,
                    "auth_failed" => auth_failed_keys += 1,
                    _ => {}
                }
                return member;
            }

            if let Some(health) = health_members.and_then(|members| members.get(name.as_str())) {
                saw_runtime_health = true;
                let status = if health.disabled {
                    "disabled"
                } else if health.auth_failed {
                    "auth_failed"
                } else {
                    match health.status.as_str() {
                        "exhausted" => "exhausted",
                        "rate_limited" | "cooldown" => {
                            if health
                                .cooldown_until
                                .as_deref()
                                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                                .is_some_and(|until| until.with_timezone(&chrono::Utc) > now)
                            {
                                "rate_limited"
                            } else {
                                "ok"
                            }
                        }
                        _ => "ok",
                    }
                };

                let mut message = health.last_error.clone();
                if message.is_none() && !matches!(status, "ok" | "configured") {
                    message = Some(format!("vault status: {}", health.status));
                }
                let last_probe_at = health
                    .last_attempt
                    .clone()
                    .or_else(|| health.last_success.clone())
                    .or_else(|| Some(health.updated_at.clone()));

                match status {
                    "ok" => healthy_keys += 1,
                    "rate_limited" => rate_limited_keys += 1,
                    "auth_failed" => auth_failed_keys += 1,
                    _ => {}
                }

                return ApiKeyRotationMemberStatus {
                    name: name.clone(),
                    status: status.to_string(),
                    message,
                    last_probe_at,
                };
            }

            ApiKeyRotationMemberStatus {
                name: name.clone(),
                status: "configured".to_string(),
                message: None,
                last_probe_at: None,
            }
        })
        .collect::<Vec<_>>();
    ApiKeyRotationStatus {
        total_keys: source.total_keys,
        configured_keys: members.len() as i64,
        healthy_keys: if probe_present || saw_runtime_health {
            Some(healthy_keys)
        } else {
            None
        },
        rate_limited_keys: probe
            .map(|probe| probe.rate_limited_keys)
            .unwrap_or(rate_limited_keys),
        auth_failed_keys: probe
            .map(|probe| probe.auth_failed_keys)
            .unwrap_or(auth_failed_keys),
        current_index: source.current_index,
        strategy: source.strategy.clone(),
        next_retry_at: probe.and_then(|probe| probe.next_retry_at.clone()),
        members,
    }
}

pub(super) fn collect_rotation_sources(
    global_db_path: &Path,
) -> Vec<(String, RotationSourceStatus)> {
    let Some(path) = global_db_path.to_str() else {
        return Vec::new();
    };
    let Ok(store) = memory_core::MemoryStore::open_read_only(path) else {
        return Vec::new();
    };
    let Ok(entries) = store.vault_list_entries() else {
        return Vec::new();
    };
    let vault_names = entries
        .into_iter()
        .filter(|entry| entry.secret_type == "api_key")
        .map(|entry| entry.name)
        .collect::<HashSet<_>>();
    let Ok(rotations) = store.vault_list_rotations() else {
        return Vec::new();
    };
    let mut out = rotations
        .into_iter()
        .map(|rotation| {
            let members = rotation_member_names(&vault_names, &rotation.prefix);
            (
                rotation.prefix,
                RotationSourceStatus {
                    total_keys: rotation.total_keys,
                    current_index: rotation.current_index,
                    strategy: rotation.rotation_strategy,
                    members,
                },
            )
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

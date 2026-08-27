//! Classify a skipped `vault:` alias against listed Vault rows (tachi#1854).
//!
//! Pool loading silently drops listed entries (wrong type, empty value,
//! `allowed_agents` fence, health unusable, not a ModelApi name). Alias
//! resolution then sees a miss and used to print revocation wording
//! (`absent from a readable Vault`) even though `vault list` showed the name.
//!
//! This classifier is metadata-only: it never returns secret values, alias
//! target names, fingerprints, or lengths.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use memcore::vault::{VaultEntry, VaultKeyHealth, SECRET_TYPE_API_KEY};
use tachi_llm::{parse_rotation_member_name, AliasSkipClass};

/// If `target` is a listed Vault row that pool loading would drop, the
/// integrity class for that drop. `None` means no row — genuine revocation.
pub(crate) fn classify_listed_alias_target(
    target: &str,
    entries: &[VaultEntry],
    health_rows: &[VaultKeyHealth],
    model_provider_names: &HashSet<String>,
    now: DateTime<Utc>,
) -> Option<AliasSkipClass> {
    let entry = entries.iter().find(|entry| entry.name == target)?;
    if entry.secret_type != SECRET_TYPE_API_KEY {
        return Some(AliasSkipClass::ListedWrongType);
    }
    if entry
        .allowed_agents
        .as_ref()
        .is_some_and(|agents| !agents.is_empty())
    {
        return Some(AliasSkipClass::ListedFenced);
    }
    if let Some(class) = health_unusable_class(target, health_rows, now) {
        return Some(class);
    }
    if !is_model_provider_name(target, model_provider_names) {
        return Some(AliasSkipClass::ListedNotModelProvider);
    }
    Some(AliasSkipClass::ListedEmpty)
}

fn is_model_provider_name(name: &str, allowed: &HashSet<String>) -> bool {
    allowed.contains(name)
        || parse_rotation_member_name(name).is_some_and(|(prefix, _)| allowed.contains(prefix))
}

fn health_unusable_class(
    target: &str,
    health_rows: &[VaultKeyHealth],
    now: DateTime<Utc>,
) -> Option<AliasSkipClass> {
    let health = health_rows
        .iter()
        .find(|row| row.key_id == target || (row.logical_name == target && row.key_id == target))?;
    if health.disabled {
        return Some(AliasSkipClass::ListedUnusableDisabled);
    }
    if health.auth_failed {
        return Some(AliasSkipClass::ListedUnusableAuthFailed);
    }
    match health.status.as_str() {
        "exhausted" => Some(AliasSkipClass::ListedUnusableExhausted),
        "rate_limited" | "cooldown" => {
            let cooling = health
                .cooldown_until
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|until| until.with_timezone(&Utc) > now);
            cooling.then_some(AliasSkipClass::ListedUnusableCooldown)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::vault::SECRET_TYPE_OTHER;

    fn entry(name: &str, secret_type: &str, allowed: Option<Vec<String>>) -> VaultEntry {
        VaultEntry {
            name: name.to_string(),
            secret_type: secret_type.to_string(),
            allowed_agents: allowed,
            ..VaultEntry::default()
        }
    }

    fn model_names() -> HashSet<String> {
        HashSet::from([
            "SILICONFLOW_API_KEY".to_string(),
            "VOYAGE_API_KEY".to_string(),
        ])
    }

    #[test]
    fn missing_row_is_revocation_not_integrity() {
        assert_eq!(
            classify_listed_alias_target(
                "SILICONFLOW_API_KEY",
                &[],
                &[],
                &model_names(),
                Utc::now()
            ),
            None
        );
    }

    #[test]
    fn auth_failed_listed_row_is_unusable_not_absent() {
        let health = VaultKeyHealth {
            logical_name: "SILICONFLOW_API_KEY".to_string(),
            key_id: "SILICONFLOW_API_KEY".to_string(),
            auth_failed: true,
            status: "error".to_string(),
            ..VaultKeyHealth::default()
        };
        let class = classify_listed_alias_target(
            "SILICONFLOW_API_KEY",
            &[entry("SILICONFLOW_API_KEY", SECRET_TYPE_API_KEY, None)],
            &[health],
            &model_names(),
            Utc::now(),
        );
        assert_eq!(class, Some(AliasSkipClass::ListedUnusableAuthFailed));
        assert!(!class
            .unwrap()
            .operator_reason("SILICONFLOW_API_KEY")
            .contains("absent from a readable Vault"));
    }

    #[test]
    fn wrong_type_listed_row_is_not_absent() {
        let class = classify_listed_alias_target(
            "EXTRACT_BASE_URL",
            &[entry("EXTRACT_BASE_URL", SECRET_TYPE_OTHER, None)],
            &[],
            &model_names(),
            Utc::now(),
        );
        assert_eq!(class, Some(AliasSkipClass::ListedWrongType));
    }

    #[test]
    fn fenced_listed_row_is_not_absent() {
        let class = classify_listed_alias_target(
            "SILICONFLOW_API_KEY",
            &[entry(
                "SILICONFLOW_API_KEY",
                SECRET_TYPE_API_KEY,
                Some(vec!["other-agent".to_string()]),
            )],
            &[],
            &model_names(),
            Utc::now(),
        );
        assert_eq!(class, Some(AliasSkipClass::ListedFenced));
    }

    #[test]
    fn search_key_listed_row_is_not_a_model_provider() {
        let class = classify_listed_alias_target(
            "TAVILY_API_KEY",
            &[entry("TAVILY_API_KEY", SECRET_TYPE_API_KEY, None)],
            &[],
            &model_names(),
            Utc::now(),
        );
        assert_eq!(class, Some(AliasSkipClass::ListedNotModelProvider));
    }
}

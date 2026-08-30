#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct VaultListRow {
    pub name: String,
    pub secret_type: String,
    pub description: String,
}

fn json_list_row(entry: &serde_json::Value) -> VaultListRow {
    VaultListRow {
        name: entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        secret_type: entry
            .get("secret_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: entry
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

fn json_rows(value: &serde_json::Value, key: &str) -> Option<Vec<VaultListRow>> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|rows| rows.iter().map(json_list_row).collect())
}

fn legacy_json_list_groups(
    value: &serde_json::Value,
) -> Option<(Vec<VaultListRow>, Vec<VaultListRow>)> {
    let secrets = value.get("secrets").and_then(|v| v.as_array())?;
    let mut config = Vec::new();
    let mut credentials = Vec::new();
    for entry in secrets {
        let mut row = json_list_row(entry);
        row.secret_type =
            memcore::effective_vault_secret_type(&row.name, &row.secret_type).to_string();
        if row.secret_type == memcore::SECRET_TYPE_CONFIG {
            config.push(row);
        } else {
            credentials.push(row);
        }
    }
    Some((config, credentials))
}

pub(super) fn format_vault_list_groups(
    config: &[VaultListRow],
    credentials: &[VaultListRow],
) -> String {
    if config.is_empty() && credentials.is_empty() {
        return "(no secrets stored)\n".to_string();
    }
    let mut out = String::new();
    let mut write_section = |title: &str, rows: &[VaultListRow]| {
        if rows.is_empty() {
            return;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(title);
        out.push('\n');
        out.push_str(&format!("{:<30} {:<12} DESCRIPTION\n", "NAME", "TYPE"));
        for row in rows {
            out.push_str(&format!(
                "{:<30} {:<12} {}\n",
                row.name, row.secret_type, row.description
            ));
        }
    };
    write_section("CONFIG", config);
    write_section("CREDENTIALS", credentials);
    out.push_str(&format!(
        "\n{} config, {} credential ({} total).\n",
        config.len(),
        credentials.len(),
        config.len() + credentials.len()
    ));
    out
}

pub(super) fn print_vault_list_output(out: &str) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(out) else {
        println!("{out}");
        return Ok(());
    };
    let config = json_rows(&value, "config");
    let credentials = json_rows(&value, "credentials");
    if let (Some(config), Some(credentials)) = (config, credentials) {
        print!("{}", format_vault_list_groups(&config, &credentials));
        return Ok(());
    }
    let Some((config, credentials)) = legacy_json_list_groups(&value) else {
        println!("{out}");
        return Ok(());
    };
    print!("{}", format_vault_list_groups(&config, &credentials));
    Ok(())
}

pub(super) fn normalize_rotation_strategy_cli(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "round_robin" | "round-robin" => "round_robin".to_string(),
        "random" => "random".to_string(),
        "least_recently_used" | "least-recently-used" | "lru" => "least_recently_used".to_string(),
        _ => "round_robin".to_string(),
    }
}

fn key_health_blocks_cli(health: &memcore::vault::VaultKeyHealth) -> bool {
    if health.disabled || health.auth_failed {
        return true;
    }
    match health.status.as_str() {
        "exhausted" => true,
        "rate_limited" | "cooldown" => health
            .cooldown_until
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|until| until.with_timezone(&chrono::Utc) > chrono::Utc::now()),
        _ => false,
    }
}

fn rotation_member_name(prefix: &str, idx: i64) -> String {
    format!("{prefix}_{idx}")
}

pub(crate) fn validate_api_key_lease_target(
    store: &memcore::MemoryStore,
    logical_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if memcore::is_lane_config_secret_name(logical_name) {
        return Err(format!(
            "Vault name '{logical_name}' is lane config, not a credential; refusing to lease it as an API key"
        )
        .into());
    }
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?;
    if let Some(entry) = entries.iter().find(|entry| entry.name == logical_name) {
        let effective = memcore::effective_vault_secret_type(&entry.name, &entry.secret_type);
        if effective != memcore::SECRET_TYPE_API_KEY {
            return Err(format!(
                "Vault name '{logical_name}' is {effective}, not a credential; refusing to lease it as an API key"
            )
            .into());
        }
    }
    let rotation_prefix =
        crate::vault_ops::canonical_api_key_health_logical_name(store, logical_name)?;
    if store
        .vault_get_rotation(&rotation_prefix)
        .map_err(|e| format!("vault_get_rotation: {e}"))?
        .is_some()
    {
        memcore::validate_api_key_rotation_members(&entries, &rotation_prefix).map_err(
            |error| format!("{error}; refusing to lease '{logical_name}' as an API key"),
        )?;
    }
    Ok(())
}

pub(crate) fn lease_api_key_from_store(
    store: &memcore::MemoryStore,
    key: &[u8; 32],
    logical_name: &str,
) -> Result<(String, String, String), Box<dyn std::error::Error>> {
    validate_api_key_lease_target(store, logical_name)?;
    if memcore::is_lane_config_secret_name(logical_name) {
        return Err(format!(
            "Vault name '{logical_name}' is lane config, not a credential; refusing to lease it as an API key"
        )
        .into());
    }
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?;
    let health_logical_name =
        crate::vault_ops::canonical_api_key_health_logical_name(store, logical_name)?;
    if let Some(entry) = entries.iter().find(|entry| entry.name == logical_name) {
        let effective = memcore::effective_vault_secret_type(&entry.name, &entry.secret_type);
        if effective != memcore::SECRET_TYPE_API_KEY {
            return Err(format!(
                "Vault name '{logical_name}' is {effective}, not a credential; refusing to lease it as an API key"
            )
            .into());
        }
    }
    let rotation = store
        .vault_get_rotation(&health_logical_name)
        .map_err(|e| format!("vault_get_rotation: {e}"))?;
    let configured_member_request = health_logical_name != logical_name;

    let candidate_names = if configured_member_request {
        vec![logical_name.to_string()]
    } else if let Some(rotation) = rotation.as_ref() {
        let total = rotation.total_keys.max(0);
        if total == 0 {
            Vec::new()
        } else {
            let start = if rotation.current_index <= 0 {
                1
            } else {
                rotation.current_index
            };
            (0..total)
                .map(|offset| {
                    let idx = ((start - 1 + offset) % total) + 1;
                    rotation_member_name(logical_name, idx)
                })
                .collect::<Vec<_>>()
        }
    } else {
        vec![logical_name.to_string()]
    };

    for candidate in candidate_names {
        let Some(entry) = entries.iter().find(|entry| entry.name == candidate) else {
            continue;
        };
        if memcore::is_lane_config_secret_name(&entry.name)
            || memcore::effective_vault_secret_type(&entry.name, &entry.secret_type)
                != memcore::SECRET_TYPE_API_KEY
        {
            continue;
        }
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }
        if let Some(health) = store
            .vault_get_key_health(&health_logical_name, &candidate)
            .map_err(|e| format!("vault_get_key_health: {e}"))?
        {
            if key_health_blocks_cli(&health) {
                continue;
            }
        }

        let decrypted = crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
        let mut value = crate::vault_crypto::decode_utf8_zeroizing(
            decrypted,
            format!("Vault secret '{}' is not valid UTF-8", entry.name),
        )?;
        if value.trim().is_empty() {
            crate::vault_crypto::zero_string(&mut value);
            continue;
        }

        if let Some(rotation) = rotation.as_ref() {
            if let Some((prefix, idx)) =
                crate::provider_config::parse_rotation_member_name(&entry.name)
            {
                if prefix == health_logical_name && rotation.total_keys > 0 {
                    store
                        .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                            current_index: (idx as i64 % rotation.total_keys) + 1,
                            updated_at: chrono::Utc::now().to_rfc3339(),
                            ..rotation.clone()
                        })
                        .map_err(|e| format!("vault_set_rotation: {e}"))?;
                }
            }
        }
        let _ = store.vault_touch_entry(&entry.name);
        return Ok((health_logical_name.to_string(), entry.name.clone(), value));
    }

    Err(format!(
        "No usable API key available for '{logical_name}' (missing, restricted, disabled, auth-failed, exhausted, or rate-limited)."
    )
    .into())
}

pub(super) fn print_lease_output(
    out: &str,
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(out)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let env_name = value
        .get("env_name")
        .and_then(|value| value.as_str())
        .ok_or("lease response missing env_name")?;
    let secret = value
        .get("env")
        .and_then(|env| env.get(env_name))
        .and_then(|value| value.as_str())
        .ok_or("lease response missing env value")?;
    let escaped = secret.replace('\'', "'\\''");
    println!("export {env_name}='{escaped}'");
    eprintln!(
        "# tachi vault lease: {} -> {}",
        value
            .get("logical_name")
            .and_then(|value| value.as_str())
            .unwrap_or(env_name),
        value
            .get("key_id")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
    );
    Ok(())
}

pub(super) fn vault_get_output(
    name: &str,
    value: &str,
    reveal: bool,
    json_output: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    if json_output {
        let mut body = serde_json::json!({
            "name": name,
            "revealed": reveal,
        });
        if reveal {
            body["value"] = serde_json::json!(value);
        } else {
            body["value"] = serde_json::json!("<redacted>");
            body["hint"] = serde_json::json!("Pass --reveal to print the decrypted secret value.");
        }
        return Ok(format!("{}\n", serde_json::to_string_pretty(&body)?));
    }

    if reveal {
        return Ok(format!("{value}\n"));
    }

    Ok(format!(
        "Secret '{name}' exists; value hidden. Re-run with --reveal to print the decrypted value.\n"
    ))
}

/// Turn one `tachi vault record-key-result` / `vault_record_key_result`
/// report into the row its caller then persists.
///
/// #1680 D6: this function used to carry its own copy of the status ladder,
/// cooldown arithmetic, and per-outcome field rules, which had already drifted
/// from the LLM client's copy. It is now an adapter over the single writer,
/// [`memcore::vault::health::record_key_outcome`]. The CLI/MCP wire contract
/// (arguments in, `VaultKeyHealth` out) is unchanged; what the row now also
/// carries is the evidence kind — `SelfReported`, because a caller told us
/// this outcome rather than Tachi observing it.
pub(super) fn build_key_health_result(
    store: &memcore::MemoryStore,
    logical_name: &str,
    key_id: &str,
    status_code: Option<u16>,
    outcome: Option<&str>,
    retry_after_secs: Option<u64>,
    reason: Option<&str>,
) -> Result<memcore::vault::VaultKeyHealth, Box<dyn std::error::Error>> {
    use memcore::vault::health::{record_key_outcome, EvidenceKind, TypedOutcome};

    let existing = store
        .vault_get_key_health(logical_name, key_id)
        .map_err(|e| format!("vault_get_key_health: {e}"))?;
    let outcome = TypedOutcome::classify(status_code, outcome, retry_after_secs);
    // The one reason default that is this channel's own: an unclassified
    // report names the status code it came with.
    let reason = match outcome {
        TypedOutcome::Error => reason
            .map(str::to_string)
            .or_else(|| status_code.map(|code| format!("provider returned HTTP {code}"))),
        _ => reason.map(str::to_string),
    };
    Ok(record_key_outcome(
        existing.as_ref(),
        logical_name,
        key_id,
        outcome,
        EvidenceKind::SelfReported,
        reason.as_deref(),
        chrono::Utc::now(),
    )
    .health)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, secret_type: &str, description: &str) -> VaultListRow {
        VaultListRow {
            name: name.to_string(),
            secret_type: secret_type.to_string(),
            description: description.to_string(),
        }
    }

    #[test]
    fn format_vault_list_groups_separates_config_from_credentials() {
        let text = format_vault_list_groups(
            &[row("EXTRACT_BASE_URL", "config", "lane url")],
            &[row("DEEPSEEK_API_KEY", "api_key", "key")],
        );
        assert!(text.contains("CONFIG"), "{text}");
        assert!(text.contains("CREDENTIALS"), "{text}");
        assert!(text.contains("EXTRACT_BASE_URL"), "{text}");
        assert!(text.contains("DEEPSEEK_API_KEY"), "{text}");
        assert!(text.contains("1 config, 1 credential (2 total)."), "{text}");
    }

    #[test]
    fn print_vault_list_output_uses_grouped_json_arrays() {
        let json = serde_json::json!({
            "count": 2,
            "config": [{
                "name": "EXTRACT_BASE_URL",
                "secret_type": "config",
                "group": "config",
                "description": "lane"
            }],
            "credentials": [{
                "name": "DEEPSEEK_API_KEY",
                "secret_type": "api_key",
                "group": "credential",
                "description": "key"
            }],
            "secrets": []
        });
        let config = json_rows(&json, "config").expect("config");
        let credentials = json_rows(&json, "credentials").expect("credentials");
        let text = format_vault_list_groups(&config, &credentials);
        assert!(text.starts_with("CONFIG\n"), "{text}");
        assert!(!text.contains("(no secrets stored)"), "{text}");
    }

    #[test]
    fn legacy_daemon_payload_remaps_leftover_lane_config_before_grouping() {
        let json = serde_json::json!({
            "secrets": [{
                "name": "EXTRACT_BASE_URL",
                "secret_type": "api_key",
                "group": "credential",
                "description": "legacy lane URL"
            }, {
                "name": "DEEPSEEK_API_KEY",
                "secret_type": "api_key",
                "description": "provider key"
            }]
        });
        let (config, credentials) = legacy_json_list_groups(&json).expect("legacy payload");
        assert_eq!(
            config,
            [row("EXTRACT_BASE_URL", "config", "legacy lane URL")]
        );
        assert_eq!(
            credentials,
            [row("DEEPSEEK_API_KEY", "api_key", "provider key")]
        );
    }
}

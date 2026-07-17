pub(super) fn print_vault_list_output(out: &str) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(out) else {
        println!("{out}");
        return Ok(());
    };
    let Some(secrets) = value.get("secrets").and_then(|v| v.as_array()) else {
        println!("{out}");
        return Ok(());
    };

    if secrets.is_empty() {
        println!("(no secrets stored)");
        return Ok(());
    }

    println!("{:<30} {:<12} DESCRIPTION", "NAME", "TYPE");
    for entry in secrets {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let secret_type = entry
            .get("secret_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let description = entry
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        println!("{name:<30} {secret_type:<12} {description}");
    }
    let count = value
        .get("count")
        .and_then(|v| v.as_u64())
        .unwrap_or(secrets.len() as u64);
    println!("\n{count} secret(s) total.");
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

pub(in crate::bootstrap) fn lease_api_key_from_store(
    store: &memcore::MemoryStore,
    key: &[u8; 32],
    logical_name: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?;
    let rotation = store
        .vault_get_rotation(logical_name)
        .map_err(|e| format!("vault_get_rotation: {e}"))?;

    let candidate_names = if let Some(rotation) = rotation.as_ref() {
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
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }
        if let Some(health) = store
            .vault_get_key_health(logical_name, &candidate)
            .map_err(|e| format!("vault_get_key_health: {e}"))?
        {
            if key_health_blocks_cli(&health) {
                continue;
            }
        }

        let decrypted = crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
        let mut value = String::from_utf8(decrypted)
            .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;
        if value.trim().is_empty() {
            crate::vault_crypto::zero_string(&mut value);
            continue;
        }

        if let Some(rotation) = rotation.as_ref() {
            if let Some((prefix, idx)) =
                crate::provider_config::parse_rotation_member_name(&entry.name)
            {
                if prefix == logical_name && rotation.total_keys > 0 {
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
        if let Err(error) = store.vault_touch_entry(&entry.name) {
            tracing::warn!(
                error = %error,
                entry = %entry.name,
                "failed to touch vault entry during output resolution"
            );
        }
        return Ok((entry.name.clone(), value));
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

pub(super) fn build_key_health_result(
    store: &memcore::MemoryStore,
    logical_name: &str,
    key_id: &str,
    status_code: Option<u16>,
    outcome: Option<&str>,
    retry_after_secs: Option<u64>,
    reason: Option<&str>,
) -> Result<memcore::vault::VaultKeyHealth, Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let mut health = store
        .vault_get_key_health(logical_name, key_id)
        .map_err(|e| format!("vault_get_key_health: {e}"))?
        .unwrap_or_else(|| memcore::vault::VaultKeyHealth {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            ..memcore::vault::VaultKeyHealth::default()
        });
    let outcome = outcome.map(|value| value.to_ascii_lowercase());
    health.last_attempt = Some(now.to_rfc3339());
    health.updated_at = now.to_rfc3339();
    if status_code == Some(429) || matches!(outcome.as_deref(), Some("rate_limited" | "cooldown")) {
        let cooldown = retry_after_secs.unwrap_or(60).clamp(1, 3600);
        health.status = "rate_limited".to_string();
        health.cooldown_until =
            Some((now + chrono::Duration::seconds(cooldown as i64)).to_rfc3339());
        health.last_error = reason
            .map(str::to_string)
            .or_else(|| Some(format!("rate limited; retry after {cooldown}s")));
        health.error_count += 1;
    } else if matches!(status_code, Some(401 | 403))
        || matches!(outcome.as_deref(), Some("auth_failed"))
    {
        health.status = "auth_failed".to_string();
        health.auth_failed = true;
        health.cooldown_until = None;
        health.last_error = reason
            .map(str::to_string)
            .or_else(|| Some("auth failure".to_string()));
        health.error_count += 1;
    } else if matches!(outcome.as_deref(), Some("exhausted")) {
        health.status = "exhausted".to_string();
        health.last_error = reason
            .map(str::to_string)
            .or_else(|| Some("key exhausted".to_string()));
        health.error_count += 1;
    } else if status_code.is_some_and(|code| (200..300).contains(&code))
        || matches!(outcome.as_deref(), Some("success" | "ok"))
    {
        health.status = "ok".to_string();
        health.auth_failed = false;
        health.cooldown_until = None;
        health.last_success = Some(now.to_rfc3339());
        health.last_error = None;
        health.error_count = 0;
    } else {
        health.status = "error".to_string();
        health.last_error = reason
            .map(str::to_string)
            .or_else(|| status_code.map(|code| format!("provider returned HTTP {code}")));
        health.error_count += 1;
    }
    Ok(health)
}

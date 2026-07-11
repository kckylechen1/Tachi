use crate::hub_ops::handle_hub_quick_add;
use crate::tool_params::HubQuickAddParams;
use memcore::HubCapability;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::{HubAction, McpAction};

use super::super::{evaluate_cli_capability_enabled, open_cli_store, print_pretty_json};

pub(super) async fn run_hub_command(
    action: HubAction,
    app_home: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let hub_db = memory_server_hub_cli::resolve_hub_db(None, app_home);
    match action {
        HubAction::List {
            cap_type,
            all,
            json: true,
        } => {
            let store = open_cli_store(&hub_db)?;
            let caps = store.hub_list(cap_type.as_deref(), all)?;
            print_pretty_json(&serde_json::to_value(caps)?)
        }
        HubAction::List {
            cap_type,
            all,
            json: false,
        } => {
            memory_server_hub_cli::run(
                &HubAction::List {
                    cap_type,
                    all,
                    json: false,
                },
                &hub_db,
                app_home,
            )
            .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::Show { id } => {
            memory_server_hub_cli::run(&HubAction::Show { id }, &hub_db, app_home)
                .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::Bindings => {
            memory_server_hub_cli::run(&HubAction::Bindings, &hub_db, app_home)
                .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::Doctor { fix } => {
            memory_server_hub_cli::run(&HubAction::Doctor { fix }, &hub_db, app_home)
                .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::Register {
            id,
            cap_type,
            name,
            definition,
            description,
        } => {
            let store = open_cli_store(&hub_db)?;
            let (enabled, warning) = evaluate_cli_capability_enabled(&cap_type, &definition)?;
            let is_mcp = cap_type.eq_ignore_ascii_case("mcp");
            let cap = HubCapability {
                id: id.clone(),
                cap_type,
                name,
                version: 1,
                description: description.unwrap_or_default(),
                definition,
                enabled,
                review_status: if is_mcp {
                    "pending".to_string()
                } else {
                    "approved".to_string()
                },
                health_status: if is_mcp {
                    "unknown".to_string()
                } else {
                    "healthy".to_string()
                },
                last_error: None,
                last_success_at: None,
                last_failure_at: None,
                fail_streak: 0,
                active_version: None,
                exposure_mode: "direct".to_string(),
                uses: 0,
                successes: 0,
                failures: 0,
                avg_rating: 0.0,
                last_used: None,
                created_at: String::new(),
                updated_at: String::new(),
            };
            store.hub_register(&cap)?;
            let saved = store.hub_get(&id)?.ok_or_else(|| {
                std::io::Error::other(format!(
                    "capability '{}' registered but failed to reload from DB",
                    id
                ))
            })?;
            let mut output = serde_json::to_value(saved)?;
            if let Some(w) = warning {
                if let Some(obj) = output.as_object_mut() {
                    obj.insert("warning".to_string(), json!(w));
                }
            }
            print_pretty_json(&output)
        }
        HubAction::Enable { id } => {
            let store = open_cli_store(&hub_db)?;
            let updated = store.hub_set_enabled(&id, true)?;
            print_pretty_json(&json!({
                "updated": updated,
                "id": id,
                "enabled": true,
            }))
        }
        HubAction::Disable { id } => {
            let store = open_cli_store(&hub_db)?;
            let updated = store.hub_set_enabled(&id, false)?;
            print_pretty_json(&json!({
                "updated": updated,
                "id": id,
                "enabled": false,
            }))
        }
        HubAction::Stats { json: true } => {
            let store = open_cli_store(&hub_db)?;
            let caps = store.hub_list(None, false)?;
            let mut by_type: HashMap<String, usize> = HashMap::new();
            for cap in &caps {
                *by_type.entry(cap.cap_type.clone()).or_insert(0) += 1;
            }
            let total_uses: u64 = caps.iter().map(|c| c.uses).sum();
            let total_successes: u64 = caps.iter().map(|c| c.successes).sum();
            print_pretty_json(&json!({
                "total_capabilities": caps.len(),
                "by_type": by_type,
                "total_uses": total_uses,
                "total_successes": total_successes,
                "success_rate": if total_uses > 0 { total_successes as f64 / total_uses as f64 } else { 0.0 },
            }))
        }
        HubAction::Stats { json: false } => {
            memory_server_hub_cli::cmd_stats(&hub_db)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(())
        }
    }
}

pub(super) async fn run_mcp_command(
    action: McpAction,
    app_home: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        McpAction::Add {
            name,
            url,
            transport,
            headers,
            key,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
            json,
        } => {
            let normalized_name = normalize_mcp_name(&name)?;
            let vault_key_name = if key.is_some() {
                Some(select_mcp_vault_key_name(&normalized_name, &headers)?)
            } else {
                None
            };
            let params = build_mcp_quick_add_params(
                &name,
                &url,
                &transport,
                &headers,
                vault_key_name.as_deref(),
            )?;
            let hub_db = memory_server_hub_cli::resolve_hub_db(None, app_home);
            let vault_key_created = if let Some(raw_key) = key {
                Some(store_mcp_key_in_vault(
                    &hub_db,
                    vault_key_name.as_deref().expect("vault key name"),
                    &normalized_name,
                    raw_key,
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                    insecure_password_file,
                )?)
            } else {
                None
            };
            let server = crate::cli_client::build_in_process_server(&hub_db, None)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let out = handle_hub_quick_add(&server, params)
                .await
                .map_err(std::io::Error::other)?;
            let value: Value = serde_json::from_str(&out)?;
            if json {
                let mut value = value;
                attach_mcp_vault_output(&mut value, vault_key_name.as_deref(), vault_key_created);
                print_pretty_json(&value)
            } else {
                print_pretty_json(&json!({
                    "id": value["register"]["id"],
                    "enabled": value.get("review").and_then(|review| review.get("enabled")).or_else(|| value["register"].get("enabled")),
                    "review_status": value.get("review").and_then(|review| review.get("review_status")).or_else(|| value["register"].get("review_status")),
                    "tool_exposure": value["register"]["tool_exposure"],
                    "auto_approve": value["auto_approve"],
                    "vault_key": vault_key_name,
                    "vault_key_created": vault_key_created,
                    "warning": value.get("warning"),
                }))
            }
        }
    }
}

fn build_mcp_quick_add_params(
    name: &str,
    endpoint: &str,
    transport: &str,
    headers: &[String],
    vault_key_name: Option<&str>,
) -> Result<HubQuickAddParams, Box<dyn std::error::Error>> {
    let normalized_name = normalize_mcp_name(name)?;
    let transport = normalize_mcp_transport(transport)?;
    let mut definition = json!({
        "transport": transport,
        "tool_exposure": "gateway",
        "policy": {
            "visibility": "discoverable",
            "seats": [],
            "allowed_mcp_servers": []
        },
        "startup_timeout_ms": 10_000,
        "tool_timeout_ms": 30_000,
        "max_concurrency": 2,
        "tags": ["cli", "mcp"]
    });

    if transport == "stdio" {
        definition["command"] = json!(endpoint);
        definition["args"] = json!([]);
    } else {
        definition["url"] = json!(endpoint);
    }

    let mut header_map = parse_mcp_headers(headers)?;
    if let Some(vault_key_name) = vault_key_name {
        if !has_secret_header(&header_map) {
            header_map.insert(
                "Authorization".to_string(),
                format!("Bearer ${{vault:{vault_key_name}}}"),
            );
        }
    }
    if !header_map.is_empty() {
        definition["headers"] = json!(header_map);
    }

    Ok(HubQuickAddParams {
        id: format!("mcp:{normalized_name}"),
        cap_type: "mcp".to_string(),
        name: name.trim().to_string(),
        description: format!("MCP server registered by `tachi mcp add {normalized_name}`"),
        definition: serde_json::to_string(&definition)?,
        version: 1,
        scope: "global".to_string(),
        auto_approve: true,
    })
}

fn normalize_mcp_name(name: &str) -> Result<String, std::io::Error> {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let normalized = out.trim_matches('-').to_string();
    if normalized.is_empty() {
        Err(std::io::Error::other(
            "MCP name must contain at least one ASCII letter, digit, or underscore",
        ))
    } else {
        Ok(normalized)
    }
}

fn normalize_mcp_transport(transport: &str) -> Result<&'static str, std::io::Error> {
    match transport.trim().to_ascii_lowercase().as_str() {
        "http" => Ok("streamable-http"),
        "sse" => Ok("sse"),
        "stdio" => Ok("stdio"),
        other => Err(std::io::Error::other(format!(
            "unsupported MCP transport '{other}' (expected http, sse, or stdio)"
        ))),
    }
}

fn select_mcp_vault_key_name(
    normalized_name: &str,
    headers: &[String],
) -> Result<String, std::io::Error> {
    let header_map = parse_mcp_headers(headers)?;
    let mut explicit_key_name = None;
    for (name, value) in &header_map {
        if !is_mcp_secret_header(name) {
            continue;
        }
        if let Some(key_name) = extract_vault_placeholder_name(value) {
            crate::vault_crypto::validate_secret_name(&key_name).map_err(std::io::Error::other)?;
            match explicit_key_name.as_ref() {
                Some(existing) if existing != &key_name => {
                    return Err(std::io::Error::other(
                        "multiple secret-bearing MCP headers reference different Vault keys",
                    ));
                }
                Some(_) => {}
                None => explicit_key_name = Some(key_name),
            }
        }
    }
    Ok(explicit_key_name.unwrap_or_else(|| default_mcp_vault_key_name(normalized_name)))
}

fn default_mcp_vault_key_name(normalized_name: &str) -> String {
    let mut out = String::from("MCP_");
    let mut last_underscore = false;
    for ch in normalized_name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    out = out.trim_end_matches('_').to_string();
    out.push_str("_API_KEY");
    out
}

fn extract_vault_placeholder_name(value: &str) -> Option<String> {
    let start = value.find("${vault:")? + "${vault:".len();
    let rest = &value[start..];
    let end = rest.find('}')?;
    Some(rest[..end].to_string())
}

fn has_secret_header(headers: &HashMap<String, String>) -> bool {
    headers.keys().any(|name| is_mcp_secret_header(name))
}

fn store_mcp_key_in_vault(
    hub_db: &PathBuf,
    vault_key_name: &str,
    normalized_name: &str,
    raw_key: String,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    crate::bootstrap::vault_cli::unlock_and_upsert_api_key_secret(
        hub_db,
        vault_key_name,
        &format!("API key for MCP server {normalized_name}"),
        raw_key,
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )
}

fn attach_mcp_vault_output(
    value: &mut Value,
    vault_key_name: Option<&str>,
    vault_key_created: Option<bool>,
) {
    if let Some(obj) = value.as_object_mut() {
        if let Some(vault_key_name) = vault_key_name {
            obj.insert("vault_key".to_string(), json!(vault_key_name));
        }
        if let Some(vault_key_created) = vault_key_created {
            obj.insert("vault_key_created".to_string(), json!(vault_key_created));
        }
    }
}

fn parse_mcp_headers(headers: &[String]) -> Result<HashMap<String, String>, std::io::Error> {
    let mut out = HashMap::new();
    for (index, header) in headers.iter().enumerate() {
        let (name, value) = header.split_once(':').ok_or_else(|| {
            std::io::Error::other(format!(
                "invalid MCP header at index {index} (expected 'Name: Value')"
            ))
        })?;
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            return Err(std::io::Error::other(format!(
                "invalid MCP header at index {index} (name and value are required)"
            )));
        }
        validate_mcp_secret_header(name, value)?;
        out.insert(name.to_string(), value.to_string());
    }
    Ok(out)
}

fn validate_mcp_secret_header(name: &str, value: &str) -> Result<(), std::io::Error> {
    if !is_mcp_secret_header(name) {
        return Ok(());
    }

    let vault_only = is_vault_placeholder(value)
        || value
            .strip_prefix("Bearer ")
            .is_some_and(is_vault_placeholder);
    if vault_only {
        return Ok(());
    }

    Err(std::io::Error::other(
        "secret-bearing MCP headers must use only a ${vault:KEY} placeholder",
    ))
}

fn is_mcp_secret_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("x-api-key")
        || name.eq_ignore_ascii_case("api-key")
}

fn is_vault_placeholder(value: &str) -> bool {
    value.starts_with("${vault:")
        && value.ends_with('}')
        && value.matches("${vault:").count() == 1
        && value.find('}').is_some_and(|idx| idx == value.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL_RAW_VALUE: &str = "TEST_SENTINEL_VALUE_DO_NOT_STORE";

    #[test]
    fn hub_quick_add_mcp_cli_builds_gateway_definition_with_vault_header() {
        let params = build_mcp_quick_add_params(
            "Context 7",
            "https://mcp.context7.com/mcp",
            "http",
            &["Authorization: Bearer ${vault:CONTEXT7_API_KEY}".to_string()],
            None,
        )
        .expect("params");
        let definition: Value = serde_json::from_str(&params.definition).expect("definition json");

        assert_eq!(params.id, "mcp:context-7");
        assert_eq!(params.cap_type, "mcp");
        assert!(params.auto_approve);
        assert_eq!(definition["transport"], "streamable-http");
        assert_eq!(definition["url"], "https://mcp.context7.com/mcp");
        assert_eq!(definition["tool_exposure"], "gateway");
        assert_eq!(
            definition["headers"]["Authorization"],
            "Bearer ${vault:CONTEXT7_API_KEY}"
        );
    }

    #[test]
    fn hub_quick_add_mcp_cli_adds_vault_header_for_key_flag() {
        let params = build_mcp_quick_add_params(
            "context7",
            "https://mcp.context7.com/mcp",
            "http",
            &[],
            Some("MCP_CONTEXT7_API_KEY"),
        )
        .expect("params");
        let definition: Value = serde_json::from_str(&params.definition).expect("definition json");

        assert_eq!(
            definition["headers"]["Authorization"],
            "Bearer ${vault:MCP_CONTEXT7_API_KEY}"
        );
    }

    #[test]
    fn hub_quick_add_mcp_cli_reserves_empty_seat_scope_policy() {
        let params = build_mcp_quick_add_params(
            "context7",
            "https://mcp.context7.com/mcp",
            "http",
            &[],
            None,
        )
        .expect("params");
        let definition: Value = serde_json::from_str(&params.definition).expect("definition json");

        assert_eq!(definition["policy"]["visibility"], "discoverable");
        assert_eq!(
            definition["policy"]["seats"]
                .as_array()
                .expect("seats array")
                .len(),
            0
        );
        assert_eq!(
            definition["policy"]["allowed_mcp_servers"]
                .as_array()
                .expect("allowed_mcp_servers array")
                .len(),
            0
        );
    }

    #[test]
    fn mcp_key_flag_uses_explicit_vault_header_key_name() {
        let key_name = select_mcp_vault_key_name(
            "context7",
            &["Authorization: Bearer ${vault:CONTEXT7_API_KEY}".to_string()],
        )
        .expect("key name");

        assert_eq!(key_name, "CONTEXT7_API_KEY");
    }

    #[test]
    fn mcp_key_flag_uses_explicit_x_api_key_vault_header_name() {
        let key_name = select_mcp_vault_key_name(
            "context7",
            &["X-API-Key: ${vault:CONTEXT7_API_KEY}".to_string()],
        )
        .expect("key name");
        let params = build_mcp_quick_add_params(
            "context7",
            "https://mcp.context7.com/mcp",
            "http",
            &["X-API-Key: ${vault:CONTEXT7_API_KEY}".to_string()],
            Some(&key_name),
        )
        .expect("params");
        let definition: Value = serde_json::from_str(&params.definition).expect("definition json");

        assert_eq!(key_name, "CONTEXT7_API_KEY");
        assert_eq!(
            definition["headers"]["X-API-Key"],
            "${vault:CONTEXT7_API_KEY}"
        );
        assert!(definition["headers"]["Authorization"].is_null());
    }

    #[test]
    fn mcp_key_flag_rejects_conflicting_secret_header_key_names() {
        let err = select_mcp_vault_key_name(
            "context7",
            &[
                "Authorization: Bearer ${vault:CONTEXT7_API_KEY}".to_string(),
                "X-API-Key: ${vault:OTHER_API_KEY}".to_string(),
            ],
        )
        .expect_err("conflicting key names must fail")
        .to_string();

        assert!(err.contains("multiple secret-bearing MCP headers"));
    }

    #[test]
    fn hub_quick_add_mcp_cli_rejects_raw_bearer_header() {
        let err = build_mcp_quick_add_params(
            "context7",
            "https://mcp.context7.com/mcp",
            "http",
            &[format!("Authorization: Bearer {SENTINEL_RAW_VALUE}")],
            None,
        )
        .expect_err("raw header must be rejected")
        .to_string();

        assert!(err.contains("secret-bearing MCP headers must use only"));
        assert!(!err.contains(SENTINEL_RAW_VALUE));
    }

    #[test]
    fn hub_quick_add_mcp_cli_rejects_mixed_raw_and_vault_bearer_header() {
        let err = build_mcp_quick_add_params(
            "context7",
            "https://mcp.context7.com/mcp",
            "http",
            &[format!(
                "Authorization: Bearer {SENTINEL_RAW_VALUE} ${{vault:KEY}}"
            )],
            None,
        )
        .expect_err("mixed raw and vault header must be rejected")
        .to_string();

        assert!(err.contains("secret-bearing MCP headers must use only"));
        assert!(!err.contains(SENTINEL_RAW_VALUE));
    }

    #[test]
    fn mcp_header_parse_errors_do_not_echo_header_values() {
        let err = parse_mcp_headers(&[format!("Authorization Bearer {SENTINEL_RAW_VALUE}")])
            .expect_err("malformed header must fail")
            .to_string();

        assert!(err.contains("invalid MCP header at index 0"));
        assert!(!err.contains(SENTINEL_RAW_VALUE));
    }
}

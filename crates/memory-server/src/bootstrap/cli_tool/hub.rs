use crate::cli::HubAction;
use crate::tool_params::{PackProjectParams, PackRegisterParams};
use memory_core::HubCapability;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;

use super::super::{evaluate_cli_capability_enabled, open_cli_store, print_pretty_json};

pub(super) async fn run_hub_command(
    action: HubAction,
    app_home: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let hub_db = crate::hub_cli::resolve_hub_db(None, app_home);
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
            crate::hub_cli::run(
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
            crate::hub_cli::run(&HubAction::Show { id }, &hub_db, app_home)
                .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::Packs { all } => {
            crate::hub_cli::run(&HubAction::Packs { all }, &hub_db, app_home)
                .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::PackRegister {
            id,
            local_path,
            name,
            source,
            version,
            description,
        } => {
            let server = crate::cli_client::build_in_process_server(&hub_db, None)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let out = crate::pack_ops::handle_pack_register(
                &server,
                PackRegisterParams {
                    id,
                    name,
                    source,
                    version,
                    description,
                    local_path: Some(local_path.display().to_string()),
                    metadata: None,
                },
            )
            .await
            .map_err(std::io::Error::other)?;
            print_pretty_json(&serde_json::from_str(&out)?)
        }
        HubAction::PackProject { pack_id, agents } => {
            let server = crate::cli_client::build_in_process_server(&hub_db, None)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let out = crate::pack_ops::handle_pack_project(
                &server,
                PackProjectParams { pack_id, agents },
            )
            .await
            .map_err(std::io::Error::other)?;
            print_pretty_json(&serde_json::from_str(&out)?)
        }
        HubAction::Bindings => {
            crate::hub_cli::run(&HubAction::Bindings, &hub_db, app_home)
                .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::Doctor { fix } => {
            crate::hub_cli::run(&HubAction::Doctor { fix }, &hub_db, app_home)
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
            crate::hub_cli::cmd_stats(&hub_db).map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(())
        }
    }
}

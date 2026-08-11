use crate::dispatch_profile::{DispatchProfileDef, DISPATCH_PROFILES};
use serde_json::{json, Value};
use std::path::PathBuf;
use tachi_bootstrap::cli::CardAction;
use tachi_dispatch::{
    compile_effective_contract, profile_resolved_model, ContractInputs, PermissionProfile,
    SkillRequest, PROVIDER_QUALIFICATIONS,
};

use super::super::print_pretty_json;

/// Local operator-only diagnostics for static dispatch profiles.
///
/// This command intentionally does not call `tachi_task` or the server's
/// profile/card JSON builders. It reads the static profile registry, compiles
/// the default workspace authority, and reports host admission as diagnostics;
/// it never approves or launches a dispatch.
pub(super) async fn run_card_command(
    action: CardAction,
    _db_path: &PathBuf,
    _project_db_path: Option<&PathBuf>,
    _app_home: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        CardAction::List { json } => {
            let profiles = operator_profile_list_json()?;
            if json {
                print_pretty_json(&profiles)
            } else {
                print_operator_profile_list(&profiles)
            }
        }
        CardAction::Show { id, json } => {
            let profile = find_static_profile(&id)
                .ok_or_else(|| format!("unknown operator profile '{id}'"))?;
            let detail = operator_profile_detail(profile)?;
            if json {
                print_pretty_json(&json!({
                    "schema_version": "tachi.operator_profile.v1",
                    "profile": detail,
                }))
            } else {
                print_operator_profile_detail(&detail)
            }
        }
    }
}

fn find_static_profile(id: &str) -> Option<&'static DispatchProfileDef> {
    let wanted = id.trim();
    DISPATCH_PROFILES
        .iter()
        .find(|profile| profile.name == wanted)
}

fn operator_profile_list_json() -> Result<Value, String> {
    let profiles = DISPATCH_PROFILES
        .iter()
        .map(operator_profile_summary)
        .collect::<Vec<_>>();
    Ok(json!({
        "schema_version": "tachi.operator_profile.v1",
        "profiles": profiles,
    }))
}

fn operator_profile_summary(profile: &DispatchProfileDef) -> Value {
    json!({
        "id": profile.name,
        "display_name": profile.display_name,
        "backend": profile.backend,
        "model": profile_resolved_model(profile),
        "role": profile.role,
        "stage": profile.stage,
        "tool_profile": profile.tool_profile,
        "github_read": profile.github_read,
        "write_actions": profile.write_actions,
    })
}

fn operator_profile_detail(profile: &DispatchProfileDef) -> Result<Value, String> {
    let admission = crate::host_profile::admit_execution_level(None);
    let workspace_authority_default = compile_operator_authority(profile)?;
    Ok(json!({
        "id": profile.name,
        "display_name": profile.display_name,
        "backend": profile.backend,
        "model": profile_resolved_model(profile),
        "role": profile.role,
        "stage": profile.stage,
        "tool_profile": profile.tool_profile,
        "github_read": profile.github_read,
        "write_actions": profile.write_actions,
        "allowed_facades": profile.allowed_facades,
        "allowed_mcp_servers": profile.allowed_mcp_servers,
        "inject_tachi_mcp": profile.inject_tachi_mcp,
        "inject_hub_mcps": profile.inject_hub_mcps,
        "workspace_authority_default": workspace_authority_default,
        "static_profile_admission": {
            "profile_registered": true,
            "reason_code": admission.reason_code,
            "host_profile": admission.host_profile,
            "host_profile_source": admission.profile_source,
            "host_max_execution_level": admission.max_execution_level.map(|level| level.as_str()),
        },
    }))
}

/// Compile the static profile's default authority. Provider certification is a
/// launch-time concern and is not needed for this diagnostic. If the real
/// backend is shell-capable and lacks a certification receipt, repeat the pure
/// authority compilation with a non-shell diagnostic backend; the profile
/// ceiling and default are unchanged, while the operator output remains
/// available without pretending to approve a launch.
fn compile_operator_authority(profile: &DispatchProfileDef) -> Result<String, String> {
    let skills: Vec<SkillRequest> = Vec::new();
    let allowed_tools: Vec<String> = Vec::new();
    let compile = |backend: &str| {
        compile_effective_contract(&ContractInputs {
            backend,
            transport: "cli",
            backend_version: None,
            profile: Some(profile),
            requested_sandbox: None,
            permission_profile: PermissionProfile::Default,
            allowed_tools: &allowed_tools,
            skills: &skills,
            mcp_write_actions: Some(profile.write_actions),
            mcp_github_read: Some(profile.github_read),
            qualifications: PROVIDER_QUALIFICATIONS,
        })
    };

    match compile(profile.backend) {
        Ok(contract) => Ok(contract.workspace_authority.as_str().to_string()),
        Err(real_backend_error) => compile("operator-diagnostic")
            .map(|contract| contract.workspace_authority.as_str().to_string())
            .map_err(|diagnostic_error| {
                format!(
                    "compile static authority for '{}': real backend: {real_backend_error}; diagnostic backend: {diagnostic_error}",
                    profile.name
                )
            }),
    }
}

fn print_operator_profile_list(profiles: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let rows = profiles["profiles"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("operator profile list lacks profiles array"))?;
    println!("Tachi Operator Profiles");
    println!(
        "{:<22} {:<30} {:<12} {:<12} {:<12} {:<14}",
        "id", "display_name", "backend", "role", "stage", "write_actions"
    );
    for row in rows {
        println!(
            "{:<22} {:<30} {:<12} {:<12} {:<12} {:<14}",
            display_value(row, "id"),
            display_value(row, "display_name"),
            display_value(row, "backend"),
            display_value(row, "role"),
            display_value(row, "stage"),
            display_value(row, "write_actions"),
        );
    }
    Ok(())
}

fn print_operator_profile_detail(profile: &Value) -> Result<(), Box<dyn std::error::Error>> {
    println!("Tachi Operator Profile");
    for field in [
        "id",
        "display_name",
        "backend",
        "model",
        "role",
        "stage",
        "tool_profile",
        "github_read",
        "write_actions",
        "allowed_facades",
        "allowed_mcp_servers",
        "inject_tachi_mcp",
        "inject_hub_mcps",
        "workspace_authority_default",
        "static_profile_admission",
    ] {
        println!("{field}: {}", profile.get(field).unwrap_or(&Value::Null));
    }
    Ok(())
}

fn display_value(value: &Value, field: &str) -> String {
    match value.get(field) {
        Some(Value::String(text)) => text.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Null) | None => "-".to_string(),
        Some(value) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys(value: &Value) -> BTreeSet<&str> {
        value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn operator_list_and_detail_have_exact_amended_shape() {
        let list = operator_profile_list_json().expect("operator list");
        assert_eq!(list["schema_version"], "tachi.operator_profile.v1");
        let rows = list["profiles"].as_array().expect("profiles");
        assert_eq!(rows.len(), DISPATCH_PROFILES.len());
        let summary_fields = BTreeSet::from([
            "id",
            "display_name",
            "backend",
            "model",
            "role",
            "stage",
            "tool_profile",
            "github_read",
            "write_actions",
        ]);
        for row in rows {
            assert_eq!(keys(row), summary_fields);
        }

        let detail = operator_profile_detail(&DISPATCH_PROFILES[0]).expect("operator detail");
        let detail_fields = BTreeSet::from([
            "id",
            "display_name",
            "backend",
            "model",
            "role",
            "stage",
            "tool_profile",
            "github_read",
            "write_actions",
            "allowed_facades",
            "allowed_mcp_servers",
            "inject_tachi_mcp",
            "inject_hub_mcps",
            "workspace_authority_default",
            "static_profile_admission",
        ]);
        assert_eq!(keys(&detail), detail_fields);
        assert_eq!(
            keys(&detail["static_profile_admission"]),
            BTreeSet::from([
                "profile_registered",
                "reason_code",
                "host_profile",
                "host_profile_source",
                "host_max_execution_level",
            ])
        );
        assert_eq!(
            detail["static_profile_admission"]["profile_registered"],
            true
        );
        assert!(matches!(
            detail["workspace_authority_default"].as_str(),
            Some("read-only") | Some("workspace-write") | Some("danger-full-access")
        ));
    }

    #[test]
    fn operator_projection_has_no_secret_or_card_overlay_fields() {
        let list = operator_profile_list_json().expect("operator list");
        let detail = operator_profile_detail(
            find_static_profile("opencode_builder").expect("static profile"),
        )
        .expect("operator detail");
        let serialized = format!("{list}{detail}");
        for forbidden in [
            "credential_profiles",
            "mbit_card",
            "archetype",
            "stats",
            "personality",
            "guidance",
            "moves",
            "skill_loadout",
            "strong_against",
            "weak_against",
            "evolution",
            "dispatch_profile_card_overlays",
            "TACHI_",
            "secret",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "operator projection leaked forbidden token {forbidden}: {serialized}"
            );
        }
        for profile in DISPATCH_PROFILES {
            for credential_profile in profile.credential_profiles {
                assert!(
                    !serialized.contains(credential_profile),
                    "operator projection leaked credential profile {credential_profile}: {serialized}"
                );
            }
        }
    }

    #[test]
    fn operator_projection_resolves_static_model_and_authority() {
        let summary =
            operator_profile_summary(find_static_profile("glm_impl").expect("static GLM profile"));
        assert!(summary["model"].as_str().is_some());
        let detail =
            operator_profile_detail(find_static_profile("glm_impl").expect("static GLM profile"))
                .expect("operator detail");
        assert_eq!(detail["workspace_authority_default"], "workspace-write");
    }
}

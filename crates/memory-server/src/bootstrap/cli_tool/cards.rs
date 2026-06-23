use crate::cli::CardAction;
use crate::tool_params::TachiTaskParams;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::super::print_pretty_json;
use super::tool_dispatch::dispatch_cli_tool;

pub(super) async fn run_card_command(
    action: CardAction,
    db_path: &PathBuf,
    project_db_path: Option<&PathBuf>,
    app_home: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let profiles = load_card_profiles(db_path, project_db_path, app_home).await?;
    match action {
        CardAction::List { json } => {
            if json {
                print_pretty_json(&profiles)
            } else {
                print_card_list(&profiles)
            }
        }
        CardAction::Show { id, json } => {
            let card = find_card_profile(&profiles, &id)
                .ok_or_else(|| format!("unknown Card/profile '{id}'"))?;
            if json {
                print_pretty_json(card)
            } else {
                print_card_show(card)
            }
        }
    }
}

async fn load_card_profiles(
    db_path: &PathBuf,
    project_db_path: Option<&PathBuf>,
    app_home: &PathBuf,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut args = serde_json::Map::new();
    args.insert("action".into(), json!("profiles"));
    let body = dispatch_cli_tool(
        "tachi_task",
        args,
        db_path,
        project_db_path,
        app_home,
        |server, args_map| {
            Box::pin(async move {
                let params: TachiTaskParams =
                    serde_json::from_value(serde_json::Value::Object(args_map))
                        .map_err(|e| format!("invalid tachi_task args: {e}"))?;
                server.tachi_task(Parameters(params)).await
            })
        },
    )
    .await?;
    serde_json::from_str(&body)
        .map_err(|e| format!("tachi_task profiles returned non-JSON output: {e}").into())
}

fn find_card_profile<'a>(profiles: &'a Value, id: &str) -> Option<&'a Value> {
    let wanted = id.trim();
    profiles
        .get("dispatch_profiles")
        .and_then(Value::as_array)?
        .iter()
        .find(|profile| profile.get("name").and_then(Value::as_str) == Some(wanted))
}

fn print_card_list(profiles: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let rows = profiles
        .get("dispatch_profiles")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            std::io::Error::other("tachi_task profiles response lacks dispatch_profiles array")
        })?;
    println!("Tachi Cards");
    println!(
        "{:<22} {:<8} {:<18} {:<12} {:<12} {:<10}",
        "id", "card", "role", "stage", "backend", "write"
    );
    for profile in rows {
        let name = profile.get("name").and_then(Value::as_str).unwrap_or("-");
        let archetype = profile
            .get("card_archetype")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let role = profile.get("role").and_then(Value::as_str).unwrap_or("-");
        let stage = profile.get("stage").and_then(Value::as_str).unwrap_or("-");
        let backend = profile
            .get("backend")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let write_code = profile
            .get("mbit_card")
            .and_then(|card| card.get("authority"))
            .and_then(|authority| authority.get("write_code"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        println!(
            "{:<22} {:<8} {:<18} {:<12} {:<12} {:<10}",
            name, archetype, role, stage, backend, write_code
        );
    }
    Ok(())
}

fn print_card_show(profile: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let null = Value::Null;
    let card = profile.get("mbit_card").unwrap_or(&null);
    let name = profile.get("name").and_then(Value::as_str).unwrap_or("-");
    let display_name = profile
        .get("display_name")
        .and_then(Value::as_str)
        .unwrap_or(name);
    println!("Tachi Card: {display_name}");
    println!("id: {name}");
    println!(
        "card: {}",
        card.get("archetype").and_then(Value::as_str).unwrap_or("-")
    );
    println!(
        "role: {}",
        profile.get("role").and_then(Value::as_str).unwrap_or("-")
    );
    println!(
        "stage: {}",
        profile.get("stage").and_then(Value::as_str).unwrap_or("-")
    );
    println!(
        "backend: {}",
        profile
            .get("backend")
            .and_then(Value::as_str)
            .unwrap_or("-")
    );
    println!(
        "authority: write_code={} merge={} github_write={}",
        card.get("authority")
            .and_then(|authority| authority.get("write_code"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        card.get("authority")
            .and_then(|authority| authority.get("merge"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        card.get("authority")
            .and_then(|authority| authority.get("github_write"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    println!(
        "guidance: {}",
        json_array_strings(
            card.get("guidance")
                .and_then(|guidance| guidance.get("superpowers"))
        )
    );
    println!(
        "moves: {}",
        json_array_strings(card.get("moves").and_then(|moves| moves.get("waza")))
    );
    println!(
        "evidence: {}",
        json_array_strings(
            card.get("evidence_contract")
                .and_then(|contract| contract.get("required"))
        )
    );
    Ok(())
}

fn json_array_strings(value: Option<&Value>) -> String {
    let items: Vec<&str> = value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if items.is_empty() {
        "-".to_string()
    } else {
        items.join(", ")
    }
}

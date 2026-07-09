use crate::tool_params::TachiTaskParams;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use std::path::PathBuf;
use tachi_bootstrap::cli::CardAction;

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
                print_pretty_json(&card_list_json(&profiles))
            } else {
                print_card_list(&profiles)
            }
        }
        CardAction::Show { id, json } => {
            let card = find_card_profile(&profiles, &id)
                .ok_or_else(|| format!("unknown Card/profile '{id}'"))?;
            if json {
                print_pretty_json(&card_show_json(card))
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

fn card_list_json(profiles: &Value) -> Value {
    let cards = profiles
        .get("dispatch_profiles")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(compact_card_json).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut output = profiles.clone();
    if let Some(object) = output.as_object_mut() {
        object.insert("schema_version".to_string(), json!("tachi.cards.list.v1"));
        object.insert("cards".to_string(), Value::Array(cards));
        output
    } else {
        json!({
            "schema_version": "tachi.cards.list.v1",
            "cards": cards,
            "profiles": profiles,
        })
    }
}

fn card_show_json(profile: &Value) -> Value {
    let compact = compact_card_json(profile);
    let mut output = profile.clone();
    if let Some(object) = output.as_object_mut() {
        object.insert("schema_version".to_string(), json!("tachi.card.show.v1"));
        object.insert("card".to_string(), compact);
        output
    } else {
        json!({
            "schema_version": "tachi.card.show.v1",
            "card": compact,
            "profile": profile,
        })
    }
}

fn compact_card_json(profile: &Value) -> Value {
    let card = profile.get("mbit_card");
    let name = profile.get("name").and_then(Value::as_str).unwrap_or("-");
    let display_name = profile
        .get("display_name")
        .and_then(Value::as_str)
        .unwrap_or(name);
    let archetype = first_json_value(&[
        card.and_then(|card| card.get("archetype")),
        profile.get("card_archetype"),
    ]);
    let skill_loadout = first_json_value(&[
        card.and_then(|card| card.get("skill_loadout")),
        profile.get("skill_loadout"),
    ]);
    let evidence_contract = first_json_value(&[
        card.and_then(|card| card.get("evidence_contract")),
        profile.get("evidence_contract"),
    ]);

    json!({
        "id": name,
        "profile_id": name,
        "display_name": display_name,
        "archetype": archetype,
        "role": first_json_value(&[profile.get("role")]),
        "stage": first_json_value(&[profile.get("stage")]),
        "backend": first_json_value(&[profile.get("backend")]),
        "host_adapter": first_json_value(&[profile.get("host_adapter")]),
        "tool_profile": first_json_value(&[profile.get("tool_profile")]),
        "authority": first_json_value(&[card.and_then(|card| card.get("authority"))]),
        "guidance": first_json_value(&[card.and_then(|card| card.get("guidance"))]),
        "moves": first_json_value(&[card.and_then(|card| card.get("moves"))]),
        "skill_loadout": skill_loadout,
        "evidence_contract": evidence_contract,
        "strengths": first_json_value(&[
            card.and_then(|card| card.get("strong_against")),
            profile.get("strong_against"),
        ]),
        "weaknesses": first_json_value(&[
            card.and_then(|card| card.get("weak_against")),
            profile.get("weak_against"),
        ]),
        "evolution": first_json_value(&[card.and_then(|card| card.get("evolution"))]),
    })
}

fn first_json_value(values: &[Option<&Value>]) -> Value {
    values
        .iter()
        .find_map(|value| value.as_ref().copied().filter(|value| !value.is_null()))
        .cloned()
        .unwrap_or(Value::Null)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profile() -> Value {
        json!({
            "name": "codex_55_review",
            "display_name": "Codex 5.5 Review",
            "card_archetype": "raven",
            "role": "reviewer",
            "stage": "review",
            "backend": "codex",
            "tool_profile": "delegate",
            "weak_against": ["direct_merge"],
            "skill_loadout": {
                "common_skills": ["skill:superpowers-requesting-code-review"]
            },
            "evidence_contract": {
                "required": ["findings", "verification"]
            },
            "mbit_card": {
                "archetype": "raven",
                "authority": {
                    "write_code": false,
                    "merge": false,
                    "github_write": false
                },
                "guidance": {
                    "superpowers": ["skill:superpowers-requesting-code-review"]
                },
                "moves": {
                    "waza": ["skill:waza-check"],
                    "external": []
                },
                "strong_against": ["review"],
                "weak_against": ["direct_merge"],
                "evolution": {
                    "status": "baseline"
                }
            }
        })
    }

    #[test]
    fn card_list_json_adds_stable_cards_array_without_dropping_profiles() {
        let profiles = json!({
            "dispatch_profiles": [sample_profile()],
            "projection_namespace": "dispatch_profile_card_overlays"
        });

        let rendered = card_list_json(&profiles);

        assert_eq!(rendered["schema_version"], json!("tachi.cards.list.v1"));
        assert_eq!(
            rendered["dispatch_profiles"][0]["name"],
            json!("codex_55_review")
        );
        assert_eq!(rendered["cards"][0]["profile_id"], json!("codex_55_review"));
        assert_eq!(rendered["cards"][0]["archetype"], json!("raven"));
        assert_eq!(
            rendered["cards"][0]["moves"]["waza"][0],
            json!("skill:waza-check")
        );
    }

    #[test]
    fn card_show_json_adds_compact_card_alias() {
        let rendered = card_show_json(&sample_profile());

        assert_eq!(rendered["schema_version"], json!("tachi.card.show.v1"));
        assert_eq!(rendered["name"], json!("codex_55_review"));
        assert_eq!(rendered["card"]["id"], json!("codex_55_review"));
        assert_eq!(rendered["card"]["authority"]["merge"], json!(false));
        assert_eq!(
            rendered["card"]["evidence_contract"]["required"][0],
            json!("findings")
        );
    }

    #[test]
    fn compact_card_json_preserves_host_adapter() {
        let profile = json!({
            "name": "opencode_builder",
            "display_name": "OpenCode Credentialed Builder",
            "backend": "opencode",
            "host_adapter": "opencode",
            "role": "executor",
            "stage": "execute",
            "mbit_card": {
                "archetype": "scv",
                "authority": {"write_code": true}
            }
        });

        let rendered = compact_card_json(&profile);

        assert_eq!(rendered["backend"], json!("opencode"));
        assert_eq!(rendered["host_adapter"], json!("opencode"));
    }
}

use super::{
    evaluate_cli_capability_enabled, open_cli_store, open_cli_store_read_only, print_pretty_json,
};
use crate::cli::{CardAction, Commands, HubAction};
use crate::kanban::{gc_expired_kanban_cards, DEFAULT_KANBAN_GC_MAX_AGE_DAYS};
use crate::server_state::MemoryServer;
use crate::tool_params::{
    ExtractFactsParams, GetMemoryParams, ListMemoriesParams, PackProjectParams, PackRegisterParams,
    RememberParams, SearchMemoryParams, TachiTaskParams, WikiSearchParams, WikiWriteParams,
};
use memory_core::HubCapability;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

pub(super) async fn run_cli_command(
    command: Commands,
    db_path: &PathBuf,
    project_db_path: Option<&PathBuf>,
    app_home: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Commands::Serve => Ok(()),
        Commands::Search {
            query,
            path,
            top_k,
            project,
        } => {
            let mut args = serde_json::Map::new();
            args.insert("query".into(), json!(query));
            if let Some(v) = &path {
                args.insert("path_prefix".into(), json!(v));
            }
            args.insert("top_k".into(), json!(top_k));
            if let Some(v) = &project {
                args.insert("project".into(), json!(v));
            }

            let body = dispatch_cli_tool(
                "search_memory",
                args,
                db_path,
                project_db_path,
                app_home,
                |server, args_map| {
                    Box::pin(async move {
                        let params: SearchMemoryParams =
                            serde_json::from_value(serde_json::Value::Object(args_map))
                                .map_err(|e| format!("invalid search_memory args: {e}"))?;
                        crate::memory_search_ops::handle_search_memory_with_access(
                            &server, params, false, true,
                        )
                        .await
                    })
                },
            )
            .await?;
            print_cli_tool_result(&body)
        }
        // `Commands::Save` was removed in favor of `Commands::Remember` which
        // carries `#[command(alias = "save")]`. The dispatcher therefore only
        // sees `Remember`, even when the user typed `tachi save TEXT`.
        Commands::Stats => {
            // Respect `--project-db` when supplied so `tachi --project-db X stats`
            // actually reports on DB X instead of silently falling back to global.
            let target_path = project_db_path.unwrap_or(db_path);
            let store = open_cli_store_read_only(target_path)?;
            let stats = store.stats(false)?;
            print_pretty_json(&json!({
                "total": stats.total,
                "by_scope": stats.by_scope,
                "by_category": stats.by_category,
                "by_root_path": stats.by_root_path,
                "warnings": ["stats opened database read-only; no maintenance writes attempted"],
                "database": {
                    "path": target_path.display().to_string(),
                    "vec_available": store.vec_available,
                }
            }))
        }
        Commands::Gc => {
            let mut store = open_cli_store(db_path)?;
            let mut gc = store.gc_tables(&memory_core::GcConfig::default())?;
            let kanban_deleted =
                gc_expired_kanban_cards(&mut store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
            if let Some(object) = gc.as_object_mut() {
                object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
            }
            print_pretty_json(&gc)
        }
        Commands::BackfillVectors { .. } => {
            unreachable!("BackfillVectors is handled in async context before this point")
        }
        Commands::BackfillSummaries { .. } => {
            unreachable!("BackfillSummaries is handled in async context before this point")
        }
        Commands::BackfillMetadata { .. } => {
            unreachable!("BackfillMetadata is handled in async context before this point")
        }
        Commands::BackfillFts { .. } => {
            unreachable!("BackfillFts is handled before generic CLI dispatch")
        }
        Commands::Distill { .. } => {
            unreachable!("Distill is handled in async context before generic CLI dispatch")
        }
        Commands::Vault { .. } => {
            unreachable!("Vault is handled in async context before generic CLI dispatch")
        }
        Commands::Env { .. } => {
            unreachable!("Env is handled in async context before generic CLI dispatch")
        }
        Commands::Setup { .. } => {
            unreachable!("Setup is handled in async context before generic CLI dispatch")
        }
        Commands::Tidy { .. } => {
            unreachable!("Tidy is handled in async context before generic CLI dispatch")
        }
        Commands::Clean { .. } => {
            unreachable!("Clean is handled in async context before generic CLI dispatch")
        }
        Commands::Harness { .. } => {
            unreachable!("Harness is handled in async context before generic CLI dispatch")
        }
        Commands::SkillSurface { .. } => {
            unreachable!("SkillSurface is handled in async context before generic CLI dispatch")
        }
        Commands::Poke { .. } => {
            unreachable!("Poke is handled in async context before generic CLI dispatch")
        }
        Commands::Card { action } => {
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
        Commands::Hub { action } => {
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
                    let (enabled, warning) =
                        evaluate_cli_capability_enabled(&cap_type, &definition)?;
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
                    crate::hub_cli::cmd_stats(&hub_db)
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                    Ok(())
                }
            }
        }
        Commands::Doctor { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
        Commands::Manifest { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
        Commands::Rescue { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
        Commands::Status { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
        Commands::Daemon { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
        Commands::Watcher { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
        Commands::Foundry { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
        Commands::Repair { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
        Commands::Wiki { action } => match action {
            crate::cli::WikiAction::Export {
                format,
                output,
                project,
            } => {
                if !format.eq_ignore_ascii_case("obsidian") {
                    return Err(format!(
                        "unsupported wiki export format '{format}' (expected obsidian)"
                    )
                    .into());
                }
                let output = if output == PathBuf::from("~") {
                    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
                } else if let Some(rest) = output.to_string_lossy().strip_prefix("~/") {
                    dirs::home_dir()
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join(rest)
                } else {
                    output
                };
                let server = MemoryServer::new(db_path.clone(), project_db_path.cloned())?;
                let result = crate::wiki_ops::export_wiki_obsidian(&server, &project, &output)
                    .map_err(std::io::Error::other)?;
                print_pretty_json(&result)
            }
        },
        Commands::Remember {
            text,
            tags,
            scope,
            project,
            path,
            importance,
            category,
            topic,
            domain,
            retention_policy,
            summary,
            force,
        } => {
            let mut args = serde_json::Map::new();
            args.insert("text".into(), json!(text));
            if !tags.is_empty() {
                args.insert("tags".into(), json!(tags));
            }
            if let Some(v) = &scope {
                args.insert("scope".into(), json!(v));
            }
            if let Some(v) = &project {
                args.insert("project".into(), json!(v));
            }
            if let Some(v) = &path {
                args.insert("path".into(), json!(v));
            }
            if let Some(v) = importance {
                args.insert("importance".into(), json!(v));
            }
            if let Some(v) = &category {
                args.insert("category".into(), json!(v));
            }
            if let Some(v) = &topic {
                args.insert("topic".into(), json!(v));
            }
            if let Some(v) = &domain {
                args.insert("domain".into(), json!(v));
            }
            if let Some(v) = &retention_policy {
                args.insert("retention_policy".into(), json!(v));
            }
            if let Some(v) = &summary {
                args.insert("summary".into(), json!(v));
            }
            if force {
                args.insert("force".into(), json!(true));
            }

            let body = dispatch_cli_tool(
                "remember",
                args,
                db_path,
                project_db_path,
                app_home,
                |server, args_map| {
                    Box::pin(async move {
                        let params: RememberParams =
                            serde_json::from_value(serde_json::Value::Object(args_map))
                                .map_err(|e| format!("invalid remember args: {e}"))?;
                        crate::memory_search_ops::handle_remember(&server, params).await
                    })
                },
            )
            .await?;
            print_cli_tool_result(&body)
        }
        Commands::WikiSearch {
            query,
            category,
            top_k,
            project,
        } => {
            let mut args = serde_json::Map::new();
            args.insert("query".into(), json!(query));
            args.insert("top_k".into(), json!(top_k));
            if let Some(v) = &category {
                args.insert("category".into(), json!(v));
            }
            if let Some(v) = &project {
                args.insert("project".into(), json!(v));
            }

            let body = dispatch_cli_tool(
                "tachi_wiki_search",
                args,
                db_path,
                project_db_path,
                app_home,
                |server, args_map| {
                    Box::pin(async move {
                        let params: WikiSearchParams =
                            serde_json::from_value(serde_json::Value::Object(args_map))
                                .map_err(|e| format!("invalid wiki_search args: {e}"))?;
                        crate::wiki_ops::handle_wiki_search(&server, params).await
                    })
                },
            )
            .await?;
            print_cli_tool_result(&body)
        }
        Commands::WikiWrite {
            title,
            text,
            path,
            topic,
            summary,
            keywords,
            entities,
            importance,
            scope,
            project,
            domain,
            force,
        } => {
            let mut args = serde_json::Map::new();
            args.insert("title".into(), json!(title));
            args.insert("text".into(), json!(text));
            if let Some(v) = &path {
                args.insert("path".into(), json!(v));
            }
            if let Some(v) = &topic {
                args.insert("topic".into(), json!(v));
            }
            if let Some(v) = &summary {
                args.insert("summary".into(), json!(v));
            }
            if !keywords.is_empty() {
                args.insert("keywords".into(), json!(keywords));
            }
            if !entities.is_empty() {
                args.insert("entities".into(), json!(entities));
            }
            if let Some(v) = importance {
                args.insert("importance".into(), json!(v));
            }
            if let Some(v) = &scope {
                args.insert("scope".into(), json!(v));
            }
            if let Some(v) = &project {
                args.insert("project".into(), json!(v));
            }
            if let Some(v) = &domain {
                args.insert("domain".into(), json!(v));
            }
            if force {
                args.insert("force".into(), json!(true));
            }

            let body = dispatch_cli_tool(
                "tachi_wiki_write",
                args,
                db_path,
                project_db_path,
                app_home,
                |server, args_map| {
                    Box::pin(async move {
                        let params: WikiWriteParams =
                            serde_json::from_value(serde_json::Value::Object(args_map))
                                .map_err(|e| format!("invalid wiki_write args: {e}"))?;
                        crate::copilot_ops::handle_tachi_wiki_write(&server, params).await
                    })
                },
            )
            .await?;
            print_cli_tool_result(&body)
        }
        Commands::List {
            path_prefix,
            limit,
            include_archived,
        } => {
            let mut args = serde_json::Map::new();
            args.insert("path_prefix".into(), json!(path_prefix));
            args.insert("limit".into(), json!(limit));
            if include_archived {
                args.insert("include_archived".into(), json!(true));
            }

            let body = dispatch_cli_tool(
                "list_memories",
                args,
                db_path,
                project_db_path,
                app_home,
                |server, args_map| {
                    Box::pin(async move {
                        let params: ListMemoriesParams =
                            serde_json::from_value(serde_json::Value::Object(args_map))
                                .map_err(|e| format!("invalid list_memories args: {e}"))?;
                        crate::memory_ops::handle_list_memories(&server, params).await
                    })
                },
            )
            .await?;
            print_cli_tool_result(&body)
        }
        Commands::Get {
            id,
            project,
            include_archived,
        } => {
            let mut args = serde_json::Map::new();
            args.insert("id".into(), json!(id));
            if let Some(v) = &project {
                args.insert("project".into(), json!(v));
            }
            if include_archived {
                args.insert("include_archived".into(), json!(true));
            }

            let body = dispatch_cli_tool(
                "get_memory",
                args,
                db_path,
                project_db_path,
                app_home,
                |server, args_map| {
                    Box::pin(async move {
                        let params: GetMemoryParams =
                            serde_json::from_value(serde_json::Value::Object(args_map))
                                .map_err(|e| format!("invalid get_memory args: {e}"))?;
                        crate::memory_ops::handle_get_memory(&server, params).await
                    })
                },
            )
            .await?;
            print_cli_tool_result(&body)
        }
        Commands::Extract { text, source } => {
            let mut args = serde_json::Map::new();
            args.insert("text".into(), json!(text));
            args.insert("source".into(), json!(source));

            let body = dispatch_cli_tool(
                "extract_facts",
                args,
                db_path,
                project_db_path,
                app_home,
                |server, args_map| {
                    Box::pin(async move {
                        let params: ExtractFactsParams =
                            serde_json::from_value(serde_json::Value::Object(args_map))
                                .map_err(|e| format!("invalid extract_facts args: {e}"))?;
                        crate::pipeline_ops::handle_extract_facts(&server, params).await
                    })
                },
            )
            .await?;
            print_cli_tool_result(&body)
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

/// Pretty-print a tool's JSON-string output. Falls back to raw text if the
/// result is not valid JSON (defensive: handlers always return JSON today).
fn print_cli_tool_result(body: &str) -> Result<(), Box<dyn std::error::Error>> {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => print_pretty_json(&v),
        Err(_) => {
            println!("{body}");
            Ok(())
        }
    }
}

fn cli_tool_allows_read_fallback(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "search_memory"
            | "get_memory"
            | "tachi_search"
            | "wiki_search"
            | "tachi_wiki_search"
            | "vault_status"
            | "vault_list"
    )
}

/// Dispatch a CLI tool invocation: try the running daemon first; on miss,
/// build a transient in-process MemoryServer and call the handler directly.
/// Either path returns the tool's JSON string body.
async fn dispatch_cli_tool<F, Fut>(
    tool_name: &str,
    args: serde_json::Map<String, serde_json::Value>,
    global_db: &PathBuf,
    project_db: Option<&PathBuf>,
    app_home: &PathBuf,
    in_process: F,
) -> Result<String, Box<dyn std::error::Error>>
where
    F: FnOnce(MemoryServer, serde_json::Map<String, serde_json::Value>) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let read_fallback = cli_tool_allows_read_fallback(tool_name);
    if let Some(info) = crate::cli_client::detect_daemon_for_global_db(app_home, global_db).await {
        if crate::cli_client::daemon_matches_requested_dbs(
            &info,
            global_db,
            project_db.map(|path| path.as_path()),
        ) {
            match crate::cli_client::call_daemon_tool(&info, tool_name, args.clone()).await {
                Ok(body) => return Ok(body),
                Err(e) if read_fallback || e.allows_in_process_fallback() => {
                    eprintln!(
                        "[cli] daemon dispatch failed ({e}); falling back to in-process execution"
                    );
                }
                Err(e) => return Err(format!(
                    "daemon dispatch failed after dispatch; refusing in-process fallback to avoid duplicate writes: {e}"
                ).into()),
            }
        } else if crate::cli_client::daemon_global_db_matches(&info, global_db) {
            if let Some(named_project) =
                project_db.and_then(|path| crate::path_utils::named_project_for_db_path(path))
            {
                let mut daemon_args = args.clone();
                daemon_args
                    .entry("project".to_string())
                    .or_insert_with(|| json!(named_project.clone()));
                eprintln!(
                    "[cli] daemon project DB scope differs; forwarding via named project '{named_project}'"
                );
                match crate::cli_client::call_daemon_tool(&info, tool_name, daemon_args).await {
                    Ok(body) => return Ok(body),
                    Err(e) if read_fallback || e.allows_in_process_fallback() => {
                        eprintln!(
                            "[cli] daemon named-project dispatch failed ({e}); falling back to in-process execution"
                        );
                    }
                    Err(e) => return Err(format!(
                        "daemon named-project dispatch failed after dispatch; refusing in-process fallback to avoid duplicate writes: {e}"
                    ).into()),
                }
            } else {
                eprintln!(
                    "[cli] daemon DB scope differs from requested CLI scope; executing in-process"
                );
            }
        } else {
            eprintln!(
                "[cli] foreign daemon global_db={:?}; executing in-process",
                info.global_db
            );
        }
    }
    let server = crate::cli_client::build_in_process_server(global_db, project_db)?;
    let body = in_process(server, args)
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(body)
}

mod cards;
mod hub;
mod tool_dispatch;

use super::{open_cli_store, open_cli_store_read_only, print_pretty_json};
use crate::kanban::{gc_expired_kanban_cards, DEFAULT_KANBAN_GC_MAX_AGE_DAYS};
use crate::server_state::MemoryServer;
use crate::tool_params::{
    ExtractFactsParams, GetMemoryParams, ListMemoriesParams, RememberParams, SearchMemoryParams,
    WikiSearchParams, WikiWriteParams,
};
use serde_json::json;
use std::path::PathBuf;
use tachi_bootstrap::cli::Commands;

use self::tool_dispatch::{dispatch_cli_tool, print_cli_tool_result};

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
        Commands::Worktree { .. } => {
            unreachable!("Worktree is handled in async context before generic CLI dispatch")
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
            cards::run_card_command(action, db_path, project_db_path, app_home).await
        }
        Commands::Hub { action } => hub::run_hub_command(action, app_home).await,
        Commands::Mcp { action } => hub::run_mcp_command(action, app_home).await,
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
            tachi_bootstrap::cli::WikiAction::Export {
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

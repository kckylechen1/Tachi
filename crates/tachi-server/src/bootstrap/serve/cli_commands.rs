use std::error::Error;
use std::path::PathBuf;
use tachi_bootstrap::cli::Commands;

pub(super) fn run_if_a2a_command(
    command: &Commands,
    app_home: &std::path::Path,
    global_db_path: &std::path::Path,
) -> Result<bool, Box<dyn Error>> {
    let Commands::A2a { action } = command else {
        return Ok(false);
    };
    super::super::sticky_cutover_cli::run(global_db_path, app_home, action.clone())?;
    Ok(true)
}

/// Operator-authorized ACP capability grant/revoke on admitted
/// AgentIdentity rows (#1960). Runs before generic CLI tool dispatch and
/// never inside an agent-facing tool session: the acting operator is a
/// host CLI user, and a caller-supplied actor equal to the target identity
/// refuses (no self-grant).
pub(super) fn run_if_agent_acp_command(
    command: &Commands,
    global_db_path: &std::path::Path,
) -> Result<bool, Box<dyn Error>> {
    let Commands::AgentAcp { action } = command else {
        return Ok(false);
    };
    let mut store = memcore::MemoryStore::open(&global_db_path.to_string_lossy())?;
    let outcome = match &action {
        tachi_bootstrap::cli::AgentAcpAction::Grant {
            identity,
            tool_profiles,
            capability_classes,
            actor,
        } => store.grant_agent_acp_capability(
            identity,
            &memcore::db::agent_capability::AcpCapabilityGrant {
                tool_profiles: tool_profiles.clone(),
                capability_classes: capability_classes.clone(),
            },
            actor,
        ),
        tachi_bootstrap::cli::AgentAcpAction::Revoke { identity, actor } => {
            store.revoke_agent_acp_capability(identity, actor)
        }
    }?;
    println!("{outcome}");
    Ok(true)
}

pub(super) async fn run_pre_serve_command(
    command: &Commands,
    home: &PathBuf,
    app_home: &PathBuf,
    global_db_path: &PathBuf,
    project_db_path: Option<&PathBuf>,
    git_root: Option<&PathBuf>,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<bool, Box<dyn Error>> {
    match command {
        Commands::Setup {
            json,
            interactive,
            non_interactive,
        } => {
            super::super::setup::run_setup_command(
                *json,
                *interactive,
                *non_interactive,
                home,
                app_home,
                global_db_path,
                project_db_path,
                git_root,
            )
            .await?;
            Ok(true)
        }
        Commands::Tidy {
            json,
            apply,
            dry_run,
            execute,
            yes,
            target_db,
        } => {
            let mut roots = vec![
                app_home.clone(),
                home.join(".sigil"),
                home.join(".gemini"),
                home.join(".openclaw"),
            ];
            if let Some(root) = git_root {
                roots.push(root.clone());
            }
            super::super::tidy::run_tidy_command(
                *json,
                *apply,
                *dry_run,
                *execute,
                *yes,
                target_db.clone(),
                home,
                app_home,
                roots,
                git_root,
            )
            .await?;
            Ok(true)
        }
        Commands::Clean { action } => {
            super::super::clean_cli::run_clean_command(action.clone()).await?;
            Ok(true)
        }
        Commands::Worktree { action } => {
            super::super::clean_cli::run_worktree_command(action.clone()).await?;
            Ok(true)
        }
        Commands::Build { action } => {
            super::super::build_cli::run_build_command(action.clone()).await?;
            Ok(true)
        }
        Commands::Harness { action } => {
            super::super::harness_cli::run_harness_command(action.clone()).await?;
            Ok(true)
        }
        Commands::Host { action } => {
            crate::host_profile::run_cli(action.clone(), app_home)?;
            Ok(true)
        }
        Commands::SkillSurface { action } => {
            super::super::skill_surface_cli::run_skill_surface_command(action.clone()).await?;
            Ok(true)
        }
        Commands::InjectionSurface { action } => {
            super::super::injection_surface_cli::run_injection_surface_command(action.clone())
                .await?;
            Ok(true)
        }
        Commands::Doctor {
            json,
            fix,
            scan_only: _,
            roots,
            jobs,
            probe_keys,
            run_daily,
        } => {
            super::super::manifest_cli::run_doctor_command(
                *json,
                *fix,
                roots.clone(),
                *jobs,
                *probe_keys,
                *run_daily,
                home,
                app_home,
                global_db_path,
                project_db_path.map(PathBuf::as_path),
                git_root,
                schema_migration,
            )
            .await?;
            Ok(true)
        }
        Commands::Gc { action } => {
            crate::repair::memory_maintenance::run_gc(action.clone(), app_home)?;
            Ok(true)
        }
        Commands::Delete { action } => {
            crate::repair::memory_maintenance::run_delete(action.clone(), app_home)?;
            Ok(true)
        }
        Commands::Manifest { action } => {
            super::super::manifest_cli::run_manifest_command(
                action.clone(),
                home,
                app_home,
                git_root,
            )
            .await?;
            Ok(true)
        }
        Commands::Migrate {
            json,
            apply,
            rename_legacy,
            offline,
        } => {
            // #1223: this subcommand's own `--apply` flag is its own,
            // independent schema-migration authorization decision (see
            // `migrate_cli`'s module doc comment) — deliberately NOT the
            // shared top-level `schema_migration` this function threads to
            // every other pre-serve command.
            if *rename_legacy {
                super::super::migrate_cli::run_filename_conversion_command(
                    *json,
                    *apply,
                    *offline,
                    app_home,
                    global_db_path,
                    project_db_path.map(PathBuf::as_path),
                )
                .await?;
            } else {
                if *offline {
                    return Err("--offline only applies to --rename-legacy --apply".into());
                }
                super::super::migrate_cli::run_migrate_command(
                    *json,
                    *apply,
                    app_home,
                    global_db_path,
                    project_db_path.map(PathBuf::as_path),
                )
                .await?;
            }
            Ok(true)
        }
        Commands::Rescue { action } => {
            super::super::rescue_cli::run_rescue_command(action.clone(), home).await?;
            Ok(true)
        }
        Commands::Status {
            watch,
            json,
            hide_orphans,
            probe_keys,
            all_dbs,
        } => {
            crate::status_ops::status_cli::run_status(
                *watch,
                *json,
                *hide_orphans,
                *probe_keys,
                *all_dbs,
                app_home,
                global_db_path,
                project_db_path.map(PathBuf::as_path),
            )
            .await?;
            Ok(true)
        }
        Commands::Daemon { action } => {
            crate::status_ops::status_cli::run_daemon(action.clone(), app_home, global_db_path)
                .await?;
            Ok(true)
        }
        Commands::Watcher { action } => {
            crate::status_ops::status_cli::run_watcher(
                action.clone(),
                global_db_path,
                project_db_path.cloned(),
            )
            .await?;
            Ok(true)
        }
        Commands::Foundry { action } => {
            crate::status_ops::status_cli::run_foundry(action.clone(), app_home, global_db_path)
                .await?;
            Ok(true)
        }
        Commands::Repair {
            action,
            db,
            rule,
            apply,
            no_backup,
            json,
            purge_failed,
        } => {
            crate::repair::run_repair(
                action.clone(),
                db.clone(),
                rule.clone(),
                *apply,
                *no_backup,
                *json,
                *purge_failed,
                app_home,
            )
            .await?;
            Ok(true)
        }
        Commands::Broker { action } => {
            super::super::broker_cli::run_broker_command(global_db_path, action.clone())?;
            Ok(true)
        }
        Commands::Vault { action } => {
            super::super::vault_cli::run_vault_command(global_db_path, app_home, action.clone())
                .await?;
            Ok(true)
        }
        Commands::Env {
            action,
            filter,
            env_only,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            super::super::env_cmd::run_env_command(
                global_db_path,
                action.clone(),
                filter.as_deref(),
                *env_only,
                *stdin_password,
                *keychain,
                password_file.as_deref(),
                *insecure_password_file,
            )
            .await?;
            Ok(true)
        }
        Commands::Poke { action } => {
            super::super::poke_cli::run_poke_command(app_home, action.clone()).await?;
            Ok(true)
        }
        Commands::Eval { action } => {
            super::super::eval_cli::run_eval_command(
                action.clone(),
                global_db_path,
                project_db_path,
                app_home,
            )
            .await?;
            Ok(true)
        }
        Commands::Clanker { action } => {
            super::super::clanker_cli::run_clanker_command(action.clone(), global_db_path).await?;
            Ok(true)
        }
        Commands::Serve => Ok(false),
        _ => {
            super::super::cli_tool::run_cli_command(
                command.clone(),
                global_db_path,
                project_db_path,
                app_home,
                schema_migration,
            )
            .await?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod agent_acp_cli_tests {
    use super::*;

    #[test]
    fn operator_cli_grants_then_revokes_the_acp_capability() {
        let temp = tempfile::tempdir().expect("tempdir");
        let global_db = temp.path().join("tachi-memory.db");
        // Initialize the schema and an admitted identity row.
        {
            let mut store = memcore::MemoryStore::open(global_db.to_str().unwrap()).unwrap();
            store
                .connection_mut()
                .execute(
                    "INSERT INTO agent_identities (agent_identity_id, created_at) \
                     VALUES ('zc-a2-e2e', '2026-10-29T00:00:00Z')",
                    [],
                )
                .unwrap();
        }

        let grant = Commands::AgentAcp {
            action: tachi_bootstrap::cli::AgentAcpAction::Grant {
                identity: "zc-a2-e2e".to_string(),
                tool_profiles: vec!["observe".to_string(), "delegate".to_string()],
                capability_classes: vec!["tachi".to_string()],
                actor: "operator-kyle".to_string(),
            },
        };
        assert!(run_if_agent_acp_command(&grant, &global_db).unwrap());

        {
            let store = memcore::MemoryStore::open(global_db.to_str().unwrap()).unwrap();
            let capability: Option<String> = store
                .connection()
                .query_row(
                    "SELECT capability_json FROM agent_identities \
                     WHERE agent_identity_id = 'zc-a2-e2e'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            memcore::db::agent_capability::acp_grant_allows_attachment(
                capability.as_deref(),
                "zc-a2-e2e",
                "delegate",
                "tachi",
            )
            .expect("granted envelope admits an attachment");
        }

        let revoke = Commands::AgentAcp {
            action: tachi_bootstrap::cli::AgentAcpAction::Revoke {
                identity: "zc-a2-e2e".to_string(),
                actor: "operator-kyle".to_string(),
            },
        };
        assert!(run_if_agent_acp_command(&revoke, &global_db).unwrap());

        let store = memcore::MemoryStore::open(global_db.to_str().unwrap()).unwrap();
        let capability: Option<String> = store
            .connection()
            .query_row(
                "SELECT capability_json FROM agent_identities \
                 WHERE agent_identity_id = 'zc-a2-e2e'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(capability.is_none(), "revoked identity carries no envelope");
    }
}

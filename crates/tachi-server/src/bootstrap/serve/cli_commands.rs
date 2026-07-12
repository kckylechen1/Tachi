use std::error::Error;
use std::path::PathBuf;
use tachi_bootstrap::cli::Commands;

pub(super) async fn run_pre_serve_command(
    command: &Commands,
    home: &PathBuf,
    app_home: &PathBuf,
    global_db_path: &PathBuf,
    project_db_path: Option<&PathBuf>,
    git_root: Option<&PathBuf>,
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
            )
            .await?;
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
        Commands::Serve => Ok(false),
        _ => {
            super::super::cli_tool::run_cli_command(
                command.clone(),
                global_db_path,
                project_db_path,
                app_home,
            )
            .await?;
            Ok(true)
        }
    }
}

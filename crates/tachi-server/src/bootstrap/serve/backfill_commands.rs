use std::error::Error;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::Commands;

pub(super) async fn run_if_backfill_command(
    command: &Commands,
    home: &Path,
    global_db_path: &PathBuf,
) -> Result<bool, Box<dyn Error>> {
    match command {
        Commands::BackfillVectors {
            db,
            project,
            batch_size,
            dry_run,
            include_cache,
        } => {
            let target_path = if let Some(p) = db {
                expand_user_path(home, p.to_string_lossy().as_ref())
            } else if let Some(project) = project {
                let path = crate::path_utils::plan_c_global_db_path(project);
                if !path.exists() {
                    return Err(format!(
                        "named project DB not found for '{project}': {}",
                        path.display()
                    )
                    .into());
                }
                path
            } else {
                global_db_path.clone()
            };
            super::super::backfill::run_backfill_vectors(
                &target_path,
                global_db_path,
                *batch_size,
                *dry_run,
                *include_cache,
            )
            .await?;
            Ok(true)
        }
        Commands::BackfillSummaries { db, dry_run } => {
            let target_path = db
                .as_ref()
                .map(|p| expand_user_path(home, p.to_string_lossy().as_ref()))
                .unwrap_or_else(|| global_db_path.clone());
            super::super::backfill::run_backfill_summaries(&target_path, global_db_path, *dry_run)
                .await?;
            Ok(true)
        }
        Commands::BackfillMetadata { db, dry_run } => {
            let target_path = db
                .as_ref()
                .map(|p| expand_user_path(home, p.to_string_lossy().as_ref()))
                .unwrap_or_else(|| global_db_path.clone());
            super::super::backfill::run_backfill_metadata(&target_path, global_db_path, *dry_run)
                .await?;
            Ok(true)
        }
        Commands::BackfillFts { db, full, dry_run } => {
            let target_path = db
                .as_ref()
                .map(|p| expand_user_path(home, p.to_string_lossy().as_ref()))
                .unwrap_or_else(|| global_db_path.clone());
            super::super::backfill::run_backfill_fts(&target_path, *full, *dry_run).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn expand_user_path(home: &Path, raw: &str) -> PathBuf {
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

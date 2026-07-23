use std::collections::BTreeSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::Commands;

pub(super) async fn run_if_backfill_command(
    command: &Commands,
    home: &Path,
    global_db_path: &PathBuf,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<bool, Box<dyn Error>> {
    match command {
        Commands::BackfillVectors {
            db,
            project,
            all_projects,
            batch_size,
            dry_run,
            include_cache,
        } => {
            let target_paths = if *all_projects {
                all_project_backfill_targets(home)?
            } else {
                vec![if let Some(p) = db {
                    expand_user_path(home, p.to_string_lossy().as_ref())
                } else if let Some(project) = project {
                    let path = crate::path_utils::plan_c_global_db_path_existing(project);
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
                }]
            };
            for target_path in target_paths {
                super::super::backfill::run_backfill_vectors(
                    &target_path,
                    global_db_path,
                    *batch_size,
                    *dry_run,
                    *include_cache,
                    schema_migration,
                )
                .await?;
            }
            Ok(true)
        }
        Commands::BackfillSummaries { db, dry_run } => {
            let target_path = db
                .as_ref()
                .map(|p| expand_user_path(home, p.to_string_lossy().as_ref()))
                .unwrap_or_else(|| global_db_path.clone());
            super::super::backfill::run_backfill_summaries(
                &target_path,
                global_db_path,
                *dry_run,
                schema_migration,
            )
            .await?;
            Ok(true)
        }
        Commands::BackfillMetadata { db, dry_run } => {
            let target_path = db
                .as_ref()
                .map(|p| expand_user_path(home, p.to_string_lossy().as_ref()))
                .unwrap_or_else(|| global_db_path.clone());
            super::super::backfill::run_backfill_metadata(
                &target_path,
                global_db_path,
                *dry_run,
                schema_migration,
            )
            .await?;
            Ok(true)
        }
        Commands::BackfillFts { db, full, dry_run } => {
            let target_path = db
                .as_ref()
                .map(|p| expand_user_path(home, p.to_string_lossy().as_ref()))
                .unwrap_or_else(|| global_db_path.clone());
            super::super::backfill::run_backfill_fts(
                &target_path,
                *full,
                *dry_run,
                schema_migration,
            )
            .await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Resolve the multi-project maintenance scope from the manifest only.
///
/// The manual backfill command must not discover arbitrary sqlite files or
/// bypass ownership policy: this path deliberately accepts only manifest rows
/// Tachi owns, that are admitted for writes, and that declare the current
/// Tachi schema. The dry-run caller still opens every returned path read-only.
fn all_project_backfill_targets(home: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let manifest_path = crate::manifest::Manifest::default_path(home);
    let manifest = crate::manifest::Manifest::load(&manifest_path).map_err(|error| {
        format!(
            "load manifest for --all-projects ({}): {error}",
            manifest_path.display()
        )
    })?;
    let targets: BTreeSet<PathBuf> = manifest
        .dbs
        .into_iter()
        .filter(|entry| entry.owner == "tachi" && entry.allow_write && entry.schema_kind == "tachi")
        .map(|entry| PathBuf::from(entry.path))
        .collect();
    if targets.is_empty() {
        return Err("--all-projects found no manifest-owned writable Tachi databases".into());
    }
    Ok(targets.into_iter().collect())
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

#[cfg(test)]
mod tests {
    use super::{all_project_backfill_targets, run_if_backfill_command};
    use crate::manifest::{DbEntry, DbRole, Manifest, MANIFEST_SCHEMA_VERSION};
    use memcore::{MemoryStore, MigrationAuthority};
    use std::path::Path;
    use tachi_bootstrap::cli::Commands;

    fn manifest_entry(path: &Path, owner: &str, allow_write: bool, schema_kind: &str) -> DbEntry {
        DbEntry {
            path: path.to_string_lossy().into_owned(),
            role: DbRole::Project,
            owner: owner.to_string(),
            schema_kind: schema_kind.to_string(),
            vec_enabled: true,
            allow_write,
            last_doctor_at: "2026-01-01T00:00:00Z".to_string(),
            last_classification: "healthy".to_string(),
            scope_hint: "project:test".to_string(),
            notes: String::new(),
        }
    }

    fn create_memory_db(path: &Path) {
        MemoryStore::open(path.to_str().expect("utf8 path")).expect("create memory DB");
    }

    #[tokio::test]
    async fn all_projects_dry_run_uses_only_manifest_targets_without_mutation() {
        let dir = tempfile::tempdir().expect("tmp");
        let home = dir.path().join("home");
        let first = dir.path().join("first.db");
        let second = dir.path().join("second.db");
        create_memory_db(&first);
        create_memory_db(&second);
        let manifest = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            comment: String::new(),
            dbs: vec![
                manifest_entry(&second, "tachi", true, "tachi"),
                manifest_entry(&first, "tachi", true, "tachi"),
                manifest_entry(&dir.path().join("external.db"), "external", true, "tachi"),
                manifest_entry(&dir.path().join("readonly.db"), "tachi", false, "tachi"),
                manifest_entry(&dir.path().join("legacy.db"), "tachi", true, "legacy"),
            ],
        };
        manifest
            .save(&Manifest::default_path(&home))
            .expect("write manifest");

        let targets = all_project_backfill_targets(&home).expect("resolve targets");
        assert_eq!(targets, vec![first.clone(), second.clone()]);

        let before = [
            (
                first.clone(),
                std::fs::read(&first).expect("read first before"),
            ),
            (
                second.clone(),
                std::fs::read(&second).expect("read second before"),
            ),
        ];
        let command = Commands::BackfillVectors {
            db: None,
            project: None,
            all_projects: true,
            batch_size: 1,
            dry_run: true,
            include_cache: false,
        };
        assert!(
            run_if_backfill_command(&command, &home, &first, &MigrationAuthority::Deny,)
                .await
                .expect("all-project dry-run")
        );

        for (path, bytes) in before {
            assert_eq!(
                std::fs::read(&path).expect("read DB after dry-run"),
                bytes,
                "dry-run changed {}",
                path.display()
            );
            let store = MemoryStore::open_read_only(path.to_str().expect("utf8 path"))
                .expect("reopen read-only");
            let state_table_count: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='vector_sweep_state'",
                    [],
                    |row| row.get(0),
                )
                .expect("state table check");
            assert_eq!(state_table_count, 0, "dry-run created sweep state");
        }
    }

    #[test]
    fn all_projects_refuses_an_empty_manifest_scope() {
        let dir = tempfile::tempdir().expect("tmp");
        let home = dir.path().join("home");
        Manifest::empty()
            .save(&Manifest::default_path(&home))
            .expect("write manifest");
        let error = all_project_backfill_targets(&home).expect_err("empty scope must refuse");
        assert!(error
            .to_string()
            .contains("no manifest-owned writable Tachi"));
    }
}

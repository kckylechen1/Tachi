use std::path::{Path, PathBuf};

pub(crate) mod apply;
pub(crate) mod classify;
pub(crate) mod fs;
pub(crate) mod legacy;
pub(crate) mod plan;
pub(crate) mod repair;
pub(crate) mod types;

#[cfg(test)]
mod tests;

pub(crate) use apply::apply_plan_internal;
pub(crate) use legacy::run_wiki_corpus_legacy_adoption_command;
pub(crate) use repair::run_wiki_corpus_sibling_repair_command;
pub(crate) use types::*;

use self::apply::*;
use self::classify::*;
use self::fs::*;
use self::plan::*;

pub(crate) fn run_wiki_corpus_command(
    apply: bool,
    confirm: Option<String>,
    backup_dir: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
) -> Result<WikiCorpusReport, String> {
    run_wiki_corpus_command_internal(
        apply, confirm, backup_dir, plan_path, global_db, project_db, app_home, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_wiki_corpus_command_internal(
    apply: bool,
    confirm: Option<String>,
    backup_dir: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
    mut race_hook: Option<CorpusRaceHook>,
) -> Result<WikiCorpusReport, String> {
    if !apply {
        if confirm.is_some() || backup_dir.is_some() || plan_path.is_some() {
            return Err("--confirm, --backup-dir, and --plan require explicit --apply".to_string());
        }
    } else {
        if confirm.as_deref() != Some(WIKI_CORPUS_CONFIRMATION_TOKEN) {
            return Err(format!(
                "apply requires exact --confirm {}",
                WIKI_CORPUS_CONFIRMATION_TOKEN
            ));
        }
        let backup_dir = backup_dir
            .as_deref()
            .ok_or_else(|| "apply requires explicit --backup-dir".to_string())?;
        if regular_directory_metadata(backup_dir).is_err() {
            return Err(format!(
                "apply requires an existing backup directory: {}",
                backup_dir.display()
            ));
        }
    }

    let mut scans = store_specs(global_db, project_db, app_home)
        .into_iter()
        .map(inventory_store)
        .collect::<Vec<_>>();
    finalize_classifications(&mut scans);
    for scan in scans.iter_mut() {
        refresh_report(scan);
    }
    if apply {
        maybe_swap_logical_path_after_inventory(&scans, &mut race_hook)?;
    }
    let warnings = scans
        .iter()
        .filter_map(|scan| {
            scan.report
                .read_failure
                .as_ref()
                .map(|failure| format!("{}: {}", scan.report.logical_store_ref, failure.message))
        })
        .collect::<Vec<_>>();
    let preview_plan = build_plan(&scans).ok();
    if !apply {
        return Ok(WikiCorpusReport {
            version: REPORT_VERSION.to_string(),
            mode: "preview".to_string(),
            apply: false,
            stores: scans.into_iter().map(|scan| scan.report).collect(),
            plan: preview_plan,
            backup_manifest: None,
            migration_outcomes: Vec::new(),
            warnings,
            sibling_repair: None,
            legacy_adoption: None,
        });
    }

    let plan = match plan_path {
        Some(path) => load_plan(&path)?,
        None => preview_plan
            .ok_or_else(|| "cannot build an apply plan from the inventory".to_string())?,
    };
    let backup_dir = backup_dir.expect("validated above");
    let (backup_manifest, migration_outcomes) =
        apply_plan_internal(&scans, &plan, &backup_dir, None, race_hook)?;
    Ok(WikiCorpusReport {
        version: REPORT_VERSION.to_string(),
        mode: "apply".to_string(),
        apply: true,
        stores: scans.into_iter().map(|scan| scan.report).collect(),
        plan: Some(plan),
        backup_manifest: Some(backup_manifest),
        migration_outcomes,
        warnings,
        sibling_repair: None,
        legacy_adoption: None,
    })
}

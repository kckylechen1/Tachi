use std::path::PathBuf;

use super::super::{
    collect_memory_db_files, open_cli_store, TidyFinding, TidyGroupSummary, TidyPlanStep,
    TidyReport,
};
use super::classify::{
    classify_tidy_scope, tidy_group_key, tidy_group_priority, tidy_rationale,
    tidy_recommended_action, tidy_target_label,
};

pub(crate) fn build_tidy_report(
    roots: &[PathBuf],
    git_root: Option<&PathBuf>,
) -> Result<TidyReport, Box<dyn std::error::Error>> {
    let mut discovered = Vec::new();
    for root in roots {
        collect_memory_db_files(root, &mut discovered, 10);
    }

    discovered.sort();
    discovered.dedup();

    let mut databases = Vec::new();
    let mut total_memories = 0usize;

    for path in discovered {
        let scope_suggestion = classify_tidy_scope(&path, git_root);
        let mut status = "ok".to_string();
        let mut entry_count = None;
        let mut vec_available = false;
        let link_meta = std::fs::symlink_metadata(&path).ok();
        let is_symlink = link_meta
            .as_ref()
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false);
        let symlink_target = if is_symlink {
            std::fs::read_link(&path)
                .ok()
                .map(|target| target.display().to_string())
        } else {
            None
        };
        let target_exists = is_symlink.then(|| path.exists());

        if is_symlink && !path.exists() {
            status = "broken_symlink".to_string();
        } else {
            match open_cli_store(&path) {
                Ok(store) => {
                    vec_available = store.vec_available;
                    match store.stats(false) {
                        Ok(stats) => {
                            entry_count = Some(stats.total as usize);
                            total_memories += stats.total as usize;
                        }
                        Err(_) => {
                            status = "stats_error".to_string();
                        }
                    }
                }
                Err(_) => {
                    status = "open_error".to_string();
                }
            }
        }

        databases.push(TidyFinding {
            path: path.display().to_string(),
            entry_count,
            vec_available,
            recommended_action: tidy_recommended_action(&scope_suggestion, &status),
            scope_suggestion,
            status,
            is_symlink,
            symlink_target,
            target_exists,
        });
    }

    databases.sort_by(|a, b| {
        tidy_group_priority(&tidy_group_key(&a.scope_suggestion))
            .cmp(&tidy_group_priority(&tidy_group_key(&b.scope_suggestion)))
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut groups_map = std::collections::BTreeMap::<String, (usize, usize)>::new();
    for db in &databases {
        let key = tidy_group_key(&db.scope_suggestion);
        let entry = groups_map.entry(key).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += db.entry_count.unwrap_or(0);
    }
    let groups = groups_map
        .into_iter()
        .map(|(group, (database_count, memory_count))| TidyGroupSummary {
            group,
            database_count,
            memory_count,
        })
        .collect::<Vec<_>>();
    let mut groups = groups;
    groups.sort_by(|a, b| {
        tidy_group_priority(&a.group)
            .cmp(&tidy_group_priority(&b.group))
            .then_with(|| a.group.cmp(&b.group))
    });

    let mut plan_map = std::collections::BTreeMap::<(usize, String, String), Vec<String>>::new();
    for db in &databases {
        let key = (
            tidy_group_priority(&tidy_group_key(&db.scope_suggestion)),
            db.scope_suggestion.clone(),
            db.recommended_action.clone(),
        );
        plan_map.entry(key).or_default().push(db.path.clone());
    }
    let dry_run_plan = plan_map
        .into_iter()
        .enumerate()
        .map(|(index, ((_, scope, action), source_paths))| TidyPlanStep {
            order: index + 1,
            target_label: tidy_target_label(&scope, &action),
            rationale: tidy_rationale(&scope, &action),
            scope,
            action,
            source_paths,
        })
        .collect::<Vec<_>>();

    let mut next_steps = Vec::new();
    if databases.len() > 1 {
        next_steps.push(
            "Review scope_suggestion for each DB before adding any migration step".to_string(),
        );
    }
    if databases.iter().any(|db| db.status != "ok") {
        next_steps.push(
            "Repair or inspect DBs with open_error/stats_error before consolidation".to_string(),
        );
    }
    if databases.iter().any(|db| db.status == "broken_symlink") {
        next_steps.push(
            "Run `tachi tidy --apply` to remove broken memory.db symlinks, then rescan."
                .to_string(),
        );
    }
    if databases.is_empty() {
        next_steps.push(
            "No memory.db files found in scanned roots; add more roots or initialize a project DB"
                .to_string(),
        );
    }

    Ok(TidyReport {
        scanned_roots: roots
            .iter()
            .map(|root| root.display().to_string())
            .collect(),
        groups,
        dry_run_plan,
        total_databases: databases.len(),
        total_memories,
        databases,
        next_steps,
    })
}

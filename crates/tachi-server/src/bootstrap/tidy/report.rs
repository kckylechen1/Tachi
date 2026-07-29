use std::path::PathBuf;

use super::super::{
    collect_memory_db_files, open_cli_store_read_only, TidyFinding, TidyGroupSummary, TidyPlanStep,
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

    let inventory = crate::physical_db_identity::classify_paths(discovered);
    let resolved_aliases = inventory
        .stores
        .iter()
        .map(|store| store.aliases.len())
        .sum::<usize>();
    let unresolved_path_count = inventory.unresolved_paths.len();
    let mut physical_stores = inventory.stores;
    let mut databases = Vec::new();
    let mut total_memories = 0usize;

    for physical_store in &mut physical_stores {
        let path = PathBuf::from(&physical_store.open_path);
        let mut status = "ok".to_string();
        let mut entry_count = None;
        let mut vec_available = false;
        let mut open_failure_kind = None;

        match open_cli_store_read_only(&path) {
            Ok(store) => {
                vec_available = store.vec_available;
                match store.stats(false) {
                    Ok(stats) => {
                        entry_count = Some(stats.total as usize);
                        total_memories += stats.total as usize;
                    }
                    Err(error) => {
                        status = "stats_error".to_string();
                        open_failure_kind =
                            Some(crate::physical_db_identity::classify_memory_failure(&error));
                    }
                }
            }
            Err(error) => {
                status = "open_error".to_string();
                open_failure_kind = Some(
                    error
                        .downcast_ref::<memcore::MemoryError>()
                        .map(crate::physical_db_identity::classify_memory_failure)
                        .unwrap_or_else(|| {
                            crate::physical_db_identity::classify_open_failure(error.as_ref())
                        }),
                );
            }
        }
        physical_store.open_failure_kind = open_failure_kind;

        for alias in &physical_store.aliases {
            let alias_path = PathBuf::from(alias);
            let scope_suggestion = classify_tidy_scope(&alias_path, git_root);
            let is_symlink = std::fs::symlink_metadata(&alias_path)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false);
            let symlink_target = is_symlink
                .then(|| std::fs::read_link(&alias_path).ok())
                .flatten()
                .map(|target| target.display().to_string());
            databases.push(TidyFinding {
                path: alias.clone(),
                entry_count,
                vec_available,
                recommended_action: tidy_recommended_action(&scope_suggestion, &status),
                scope_suggestion,
                status: status.clone(),
                is_symlink,
                symlink_target,
                target_exists: is_symlink.then_some(true),
                physical_id: Some(physical_store.physical_id.clone()),
                canonical_path: Some(physical_store.canonical_path.clone()),
                inventory_open_path: Some(physical_store.open_path.clone()),
                is_primary_alias: alias == &physical_store.primary_path,
                open_failure_kind,
            });
        }
    }

    for unresolved in inventory.unresolved_paths {
        let scope_suggestion = classify_tidy_scope(&unresolved.path, git_root);
        let status = if unresolved.failure_kind
            == crate::physical_db_identity::InventoryFailureKind::BrokenSymlink
        {
            "broken_symlink".to_string()
        } else {
            "open_error".to_string()
        };
        databases.push(TidyFinding {
            path: unresolved.path.display().to_string(),
            entry_count: None,
            vec_available: false,
            recommended_action: tidy_recommended_action(&scope_suggestion, &status),
            scope_suggestion,
            status,
            is_symlink: unresolved.is_symlink,
            symlink_target: unresolved.symlink_target,
            target_exists: unresolved.target_exists,
            physical_id: None,
            canonical_path: None,
            inventory_open_path: None,
            is_primary_alias: false,
            open_failure_kind: Some(unresolved.failure_kind),
        });
    }

    databases.sort_by(|a, b| {
        tidy_group_priority(&tidy_group_key(&a.scope_suggestion))
            .cmp(&tidy_group_priority(&tidy_group_key(&b.scope_suggestion)))
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut groups_map = std::collections::BTreeMap::<String, (usize, usize)>::new();
    for db in databases.iter().filter(|db| db.is_primary_alias) {
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
    for db in databases
        .iter()
        .filter(|db| db.is_primary_alias || db.physical_id.is_none())
    {
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
    if physical_stores.len() > 1 {
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
        total_databases: physical_stores.len(),
        total_aliases: resolved_aliases,
        resolved_aliases,
        unresolved_paths: unresolved_path_count,
        path_appearances: resolved_aliases + unresolved_path_count,
        total_memories,
        databases,
        physical_stores,
        next_steps,
    })
}

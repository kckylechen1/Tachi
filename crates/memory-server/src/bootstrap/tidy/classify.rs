use std::path::PathBuf;

pub(super) fn classify_tidy_scope(path: &std::path::Path, git_root: Option<&PathBuf>) -> String {
    if let Some(root) = git_root {
        if path.starts_with(root) {
            return "project".to_string();
        }
    }

    let normalized = path.to_string_lossy().replace('\\', "/");
    let extract_agent = |marker: &str| -> Option<String> {
        let (_, rest) = normalized.split_once(marker)?;
        Some(rest.split('/').next().unwrap_or("unknown").to_string())
    };
    let extract_backup_agent = || -> Option<String> {
        let (_, rest) = normalized.split_once("/.openclaw/backups/")?;
        let (_, rest) = rest.split_once("/data/agents/")?;
        Some(rest.split('/').next().unwrap_or("unknown").to_string())
    };

    if normalized.contains("/.tachi/global/")
        || normalized.ends_with("/.tachi/global/memory.db")
        || normalized.contains("/.sigil/global/")
        || normalized.ends_with("/.sigil/global/memory.db")
    {
        "global".to_string()
    } else if let Some(agent) = extract_agent("/.openclaw/extensions/tachi/data/agents/") {
        format!("openclaw-plugin-agent:{agent}")
    } else if let Some(agent) = extract_agent("/.openclaw/core/extensions/tachi/data/agents/") {
        format!("openclaw-core-agent:{agent}")
    } else if let Some(agent) =
        extract_agent("/.openclaw/core/extensions/memory-hybrid-bridge/data/agents/")
    {
        format!("openclaw-legacy-agent:{agent}")
    } else if let Some(agent) = extract_backup_agent() {
        format!("openclaw-backup-agent:{agent}")
    } else if normalized.contains("/.openclaw/backups/") {
        "openclaw-backup".to_string()
    } else if let Some(agent) = extract_agent("/.openclaw/agents/") {
        format!("openclaw-agent-local:{agent}")
    } else if normalized.contains("/.openclaw/") {
        "openclaw-review".to_string()
    } else if let Some((_, rest)) = normalized.split_once("/.tachi/projects/") {
        let project_name = rest.split('/').next().unwrap_or("unknown");
        format!("project:{project_name}")
    } else if normalized.contains("/.gemini/") {
        "global".to_string()
    } else if normalized.contains("/.sigil/") || normalized.contains("/.tachi/") {
        "review".to_string()
    } else {
        "archive".to_string()
    }
}

pub(super) fn tidy_group_key(scope_suggestion: &str) -> String {
    scope_suggestion
        .split(':')
        .next()
        .unwrap_or(scope_suggestion)
        .to_string()
}

pub(super) fn tidy_group_priority(group: &str) -> usize {
    match group {
        "openclaw-plugin-agent" => 0,
        "project" => 1,
        "global" => 2,
        "openclaw-agent-local" => 3,
        "openclaw-core-agent" => 4,
        "openclaw-legacy-agent" => 5,
        "openclaw-backup-agent" => 6,
        "openclaw-backup" => 7,
        "openclaw-review" => 8,
        "review" => 9,
        "archive" => 10,
        _ => 99,
    }
}

pub(super) fn tidy_recommended_action(scope_suggestion: &str, status: &str) -> String {
    if status == "broken_symlink" {
        return "remove_broken_symlink".to_string();
    }

    if status != "ok" {
        return "repair_before_any_move".to_string();
    }

    match tidy_group_key(scope_suggestion).as_str() {
        "openclaw-plugin-agent" | "openclaw-agent-local" => "keep_separate_agent_db".to_string(),
        "project" => "keep_project_db".to_string(),
        "global" => "keep_global_db".to_string(),
        "openclaw-core-agent" | "openclaw-legacy-agent" => {
            "review_for_legacy_migration".to_string()
        }
        "openclaw-backup-agent" | "openclaw-backup" => "archive_or_delete_after_review".to_string(),
        "openclaw-review" | "review" | "archive" => "manual_review".to_string(),
        _ => "manual_review".to_string(),
    }
}

pub(super) fn tidy_target_label(scope_suggestion: &str, action: &str) -> String {
    match action {
        "keep_separate_agent_db" | "keep_project_db" | "keep_global_db" => {
            scope_suggestion.to_string()
        }
        "review_for_legacy_migration" => format!("review->{scope_suggestion}"),
        "archive_or_delete_after_review" => "archive".to_string(),
        "repair_before_any_move" => "repair".to_string(),
        "remove_broken_symlink" => "cleanup".to_string(),
        _ => "manual-review".to_string(),
    }
}

pub(super) fn tidy_rationale(scope_suggestion: &str, action: &str) -> String {
    match action {
        "keep_separate_agent_db" => format!(
            "{scope_suggestion} looks like an active agent-local OpenClaw database and should stay separate."
        ),
        "keep_project_db" => "This database already matches the current project-scoped layout.".to_string(),
        "keep_global_db" => "This database already matches the global/shared layout.".to_string(),
        "review_for_legacy_migration" => format!(
            "{scope_suggestion} appears to be legacy OpenClaw state and needs manual migration review."
        ),
        "archive_or_delete_after_review" => format!(
            "{scope_suggestion} appears to be backup state that should not be merged blindly."
        ),
        "repair_before_any_move" => "The DB could not be opened/read cleanly; repair it before planning migration.".to_string(),
        "remove_broken_symlink" => {
            "The memory.db path is a broken symlink; remove the stale link and rescan.".to_string()
        }
        _ => format!("{scope_suggestion} needs manual review before deciding a destination."),
    }
}

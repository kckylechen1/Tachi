use super::*;
use serde::Serialize;

const SETUP_API_KEYS: [(&str, &str); 5] = [
    ("VOYAGE_API_KEY", "Voyage embeddings (voyage-4)"),
    (
        "VOYAGE_RERANK_API_KEY",
        "Voyage reranking (rerank-2.5) — optional",
    ),
    ("SILICONFLOW_API_KEY", "SiliconFlow extraction"),
    ("MINIMAX_API_KEY", "MiniMax distill/summary"),
    ("REASONING_API_KEY", "GLM-5.1 reasoning lane"),
];

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SetupItem {
    pub id: String,
    pub label: String,
    pub status: String,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SetupReport {
    pub app_home: String,
    pub config_env_path: String,
    pub global_db_path: String,
    pub project_db_path: Option<String>,
    pub git_root: Option<String>,
    pub items: Vec<SetupItem>,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyFinding {
    pub path: String,
    pub entry_count: Option<usize>,
    pub vec_available: bool,
    pub scope_suggestion: String,
    pub status: String,
    pub recommended_action: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyGroupSummary {
    pub group: String,
    pub database_count: usize,
    pub memory_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyPlanStep {
    pub order: usize,
    pub scope: String,
    pub action: String,
    pub source_paths: Vec<String>,
    pub target_label: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyAppliedStep {
    pub order: usize,
    pub scope: String,
    pub action: String,
    pub outcome: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyApplySummary {
    pub report_path: String,
    pub applied_steps: Vec<TidyAppliedStep>,
    pub applied_count: usize,
    pub skipped_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyReport {
    pub scanned_roots: Vec<String>,
    pub databases: Vec<TidyFinding>,
    pub groups: Vec<TidyGroupSummary>,
    pub dry_run_plan: Vec<TidyPlanStep>,
    pub total_databases: usize,
    pub total_memories: usize,
    pub next_steps: Vec<String>,
}

fn open_cli_store(db_path: &PathBuf) -> Result<MemoryStore, Box<dyn std::error::Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;
    Ok(MemoryStore::open(db_str)?)
}

fn open_cli_store_read_only(db_path: &PathBuf) -> Result<MemoryStore, Box<dyn std::error::Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;
    Ok(MemoryStore::open_read_only(db_str)?)
}

fn print_pretty_json(value: &serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

// `build_cli_memory_entry` was removed alongside the legacy `Commands::Save`
// arm. `tachi save` now aliases to `tachi remember`, which routes through
// `crate::memory_search_ops::handle_remember` and constructs its `MemoryEntry`
// from the richer `RememberParams` (tags, scope, project, category, topic,
// domain, retention-policy, summary, force).

fn evaluate_cli_capability_enabled(
    cap_type: &str,
    definition: &str,
) -> Result<(bool, Option<String>), Box<dyn std::error::Error>> {
    if cap_type != "mcp" {
        return Ok((true, None));
    }

    let def: serde_json::Value = serde_json::from_str(definition)?;
    let transport_type = def["transport"].as_str().unwrap_or("stdio");
    if transport_type != "stdio" {
        return Ok((true, None));
    }

    match def["command"].as_str() {
        Some(cmd) if is_trusted_command(cmd) => Ok((true, None)),
        Some(cmd) => Ok((
            false,
            Some(format!(
                "Command '{}' is not in the trusted allowlist. Capability registered but disabled.",
                cmd
            )),
        )),
        None => Ok((
            false,
            Some(
                "mcp definition missing 'command' for stdio transport. Capability registered but disabled."
                    .to_string(),
            ),
        )),
    }
}

fn count_matching_entries(
    root: &std::path::Path,
    matcher: &dyn Fn(&std::path::Path) -> bool,
) -> usize {
    std::fs::read_dir(root)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter(|entry| matcher(&entry.path()))
        .count()
}

fn collect_memory_db_files(root: &std::path::Path, out: &mut Vec<PathBuf>, max_depth: usize) {
    if !root.exists() {
        return;
    }

    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name == "memory.db")
                .unwrap_or(false)
            {
                out.push(path);
            }
            continue;
        }

        if path.is_dir() && max_depth > 0 {
            collect_memory_db_files(&path, out, max_depth.saturating_sub(1));
        }
    }
}

fn classify_tidy_scope(path: &std::path::Path, git_root: Option<&PathBuf>) -> String {
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

fn tidy_group_key(scope_suggestion: &str) -> String {
    scope_suggestion
        .split(':')
        .next()
        .unwrap_or(scope_suggestion)
        .to_string()
}

fn tidy_group_priority(group: &str) -> usize {
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

fn tidy_recommended_action(scope_suggestion: &str, status: &str) -> String {
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

fn tidy_target_label(scope_suggestion: &str, action: &str) -> String {
    match action {
        "keep_separate_agent_db" | "keep_project_db" | "keep_global_db" => {
            scope_suggestion.to_string()
        }
        "review_for_legacy_migration" => format!("review->{scope_suggestion}"),
        "archive_or_delete_after_review" => "archive".to_string(),
        "repair_before_any_move" => "repair".to_string(),
        _ => "manual-review".to_string(),
    }
}

fn tidy_rationale(scope_suggestion: &str, action: &str) -> String {
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
        _ => format!("{scope_suggestion} needs manual review before deciding a destination."),
    }
}

pub(crate) fn build_setup_report(
    home: &std::path::Path,
    app_home: &std::path::Path,
    global_db_path: &PathBuf,
    project_db_path: Option<&PathBuf>,
    git_root: Option<&PathBuf>,
    env_vars: &HashMap<String, String>,
) -> Result<SetupReport, Box<dyn std::error::Error>> {
    let config_env_path = app_home.join("config.env");

    let api_key_details = SETUP_API_KEYS
        .iter()
        .map(|(key, label)| {
            let status = if env_vars
                .get(*key)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                "configured"
            } else {
                "missing"
            };
            format!("{key}: {status} ({label})")
        })
        .collect::<Vec<_>>();
    let configured_api_keys = api_key_details
        .iter()
        .filter(|detail| detail.contains(": configured "))
        .count();

    let skill_roots = vec![
        ("tachi", app_home.join("skills"), "SKILL.md"),
        ("claude", home.join(".claude").join("skills"), "SKILL.md"),
        ("codex", home.join(".codex").join("skills"), "SKILL.md"),
        ("gemini", home.join(".gemini").join("skills"), "SKILL.md"),
        ("cursor", home.join(".cursor").join("rules"), ".mdc"),
        (
            "openclaw",
            home.join(".openclaw").join("plugins"),
            "tachi-projection.json",
        ),
        (
            "opencode",
            home.join(".opencode").join("skills"),
            "SKILL.md",
        ),
    ];
    let mut discovered_skill_entries = 0usize;
    let skill_details = skill_roots
        .into_iter()
        .map(|(label, root, marker)| {
            let count = if marker.starts_with('.') {
                count_matching_entries(&root, &|path| {
                    path.is_file()
                        && path
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .map(|ext| format!(".{ext}") == marker)
                            .unwrap_or(false)
                })
            } else {
                count_matching_entries(&root, &|path| {
                    (path.is_dir() && path.join(marker).exists())
                        || path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(|name| name == marker)
                            .unwrap_or(false)
                })
            };
            discovered_skill_entries += count;
            format!("{label}: {count} entries at {}", root.display())
        })
        .collect::<Vec<_>>();

    let agent_configs = [
        (
            "amp",
            home.join("Library/Application Support/Amp/settings.json"),
        ),
        ("claude", home.join(".claude").join("mcp.json")),
        ("cursor", home.join(".cursor").join("mcp.json")),
        ("gemini", home.join(".gemini").join("mcp.json")),
        ("codex", home.join(".codex")),
        ("openclaw", home.join(".openclaw").join("openclaw.json")),
        ("opencode", home.join(".opencode")),
    ];
    let detected_agents = agent_configs
        .iter()
        .filter(|(_, path)| path.exists())
        .count();
    let agent_details = agent_configs
        .iter()
        .map(|(label, path)| {
            format!(
                "{label}: {} ({})",
                if path.exists() { "detected" } else { "missing" },
                path.display()
            )
        })
        .collect::<Vec<_>>();

    let pipeline_enabled = env_vars
        .get("ENABLE_PIPELINE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    let pipeline_details = vec![format!(
        "ENABLE_PIPELINE={} (config: {})",
        if pipeline_enabled { "true" } else { "false" },
        config_env_path.display()
    )];

    let (vault_status, mut vault_details) = if global_db_path.exists() {
        let store = open_cli_store(global_db_path)?;
        let initialized = store.vault_get_config()?.is_some();
        let entry_count = if initialized {
            Some(store.vault_count_entries()?)
        } else {
            None
        };
        (
            if initialized { "configured" } else { "missing" }.to_string(),
            vec![
                format!("global db: {}", global_db_path.display()),
                format!(
                    "vault: {}",
                    if initialized {
                        format!("initialized ({} secrets)", entry_count.unwrap_or(0))
                    } else {
                        "not initialized".to_string()
                    }
                ),
            ],
        )
    } else {
        (
            "missing".to_string(),
            vec![
                format!("global db: {} (not created yet)", global_db_path.display()),
                "vault: not initialized".to_string(),
            ],
        )
    };
    if let Some(project_db_path) = project_db_path {
        vault_details.push(format!("project db: {}", project_db_path.display()));
    }

    let items = vec![
        SetupItem {
            id: "api_keys".to_string(),
            label: "1/5 API Keys".to_string(),
            status: if configured_api_keys >= 2 {
                "ready".to_string()
            } else {
                "needs_attention".to_string()
            },
            details: api_key_details,
        },
        SetupItem {
            id: "skills".to_string(),
            label: "2/5 Skills".to_string(),
            status: if discovered_skill_entries > 0 {
                "ready".to_string()
            } else {
                "needs_attention".to_string()
            },
            details: skill_details,
        },
        SetupItem {
            id: "agents".to_string(),
            label: "3/5 Agents".to_string(),
            status: if detected_agents > 0 {
                "ready".to_string()
            } else {
                "needs_attention".to_string()
            },
            details: agent_details,
        },
        SetupItem {
            id: "pipeline".to_string(),
            label: "4/5 Pipeline".to_string(),
            status: if pipeline_enabled {
                "ready".to_string()
            } else {
                "needs_attention".to_string()
            },
            details: pipeline_details,
        },
        SetupItem {
            id: "vault".to_string(),
            label: "5/5 Vault".to_string(),
            status: vault_status,
            details: vault_details,
        },
    ];

    let mut next_steps = Vec::new();
    if configured_api_keys < 2 {
        next_steps.push(format!(
            "Add missing API keys to {}",
            config_env_path.display()
        ));
    }
    if discovered_skill_entries == 0 {
        next_steps.push(
            "Scan or project skills into ~/.tachi/skills or a supported agent directory"
                .to_string(),
        );
    }
    if detected_agents == 0 {
        next_steps.push(
            "Configure at least one MCP client (Claude, Cursor, Gemini, Codex, OpenClaw, or Amp)"
                .to_string(),
        );
    }
    if !pipeline_enabled {
        next_steps.push(format!(
            "Enable the extraction pipeline with ENABLE_PIPELINE=true in {}",
            config_env_path.display()
        ));
    }
    if items
        .iter()
        .find(|item| item.id == "vault")
        .map(|item| item.status != "configured")
        .unwrap_or(true)
    {
        next_steps.push(
            "Initialize the vault after the daemon is running with the vault_init tool".to_string(),
        );
    }

    Ok(SetupReport {
        app_home: app_home.display().to_string(),
        config_env_path: config_env_path.display().to_string(),
        global_db_path: global_db_path.display().to_string(),
        project_db_path: project_db_path.map(|path| path.display().to_string()),
        git_root: git_root.map(|path| path.display().to_string()),
        items,
        next_steps,
    })
}

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

        databases.push(TidyFinding {
            path: path.display().to_string(),
            entry_count,
            vec_available,
            recommended_action: tidy_recommended_action(&scope_suggestion, &status),
            scope_suggestion,
            status,
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

fn render_setup_report(report: &SetupReport) -> String {
    let mut lines = vec![
        "tachi setup".to_string(),
        format!("app home: {}", report.app_home),
        format!("config env: {}", report.config_env_path),
        format!("global db: {}", report.global_db_path),
    ];
    if let Some(project_db) = report.project_db_path.as_ref() {
        lines.push(format!("project db: {project_db}"));
    }
    if let Some(git_root) = report.git_root.as_ref() {
        lines.push(format!("git root: {git_root}"));
    }
    lines.push(String::new());

    for item in &report.items {
        let emoji = match item.status.as_str() {
            "ready" | "configured" => "✅",
            _ => "⚠️",
        };
        lines.push(format!("{emoji} {}", item.label));
        for detail in &item.details {
            lines.push(format!("  - {detail}"));
        }
        lines.push(String::new());
    }

    if !report.next_steps.is_empty() {
        lines.push("Next steps:".to_string());
        for step in &report.next_steps {
            lines.push(format!("  - {step}"));
        }
    }

    lines.join("\n")
}

fn render_tidy_report(report: &TidyReport) -> String {
    let mut lines = vec![
        "tachi tidy".to_string(),
        format!("scanned roots: {}", report.scanned_roots.join(", ")),
        format!(
            "found {} databases, {} memories",
            report.total_databases, report.total_memories
        ),
        String::new(),
    ];

    for db in &report.databases {
        let emoji = if db.status == "ok" { "✅" } else { "⚠️" };
        let count = db
            .entry_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "?".to_string());
        lines.push(format!(
            "{emoji} {} — entries: {count}, suggest: {}, action: {}, vectors: {}, status: {}",
            db.path, db.scope_suggestion, db.recommended_action, db.vec_available, db.status
        ));
    }

    if !report.groups.is_empty() {
        lines.push(String::new());
        lines.push("Groups:".to_string());
        for group in &report.groups {
            lines.push(format!(
                "  - {}: {} DBs, {} memories",
                group.group, group.database_count, group.memory_count
            ));
        }
    }

    if !report.dry_run_plan.is_empty() {
        lines.push(String::new());
        lines.push("Dry-run plan:".to_string());
        for step in &report.dry_run_plan {
            lines.push(format!(
                "  {}. [{}] {} -> {}",
                step.order, step.action, step.scope, step.target_label
            ));
            lines.push(format!("     rationale: {}", step.rationale));
            for source in &step.source_paths {
                lines.push(format!("     source: {source}"));
            }
        }
    }

    if !report.next_steps.is_empty() {
        lines.push(String::new());
        lines.push("Next steps:".to_string());
        for step in &report.next_steps {
            lines.push(format!("  - {step}"));
        }
    }

    lines.join("\n")
}

fn render_tidy_apply_summary(summary: &TidyApplySummary) -> String {
    let mut lines = vec![
        "Apply summary:".to_string(),
        format!("  report: {}", summary.report_path),
        format!(
            "  applied: {} | skipped: {}",
            summary.applied_count, summary.skipped_count
        ),
    ];

    for step in &summary.applied_steps {
        lines.push(format!(
            "  {}. [{}] {} -> {}",
            step.order, step.outcome, step.scope, step.action
        ));
        lines.push(format!("     {}", step.note));
    }

    lines.join("\n")
}

pub(crate) fn execute_tidy_apply(
    app_home: &std::path::Path,
    report: &TidyReport,
) -> Result<TidyApplySummary, Box<dyn std::error::Error>> {
    let tidy_dir = app_home.join("tidy");
    std::fs::create_dir_all(&tidy_dir)?;
    let report_path = tidy_dir.join("last-apply.json");

    let mut applied_steps = Vec::new();
    let mut applied_count = 0usize;
    let mut skipped_count = 0usize;

    for step in &report.dry_run_plan {
        let (outcome, note) = match step.action.as_str() {
            "keep_separate_agent_db" => (
                "confirmed".to_string(),
                "No file move required; keeping the agent-scoped DB in place.".to_string(),
            ),
            "keep_project_db" => (
                "confirmed".to_string(),
                "No file move required; project DB already matches the intended layout."
                    .to_string(),
            ),
            "keep_global_db" => (
                "confirmed".to_string(),
                "No file move required; global DB already matches the intended layout.".to_string(),
            ),
            "archive_or_delete_after_review" => (
                "skipped".to_string(),
                "Backup/legacy DBs still require explicit review before any delete/archive action."
                    .to_string(),
            ),
            "review_for_legacy_migration" => (
                "skipped".to_string(),
                "Legacy DBs require provenance-aware migration rules before apply.".to_string(),
            ),
            "repair_before_any_move" => (
                "skipped".to_string(),
                "DB must be repaired and re-scanned before apply.".to_string(),
            ),
            _ => (
                "skipped".to_string(),
                "This action remains manual-review only in the conservative apply path."
                    .to_string(),
            ),
        };

        if outcome == "confirmed" {
            applied_count += 1;
        } else {
            skipped_count += 1;
        }

        applied_steps.push(TidyAppliedStep {
            order: step.order,
            scope: step.scope.clone(),
            action: step.action.clone(),
            outcome,
            note,
        });
    }

    let summary = TidyApplySummary {
        report_path: report_path.display().to_string(),
        applied_steps,
        applied_count,
        skipped_count,
    };

    let payload = json!({
        "report": report,
        "apply_summary": &summary,
    });
    std::fs::write(&report_path, serde_json::to_string_pretty(&payload)?)?;

    Ok(summary)
}

async fn run_setup_command(
    json_output: bool,
    home: &std::path::Path,
    app_home: &std::path::Path,
    global_db_path: &PathBuf,
    project_db_path: Option<&PathBuf>,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let env_vars = std::env::vars().collect::<HashMap<_, _>>();
    let report = build_setup_report(
        home,
        app_home,
        global_db_path,
        project_db_path,
        git_root,
        &env_vars,
    )?;

    if json_output {
        return print_pretty_json(&serde_json::to_value(&report)?);
    }

    println!("{}", render_setup_report(&report));

    // Interactive wizard — only when stdout is a TTY and not JSON mode
    if !atty_stdout() {
        return Ok(());
    }

    let needs_attention: Vec<&SetupItem> = report
        .items
        .iter()
        .filter(|item| item.status != "ready" && item.status != "configured")
        .collect();

    if needs_attention.is_empty() {
        println!("\nAll checks passed. Nothing to configure.");
        return Ok(());
    }

    let proceed = dialoguer::Confirm::new()
        .with_prompt("Run interactive setup wizard?")
        .default(true)
        .interact()?;

    if !proceed {
        return Ok(());
    }

    let config_env_path = app_home.join("config.env");
    let mut new_entries: Vec<(String, String)> = Vec::new();

    // 1. API Keys
    let missing_keys: Vec<(&str, &str)> = SETUP_API_KEYS
        .iter()
        .filter(|(key, _)| {
            !env_vars
                .get(*key)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        })
        .copied()
        .collect();

    if !missing_keys.is_empty() {
        println!("\n--- API Keys ---");
        for (key, label) in &missing_keys {
            let value: String = dialoguer::Input::new()
                .with_prompt(format!("{key} ({label})"))
                .allow_empty(true)
                .interact_text()?;
            let value = value.trim().to_string();
            if !value.is_empty() {
                new_entries.push((key.to_string(), value));
            }
        }
    }

    // 2. Pipeline
    let pipeline_enabled = env_vars
        .get("ENABLE_PIPELINE")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);

    if !pipeline_enabled {
        println!("\n--- Pipeline ---");
        let enable = dialoguer::Confirm::new()
            .with_prompt("Enable the extraction pipeline? (ENABLE_PIPELINE=true)")
            .default(false)
            .interact()?;
        if enable {
            new_entries.push(("ENABLE_PIPELINE".to_string(), "true".to_string()));
        }
    }

    // 3. Daemon settings
    let daemon_port = env_vars.get("TACHI_DAEMON_PORT");
    if daemon_port.is_none() {
        println!("\n--- Daemon ---");
        let port: String = dialoguer::Input::new()
            .with_prompt("Daemon port (TACHI_DAEMON_PORT)")
            .default("6919".to_string())
            .interact_text()?;
        let port = port.trim().to_string();
        if !port.is_empty() && port != "6919" {
            new_entries.push(("TACHI_DAEMON_PORT".to_string(), port));
        }
    }

    // Write to config.env
    if new_entries.is_empty() {
        println!("\nNo new values to write.");
        return Ok(());
    }

    println!("\nWill append to {}:", config_env_path.display());
    for (key, value) in &new_entries {
        let masked = if key.contains("KEY") || key.contains("SECRET") || key.contains("TOKEN") {
            let v = value.as_str();
            if v.len() > 8 {
                format!("{}...{}", &v[..4], &v[v.len() - 4..])
            } else {
                "****".to_string()
            }
        } else {
            value.clone()
        };
        println!("  {key}={masked}");
    }

    let confirm = dialoguer::Confirm::new()
        .with_prompt("Write these values?")
        .default(true)
        .interact()?;

    if !confirm {
        println!("Aborted.");
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = config_env_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Read existing content and build updated lines
    let existing = std::fs::read_to_string(&config_env_path).unwrap_or_default();
    let mut existing_lines: Vec<String> = existing.lines().map(String::from).collect();

    for (key, value) in &new_entries {
        let prefix = format!("{key}=");
        let commented_prefix = format!("# {key}=");
        let mut replaced = false;
        for line in existing_lines.iter_mut() {
            let trimmed = line.trim();
            if trimmed.starts_with(&prefix) || trimmed.starts_with(&commented_prefix) {
                *line = format!("{key}={value}");
                replaced = true;
                break;
            }
        }
        if !replaced {
            existing_lines.push(format!("{key}={value}"));
        }
    }

    // Ensure trailing newline
    let mut output = existing_lines.join("\n");
    if !output.ends_with('\n') {
        output.push('\n');
    }

    std::fs::write(&config_env_path, output)?;
    println!(
        "\nWrote {} entries to {}",
        new_entries.len(),
        config_env_path.display()
    );
    println!("Restart the daemon for changes to take effect.");
    Ok(())
}

fn atty_stdout() -> bool {
    unsafe { libc::isatty(libc::STDOUT_FILENO) != 0 }
}

async fn run_tidy_command(
    json_output: bool,
    apply: bool,
    app_home: &std::path::Path,
    roots: Vec<PathBuf>,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = build_tidy_report(&roots, git_root)?;
    let apply_summary = if apply {
        Some(execute_tidy_apply(app_home, &report)?)
    } else {
        None
    };

    if json_output {
        if let Some(summary) = apply_summary.as_ref() {
            print_pretty_json(&json!({
                "report": report,
                "apply_summary": summary,
            }))
        } else {
            print_pretty_json(&serde_json::to_value(&report)?)
        }
    } else {
        println!("{}", render_tidy_report(&report));
        if let Some(summary) = apply_summary.as_ref() {
            println!();
            println!("{}", render_tidy_apply_summary(summary));
        }
        Ok(())
    }
}

async fn run_doctor_command(
    json_output: bool,
    scan_only: bool,
    roots_override: Vec<PathBuf>,
    jobs_report: bool,
    home: &std::path::Path,
    app_home: &std::path::Path,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let roots: Vec<PathBuf> = if !roots_override.is_empty() {
        roots_override
    } else {
        crate::doctor::default_scan_roots(home, git_root.map(|p| p.as_path()))
    };
    let quarantine_dir = app_home.join("quarantine");
    let opts = crate::doctor::ScanOptions {
        auto_fix: !scan_only,
        max_depth: 10,
    };
    let report = crate::doctor::scan(&roots, &quarantine_dir, opts);

    // Always update the manifest after a doctor run (idempotent; preserves notes).
    let manifest_path = crate::manifest::Manifest::default_path(home);
    let mut m = crate::manifest::Manifest::load_or_empty(&manifest_path);
    m.populate_from_doctor(&report);
    if let Err(e) = m.save(&manifest_path) {
        eprintln!(
            "[doctor] warning: failed to update manifest at {}: {e}",
            manifest_path.display()
        );
    }

    // Branch #5: optional foundry job-status histogram per manifest DB.
    let jobs_section = if jobs_report {
        Some(collect_job_histograms(&m))
    } else {
        None
    };

    if json_output {
        let mut full = serde_json::to_value(&report)?;
        if let Some(jobs) = &jobs_section {
            if let Some(obj) = full.as_object_mut() {
                obj.insert("foundry_jobs".into(), serde_json::to_value(jobs)?);
            }
        }
        print_pretty_json(&full)
    } else {
        println!("{}", crate::doctor::render_report(&report));
        println!();
        println!(
            "manifest: {} ({} dbs recorded)",
            manifest_path.display(),
            m.dbs.len()
        );
        if let Some(jobs) = &jobs_section {
            println!("\n=== foundry jobs ===");
            for entry in jobs {
                let h = &entry.histogram;
                println!(
                    "  {} planned={} queued={} running={} completed={} failed={} skipped={} other={} gc_eligible(>=30d)={} total={}",
                    entry.path,
                    h.planned, h.queued, h.running, h.completed, h.failed, h.skipped,
                    h.other.iter().map(|(_, n)| n).sum::<usize>(),
                    h.gc_eligible, h.total
                );
                if !entry.error.is_empty() {
                    println!("    error: {}", entry.error);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
struct DbJobReport {
    path: String,
    histogram: memory_core::JobStatusHistogram,
    error: String,
}

fn collect_job_histograms(manifest: &crate::manifest::Manifest) -> Vec<DbJobReport> {
    let mut out = Vec::new();
    for entry in &manifest.dbs {
        // Read-only: open the connection directly without going through MemoryStore::open
        // which would try to write schema. We use rusqlite OpenFlags to be paranoid.
        let conn = match rusqlite::Connection::open_with_flags(
            &entry.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(c) => c,
            Err(e) => {
                out.push(DbJobReport {
                    path: entry.path.clone(),
                    histogram: memory_core::JobStatusHistogram::default(),
                    error: format!("open failed: {e}"),
                });
                continue;
            }
        };
        let hist = match memory_core::job_status_histogram(&conn, 30) {
            Ok(h) => h,
            Err(e) => {
                out.push(DbJobReport {
                    path: entry.path.clone(),
                    histogram: memory_core::JobStatusHistogram::default(),
                    error: format!("histogram failed: {e}"),
                });
                continue;
            }
        };
        // Skip silent zero-job DBs to keep the report focused.
        if hist.total > 0 {
            out.push(DbJobReport {
                path: entry.path.clone(),
                histogram: hist,
                error: String::new(),
            });
        }
    }
    out
}

async fn run_manifest_command(
    action: ManifestAction,
    home: &std::path::Path,
    app_home: &std::path::Path,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path = crate::manifest::Manifest::default_path(home);

    match action {
        ManifestAction::Show { json } => {
            let m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            if json {
                print_pretty_json(&serde_json::to_value(&m)?)
            } else {
                println!("{}", crate::manifest::render_manifest(&m));
                println!("(stored at {})", manifest_path.display());
                Ok(())
            }
        }
        ManifestAction::Init | ManifestAction::Refresh => {
            let roots = crate::doctor::default_scan_roots(home, git_root.map(|p| p.as_path()));
            let quarantine_dir = app_home.join("quarantine");
            let opts = crate::doctor::ScanOptions {
                auto_fix: false,
                max_depth: 10,
            };
            let report = crate::doctor::scan(&roots, &quarantine_dir, opts);
            let mut m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            m.populate_from_doctor(&report);
            m.save(&manifest_path)?;
            println!(
                "manifest written: {} ({} dbs)",
                manifest_path.display(),
                m.dbs.len()
            );
            Ok(())
        }
        ManifestAction::Resolve { target } => {
            let m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            // Try exact path first.
            if let Some(e) = m.lookup(&target) {
                println!(
                    "[exact] {} role={:?} owner={} writable={} schema={} last={}",
                    e.path, e.role, e.owner, e.allow_write, e.schema_kind, e.last_classification
                );
                return Ok(());
            }
            // Else interpret as a scope hint.
            let matches: Vec<_> = m
                .dbs
                .iter()
                .filter(|e| e.scope_hint == target || e.owner == target)
                .collect();
            if matches.is_empty() {
                println!(
                    "no manifest entry matches '{target}' (try `tachi manifest show` to list, or `tachi doctor` to refresh)"
                );
            } else {
                for e in matches {
                    println!(
                        "[scope] {} role={:?} owner={} writable={} schema={} last={}",
                        e.path,
                        e.role,
                        e.owner,
                        e.allow_write,
                        e.schema_kind,
                        e.last_classification
                    );
                }
            }
            Ok(())
        }
        ManifestAction::Sweep { apply, json } => {
            let roots = crate::doctor::default_scan_roots(home, git_root.map(|p| p.as_path()));
            let quarantine_dir = app_home.join("quarantine");
            let opts = crate::doctor::ScanOptions {
                auto_fix: false,
                max_depth: 10,
            };
            let report = crate::doctor::scan(&roots, &quarantine_dir, opts);
            let m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            let mut plan = crate::manifest::plan_sweep(&report, &m, &quarantine_dir);
            if apply {
                plan = crate::manifest::apply_sweep(plan, &quarantine_dir);
            }
            if json {
                print_pretty_json(&serde_json::to_value(&plan)?)
            } else {
                println!(
                    "sweep {}: planned={} applied={} skipped={}",
                    if apply { "applied" } else { "dry-run" },
                    plan.planned.len(),
                    plan.applied.len(),
                    plan.skipped.len()
                );
                for a in &plan.planned {
                    println!(
                        "  [plan]   {} → {}  ({})",
                        a.path,
                        a.quarantine_to.as_deref().unwrap_or("-"),
                        a.reason
                    );
                }
                for a in &plan.applied {
                    println!(
                        "  [moved]  {} → {}  ({})",
                        a.path,
                        a.quarantine_to.as_deref().unwrap_or("-"),
                        a.reason
                    );
                }
                for a in &plan.skipped {
                    println!("  [skip]   {}  ({})", a.path, a.note);
                }
                if !apply {
                    println!("\n(dry-run; re-run with --apply to move files)");
                }
                Ok(())
            }
        }
        ManifestAction::Gc { json } => {
            let report = crate::manifest::gc_manifest(&manifest_path)?;
            if json {
                print_pretty_json(&serde_json::to_value(&report)?)
            } else {
                println!(
                    "manifest gc: before={} after={} canonicalized={} removed_missing={} removed_fixture={} schema_kind_fixed={} dedup_collapsed={}{}",
                    report.entries_before,
                    report.entries_after,
                    report.canonicalized,
                    report.removed_missing,
                    report.removed_fixture,
                    report.schema_kind_fixed,
                    report.dedup_collapsed,
                    if report.aborted {
                        format!(" ABORTED: {}", report.abort_reason.as_deref().unwrap_or("?"))
                    } else {
                        String::new()
                    }
                );
                println!("(manifest at {})", manifest_path.display());
                Ok(())
            }
        }
    }
}

async fn run_rescue_command(
    action: RescueAction,
    home: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        RescueAction::Antigravity {
            source,
            targets_root,
            apply,
            json,
        } => {
            let source_path: PathBuf = source
                .unwrap_or_else(|| home.join(".gemini").join("antigravity").join("memory.db"));
            let targets_root_path: PathBuf =
                targets_root.unwrap_or_else(|| home.join(".tachi").join("projects"));

            if !source_path.exists() {
                return Err(
                    format!("rescue source DB not found: {}", source_path.display()).into(),
                );
            }
            if !targets_root_path.exists() {
                return Err(format!(
                    "rescue targets root not found: {}",
                    targets_root_path.display()
                )
                .into());
            }

            let plan = crate::rescue::plan_rescue(&source_path)
                .map_err(|e| format!("plan_rescue: {e}"))?;

            if !apply {
                if json {
                    print_pretty_json(&serde_json::to_value(&plan)?)
                } else {
                    println!("{}", crate::rescue::render_plan(&plan));
                    println!(
                        "\n(dry-run; re-run with --apply to write into target DBs at {})",
                        targets_root_path.display()
                    );
                    Ok(())
                }
            } else {
                let report = crate::rescue::apply_rescue(&source_path, &targets_root_path, plan)
                    .map_err(|e| format!("apply_rescue: {e}"))?;
                if json {
                    print_pretty_json(&serde_json::to_value(&report)?)
                } else {
                    println!("{}", crate::rescue::render_plan(&report.plan));
                    println!("\n=== apply summary ===");
                    let mut total_written = 0usize;
                    for (target, n) in &report.written_per_target {
                        println!("  written {:>16} <- {}", target, n);
                        total_written += n;
                    }
                    println!("  total written: {}", total_written);
                    println!("  skipped (already present): {}", report.skipped_existing);
                    if let Some(bk) = &report.source_backed_up_to {
                        println!("  source backed up to: {}", bk);
                    }
                    if !report.errors.is_empty() {
                        println!("\n=== errors ({}) ===", report.errors.len());
                        for e in &report.errors {
                            println!("  - {}", e);
                        }
                    }
                    Ok(())
                }
            }
        }
    }
}

// ─── `tachi env` handler ────────────────────────────────────────────────────

async fn run_env_command(
    global_db_path: &PathBuf,
    filter: Option<&str>,
    env_only: bool,
    stdin_password: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_cli_store_read_only(global_db_path)?;

    // 1. Check vault is initialized
    let config = store
        .vault_get_config()
        .map_err(|e| format!("Failed to read vault config: {e}"))?
        .ok_or_else(|| {
            "Vault not initialized. Run `tachi serve` and call vault_init first, \
             or set up the vault via an MCP client."
                .to_string()
        })?;

    // 2. Prompt for password
    let password = if stdin_password {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        buf.trim().to_string()
    } else {
        rpassword::prompt_password("Vault password: ")?
    };

    if password.is_empty() {
        return Err("Password cannot be empty".into());
    }

    // 3. Derive key and verify
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    let key = crate::vault_crypto::derive_key(&password, &salt)?;

    if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
        return Err("Wrong password".into());
    }

    // 4. List and decrypt all entries
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("Failed to list vault entries: {e}"))?;

    // Build glob pattern if provided
    let glob_pattern = filter.map(|f| glob::Pattern::new(f)).transpose()?;

    let mut emitted = 0usize;
    for entry in entries {
        // Skip agent-restricted secrets — those aren't meant for env injection
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }

        // --env-only: skip names that don't look like env vars (UPPER_SNAKE_CASE)
        if env_only && !entry.name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()) {
            continue;
        }

        // --filter: apply glob pattern
        if let Some(ref pat) = glob_pattern {
            if !pat.matches(&entry.name) {
                continue;
            }
        }

        let decrypted = match crate::vault_crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("WARNING: failed to decrypt '{}': {}", entry.name, e);
                continue;
            }
        };
        let value = match String::from_utf8(decrypted) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARNING: secret '{}' is not valid UTF-8: {}", entry.name, e);
                continue;
            }
        };

        if value.trim().is_empty() {
            continue;
        }

        // Shell-safe escaping: wrap in single quotes, escape embedded single quotes
        let escaped = value.replace('\'', "'\\''");
        println!("export {}='{}'", entry.name, escaped);
        emitted += 1;
    }

    eprintln!("# tachi env: {} secret(s) emitted", emitted);
    Ok(())
}

async fn run_cli_command(
    command: Commands,
    db_path: &PathBuf,
    project_db_path: Option<&PathBuf>,
    app_home: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Commands::Serve => Ok(()),
        Commands::Search { query, path, top_k } => {
            let store = open_cli_store_read_only(db_path)?;
            let path_prefix = path.clone();
            let query_vec = if store.vec_available {
                match crate::llm::LlmClient::new() {
                    Ok(llm) => match llm.embed_voyage(&query, "query").await {
                        Ok(vec) => Some(vec),
                        Err(e) => {
                            eprintln!(
                                "[cli search] query embedding failed, falling back to lexical-only search: {e}"
                            );
                            None
                        }
                    },
                    Err(e) => {
                        eprintln!(
                            "[cli search] LLM client init failed, falling back to lexical-only search: {e}"
                        );
                        None
                    }
                }
            } else {
                None
            };
            let results = store.search(
                &query,
                Some(SearchOptions {
                    top_k,
                    path_prefix,
                    query_vec,
                    record_access: false,
                    ..Default::default()
                }),
            )?;
            print_pretty_json(&json!({
                "query": query,
                "path": path,
                "top_k": top_k,
                "vector": store.vec_available,
                "warnings": ["audit/access write skipped: database opened read-only"],
                "results": results,
            }))
        }
        // `Commands::Save` was removed in favor of `Commands::Remember` which
        // carries `#[command(alias = "save")]`. The dispatcher therefore only
        // sees `Remember`, even when the user typed `tachi save TEXT`.
        Commands::Stats => {
            let store = open_cli_store_read_only(db_path)?;
            let stats = store.stats(false)?;
            print_pretty_json(&json!({
                "total": stats.total,
                "by_scope": stats.by_scope,
                "by_category": stats.by_category,
                "by_root_path": stats.by_root_path,
                "warnings": ["stats opened database read-only; no maintenance writes attempted"],
                "database": {
                    "path": db_path.display().to_string(),
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
        Commands::BackfillFts { .. } => {
            unreachable!("BackfillFts is handled before generic CLI dispatch")
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
        Commands::Hub { action } => {
            let store = open_cli_store(db_path)?;
            match action {
                HubAction::List { cap_type } => {
                    let caps = store.hub_list(cap_type.as_deref(), false)?;
                    print_pretty_json(&serde_json::to_value(caps)?)
                }
                HubAction::Register {
                    id,
                    cap_type,
                    name,
                    definition,
                    description,
                } => {
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
                    let updated = store.hub_set_enabled(&id, true)?;
                    print_pretty_json(&json!({
                        "updated": updated,
                        "id": id,
                        "enabled": true,
                    }))
                }
                HubAction::Disable { id } => {
                    let updated = store.hub_set_enabled(&id, false)?;
                    print_pretty_json(&json!({
                        "updated": updated,
                        "id": id,
                        "enabled": false,
                    }))
                }
                HubAction::Stats => {
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
        Commands::Foundry { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
        Commands::Repair { .. } => {
            // Pre-handled above before run_cli_command dispatch.
            Ok(())
        }
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
    if let Some(info) = crate::cli_client::detect_daemon(app_home).await {
        match crate::cli_client::call_daemon_tool(&info, tool_name, args.clone()).await {
            Ok(body) => return Ok(body),
            Err(e) => {
                eprintln!(
                    "[cli] daemon dispatch failed ({e}); falling back to in-process execution"
                );
            }
        }
    }
    let server = crate::cli_client::build_in_process_server(global_db, project_db)?;
    let body = in_process(server, args)
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(body)
}

pub(super) fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    tokio_main(cli)
}

/// Initialize the global tracing subscriber.
///
/// Sink resolution (PR7):
///   1. `~/.tachi/logs/tachi.log` (created if missing) — append mode.
///   2. `/tmp/tachi.log` fallback when (1) is unwritable (read-only HOME,
///      sandboxed runtime, etc.).
///   3. Pure stderr fallback when even /tmp is unwritable.
///
/// Filter level: `RUST_LOG` if set, else `info,memory_server=info,rmcp=warn`.
///
/// Sensitive-payload safety: this initializer ONLY wires the sink. It does not
/// add any new instrumentation that would log API keys, secrets, or large user
/// payloads. Existing call sites already redact sensitive params (cf. the
/// `tool_audit` module). Adding `tracing::info!` calls that include user text
/// is out of scope for PR7 — operators who need verbose payload logging must
/// opt in explicitly via `RUST_LOG=trace`.
///
/// This function is idempotent in practice (only the FIRST call wins because
/// `tracing::subscriber::set_global_default` returns Err on the second), and
/// errors are intentionally swallowed: a missing log sink must never crash
/// the daemon.
fn init_tracing(home: &std::path::Path) {
    use std::io::Write as _;
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,memory_server=info,rmcp=warn"));

    // Try the canonical location first, then /tmp, then bare stderr.
    let log_dir = home.join(".tachi").join("logs");
    let primary = log_dir.join("tachi.log");
    let fallback = std::path::PathBuf::from("/tmp/tachi.log");

    let opener = |path: &std::path::Path| -> Option<std::fs::File> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    };

    let (writer, sink_label): (Box<dyn std::io::Write + Send + Sync>, String) =
        if let Some(file) = opener(&primary) {
            (Box::new(file), primary.display().to_string())
        } else if let Some(file) = opener(&fallback) {
            (Box::new(file), fallback.display().to_string())
        } else {
            (Box::new(std::io::stderr()), "stderr".to_string())
        };

    // Wrap the writer behind Mutex so MakeWriter can hand out shared refs.
    let shared: std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send + Sync>>> =
        std::sync::Arc::new(std::sync::Mutex::new(writer));

    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send + Sync>>>);
    impl std::io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }

    let make_writer = move || SharedWriter(shared.clone());

    let file_layer = fmt::Layer::new()
        .with_writer(make_writer)
        .with_ansi(false)
        .with_target(true);

    // Best-effort registration. A second call (e.g. from a test) becomes a
    // no-op because `set_global_default` was already set.
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .try_init();

    // Mirror the sink choice to stderr so operators can find their logs.
    let _ = writeln!(std::io::stderr(), "tachi: logging to {sink_label}");
}

/// Liveness probe for the manifest self-lock fix: returns true iff some
/// process *other than us* currently holds the file at `db_path` open.
/// Uses `lsof` on Unix (best-effort: missing binary, non-zero exit, or
/// non-Unix platform → returns false, falling back to the prior pid-file
/// check). Critical for the stdio MCP case where the holder never wrote
/// `~/.tachi/daemon.pid`.
fn db_path_held_by_other_process(db_path: &str) -> bool {
    #[cfg(unix)]
    {
        use std::process::Command;
        let our_pid = std::process::id().to_string();
        // -t prints PIDs, one per line. -F p would also work but -t is portable.
        let output = Command::new("lsof")
            .arg("-t")
            .arg("--")
            .arg(db_path)
            .output();
        match output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout
                    .lines()
                    .any(|line| line.trim() != our_pid && !line.trim().is_empty())
            }
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = db_path;
        false
    }
}

#[tokio::main]
async fn tokio_main(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // PR7 — install the tracing sink before doing anything else so early errors
    // (config load, manifest resolution, daemon bind) are captured.
    let home_for_logs = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    init_tracing(&home_for_logs);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "tachi memory-server starting"
    );

    // Load config from dotenv files (same as before)
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let expand_user_path = |raw: &str| {
        if raw == "~" {
            home.clone()
        } else if let Some(rest) = raw.strip_prefix("~/") {
            home.join(rest)
        } else {
            PathBuf::from(raw)
        }
    };

    let app_home = std::env::var("TACHI_HOME")
        .or_else(|_| std::env::var("SIGIL_HOME"))
        .map(|v| expand_user_path(&v))
        .unwrap_or_else(|_| home.join(".tachi"));
    let git_root = find_git_root();

    let expand_cli_path = |raw: &PathBuf| expand_user_path(raw.to_string_lossy().as_ref());

    let _ = dotenvy::from_path(home.join(".secrets/master.env"));
    let _ = dotenvy::from_path_override(app_home.join("config.env"));
    let _ = dotenvy::from_path_override(PathBuf::from(".tachi/config.env"));
    // Backward compatibility with old Sigil paths
    let _ = dotenvy::from_path_override(home.join(".sigil/config.env"));
    let _ = dotenvy::from_path_override(PathBuf::from(".sigil/config.env"));

    // Project-local dotenv support (non-overriding):
    // - current working directory .env
    // - git root .env (if different from cwd)
    if let Ok(cwd) = std::env::current_dir() {
        let _ = dotenvy::from_path(cwd.join(".env"));
        if let Some(root) = git_root.as_ref() {
            if root != &cwd {
                let _ = dotenvy::from_path(root.join(".env"));
            }
        }
    }

    let command = cli.command.clone().unwrap_or(Commands::Serve);

    // Resolve global DB path
    let global_db_path = if let Some(p) = cli.global_db.as_ref() {
        expand_cli_path(p)
    } else if let Ok(p) = std::env::var("MEMORY_DB_PATH") {
        expand_user_path(&p)
    } else {
        let default_global = app_home.join("global/memory.db");
        // Migration: move legacy DBs into ${TACHI_HOME}/global/memory.db
        let legacy_candidates = vec![
            app_home.join("memory.db"),
            home.join(".sigil/global/memory.db"),
            home.join(".sigil/memory.db"),
        ];
        if !default_global.exists() {
            for legacy in legacy_candidates {
                if legacy.exists() {
                    if let Some(parent) = default_global.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::copy(&legacy, &default_global).await?;
                    eprintln!(
                        "Migrated legacy DB: {} -> {}",
                        legacy.display(),
                        default_global.display()
                    );
                    break;
                }
            }
        }
        default_global
    };

    // ─── Manifest-driven resolution (PR5) ───────────────────────────────────
    //
    // The manifest (`~/.tachi/manifest.json`, populated by `tachi doctor` /
    // `tachi manifest refresh`) is the durable source of truth for "which
    // memory.db file is the global one". Heuristic discovery above can pick a
    // freshly created empty DB when the user actually has data living at a
    // non-standard path. Consult the manifest:
    //
    //   * No CLI override + manifest has a global entry  → switch to manifest path.
    //   * Explicit --global-db / MEMORY_DB_PATH that disagrees with the manifest
    //     → honor the override but warn loudly so the user can spot misconfig.
    //   * Manifest entry exists but is marked non-writable (last_classification
    //     other than "healthy") → refuse with an actionable hint.
    //
    // Failure to read the manifest is non-fatal: we proceed with the heuristic
    // result so first-run installs still work without `tachi doctor`.
    let manifest_path = crate::manifest::Manifest::default_path(&home);
    // PR-2: hygiene pass before any manifest consumer reads. Idempotent.
    // Failures are logged and swallowed — startup must never block on GC.
    if matches!(command, Commands::Serve) && manifest_path.exists() {
        match crate::manifest::gc_manifest(&manifest_path) {
            Ok(report) => {
                if report.aborted {
                    tracing::warn!(
                        target: "tachi::manifest::gc",
                        reason = report.abort_reason.as_deref().unwrap_or(""),
                        "manifest GC aborted at startup"
                    );
                } else if report.canonicalized
                    + report.removed_missing
                    + report.removed_fixture
                    + report.schema_kind_fixed
                    + report.dedup_collapsed
                    > 0
                {
                    tracing::info!(
                        target: "tachi::manifest::gc",
                        before = report.entries_before,
                        after = report.entries_after,
                        canonicalized = report.canonicalized,
                        removed_missing = report.removed_missing,
                        removed_fixture = report.removed_fixture,
                        schema_kind_fixed = report.schema_kind_fixed,
                        dedup_collapsed = report.dedup_collapsed,
                        "manifest GC applied at startup"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(target: "tachi::manifest::gc", error = %e, "manifest GC failed; continuing")
            }
        }
    }
    let manifest_opt = if manifest_path.exists() {
        crate::manifest::Manifest::load(&manifest_path).ok()
    } else {
        None
    };
    let global_db_path = if let Some(m) = manifest_opt.as_ref() {
        if let Some(entry) = m.global() {
            let manifest_global = PathBuf::from(&entry.path);
            let user_overrode =
                cli.global_db.is_some() || std::env::var_os("MEMORY_DB_PATH").is_some();
            if user_overrode {
                if global_db_path != manifest_global {
                    eprintln!(
                        "warning: --global-db ({}) does not match manifest global ({}). \
                         Honoring CLI override; run `tachi manifest refresh` to update the manifest.",
                        global_db_path.display(),
                        manifest_global.display()
                    );
                }
                global_db_path
            } else {
                // No override → trust the manifest.
                // Escape hatch: TACHI_BYPASS_MANIFEST=1 lets users (and the
                // daemon itself when restarting after a crash) skip the
                // manifest write guard so a misclassified-but-healthy DB
                // does not self-lock the CLI. Doctor / manifest commands
                // never go through this path.
                let bypass = std::env::var("TACHI_BYPASS_MANIFEST")
                    .ok()
                    .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
                    .unwrap_or(false);
                if !bypass {
                    if let Err(err) = m.check_writable(&entry.path) {
                        // Before failing hard, check if any live process
                        // is already holding this DB — that is the most
                        // common cause of a false-positive WalOrphan
                        // classification. Two checks:
                        //   (a) HTTP daemon pid file (~/.tachi/daemon.pid),
                        //       only exists when started via `tachi --daemon`;
                        //   (b) `lsof` on the DB path itself — covers
                        //       stdio MCP instances that never write a pid
                        //       file (the common case: editor/IDE spawned
                        //       tachi as a subprocess).
                        let daemon_alive =
                            crate::cli_client::detect_daemon(&app_home).await.is_some();
                        let db_held = db_path_held_by_other_process(&entry.path);
                        if !daemon_alive && !db_held {
                            return Err(format!(
                                "manifest global DB is not writable: {err}. \
                                 Run `tachi doctor` to inspect, then `tachi manifest refresh` once resolved. \
                                 To force-bypass: TACHI_BYPASS_MANIFEST=1"
                            )
                            .into());
                        }
                        eprintln!(
                            "info: manifest reports {} as not writable, but a live holder was detected (daemon={daemon_alive}, lsof={db_held}) — proceeding.",
                            entry.path
                        );
                    }
                }
                if global_db_path != manifest_global {
                    eprintln!(
                        "info: using manifest global DB ({}) instead of heuristic default ({}).",
                        manifest_global.display(),
                        global_db_path.display()
                    );
                }
                manifest_global
            }
        } else {
            global_db_path
        }
    } else {
        global_db_path
    };

    if let Some(parent) = global_db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // BackfillVectors needs async (LLM client), handle it here before sync dispatch
    if let Commands::BackfillVectors {
        db,
        batch_size,
        dry_run,
    } = &command
    {
        let target_path = if let Some(p) = db {
            expand_user_path(p.to_string_lossy().as_ref())
        } else {
            global_db_path.clone()
        };
        return run_backfill_vectors(&target_path, *batch_size, *dry_run).await;
    }

    if let Commands::BackfillSummaries { db, dry_run } = &command {
        let target_path = if let Some(p) = db {
            expand_user_path(p.to_string_lossy().as_ref())
        } else {
            global_db_path.clone()
        };
        return run_backfill_summaries(&target_path, *dry_run).await;
    }

    if let Commands::BackfillFts { db, full, dry_run } = &command {
        let target_path = if let Some(p) = db {
            expand_user_path(p.to_string_lossy().as_ref())
        } else {
            global_db_path.clone()
        };
        return run_backfill_fts(&target_path, *full, *dry_run).await;
    }

    let gc_enabled = cli
        .gc_enabled
        .or_else(|| parse_env_bool("MEMORY_GC_ENABLED"))
        .unwrap_or(true);
    let gc_initial_delay_secs = cli
        .gc_initial_delay_secs
        .or_else(|| parse_env_u64("MEMORY_GC_INITIAL_DELAY_SECS"))
        .unwrap_or(300);
    let mut gc_interval_secs = cli
        .gc_interval_secs
        .or_else(|| parse_env_u64("MEMORY_GC_INTERVAL_SECS"))
        .unwrap_or(6 * 3600);
    if gc_interval_secs == 0 {
        eprintln!("MEMORY_GC_INTERVAL_SECS/--gc-interval-secs must be >= 1; using 1 second");
        gc_interval_secs = 1;
    }

    // Resolve project DB path
    let explicit_project_db = cli.project_db.is_some();
    let project_db_path = if cli.no_project_db {
        if cli.project_db.is_some() {
            eprintln!("--project-db is ignored because --no-project-db is set");
        }
        None
    } else if let Some(p) = cli.project_db.as_ref() {
        Some(expand_cli_path(p))
    } else if let Some(root) = git_root.as_ref() {
        let project_default = root.join(".tachi/memory.db");
        let project_legacy = root.join(".sigil/memory.db");

        if project_legacy.exists() && !project_default.exists() {
            if let Some(parent) = project_default.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(&project_legacy, &project_default).await?;
            eprintln!(
                "Migrated legacy project DB: {} -> {}",
                project_legacy.display(),
                project_default.display()
            );
        }

        Some(project_default)
    } else {
        None
    };

    if cli.daemon && project_db_path.is_some() {
        if explicit_project_db {
            eprintln!(
                "Daemon mode: using explicit project DB path {} (single-project mode)",
                project_db_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<unknown>".to_string())
            );
        } else {
            // PR-4: previously this branch *disabled* the auto-detected
            // project DB to avoid mixed project context. With the multi-DB
            // FoundryScheduler in place, all manifest DBs receive foundry
            // coverage equally, so we keep the auto-detected project but
            // surface a clear warning so the operator knows the daemon's
            // bound project follows whichever cwd it was launched from.
            eprintln!(
                "Daemon mode: auto-detected project DB {} retained (multi-DB scheduler is active). \
                 Use --project-db PATH to pin a specific project, or unset to run global-only.",
                project_db_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<unknown>".to_string())
            );
        }
    }

    if let Commands::Setup { json } = &command {
        return run_setup_command(
            *json,
            &home,
            &app_home,
            &global_db_path,
            project_db_path.as_ref(),
            git_root.as_ref(),
        )
        .await;
    }

    if let Commands::Tidy { json, apply } = &command {
        let mut roots = vec![
            app_home.clone(),
            home.join(".sigil"),
            home.join(".gemini"),
            home.join(".openclaw"),
        ];
        if let Some(root) = git_root.as_ref() {
            roots.push(root.clone());
        }
        return run_tidy_command(*json, *apply, &app_home, roots, git_root.as_ref()).await;
    }

    if let Commands::Doctor {
        json,
        scan_only,
        roots,
        jobs,
    } = &command
    {
        return run_doctor_command(
            *json,
            *scan_only,
            roots.clone(),
            *jobs,
            &home,
            &app_home,
            git_root.as_ref(),
        )
        .await;
    }

    if let Commands::Manifest { action } = &command {
        return run_manifest_command(action.clone(), &home, &app_home, git_root.as_ref()).await;
    }

    if let Commands::Rescue { action } = &command {
        return run_rescue_command(action.clone(), &home).await;
    }

    if let Commands::Status {
        watch,
        json,
        hide_orphans,
    } = &command
    {
        return crate::status_ops::run_status(
            *watch,
            *json,
            *hide_orphans,
            &app_home,
            &global_db_path,
            project_db_path.as_deref(),
        )
        .await;
    }

    if let Commands::Daemon { action } = &command {
        return crate::status_ops::run_daemon(action.clone(), &app_home).await;
    }

    if let Commands::Foundry { action } = &command {
        return crate::status_ops::run_foundry(action.clone(), &app_home, &global_db_path).await;
    }

    if let Commands::Repair {
        action,
        db,
        rule,
        apply,
        no_backup,
        json,
    } = &command
    {
        return crate::repair::run_repair(
            action.clone(),
            db.clone(),
            rule.clone(),
            *apply,
            *no_backup,
            *json,
            &app_home,
        )
        .await;
    }

    if let Commands::Env {
        filter,
        env_only,
        stdin_password,
    } = &command
    {
        return run_env_command(
            &global_db_path,
            filter.as_deref(),
            *env_only,
            *stdin_password,
        )
        .await;
    }

    if !matches!(command, Commands::Serve) {
        return run_cli_command(
            command,
            &global_db_path,
            project_db_path.as_ref(),
            &app_home,
        )
        .await;
    }

    // Ensure parent dirs exist
    if let Some(parent) = global_db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(ref p) = project_db_path {
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let server = MemoryServer::new(global_db_path.clone(), project_db_path.clone())?;

    let requested_tool_profile = cli
        .profile
        .clone()
        .or_else(|| std::env::var("TACHI_PROFILE").ok());
    if let Some(raw_profile) = requested_tool_profile.as_deref() {
        match crate::profiles::parse_tool_profile(raw_profile) {
            Some(profile) => server.set_tool_profile(Some(profile)),
            None => eprintln!(
                "Ignoring unknown tool profile '{}'; expected observe | remember | coordinate | operate | admin or a compatible host alias",
                raw_profile
            ),
        }
    }

    // Spawn idle connection cleanup task
    {
        let pool = server.pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let mut conns = lock_or_recover(&pool.connections, "mcp_pool.connections");
                let now = Instant::now();
                let idle_ttl = pool.idle_ttl;
                let stale: Vec<String> = conns
                    .iter()
                    .filter(|(_, c)| now.duration_since(c.last_used) > idle_ttl)
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in stale {
                    if let Some(conn) = conns.remove(&key) {
                        eprintln!("Idle cleanup: disconnecting '{}'", key);
                        drop(conn);
                    }
                }
            }
        });
    }

    if gc_enabled {
        eprintln!(
            "Background GC enabled (initial_delay={}s, interval={}s)",
            gc_initial_delay_secs, gc_interval_secs
        );
        let gc_server = server.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(gc_initial_delay_secs)).await;
            let mut interval = tokio::time::interval(Duration::from_secs(gc_interval_secs));
            loop {
                interval.tick().await;
                eprintln!("[gc] Running scheduled garbage collection...");
                match gc_server.with_global_store(|store: &mut MemoryStore| {
                    let mut gc = store
                        .gc_tables(&memory_core::GcConfig::default())
                        .map_err(|e| format!("{e}"))?;
                    let kanban_deleted =
                        gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
                    if let Some(object) = gc.as_object_mut() {
                        object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
                    }
                    // Auto-archive stale memories (configurable via MEMORY_GC_STALE_DAYS env var)
                    let stale_days: u32 = std::env::var("MEMORY_GC_STALE_DAYS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(90);
                    match store.archive_stale_memories(stale_days) {
                        Ok(archived) => {
                            if archived > 0 {
                                eprintln!("[gc] Archived {} stale memories", archived);
                            }
                            if let Some(object) = gc.as_object_mut() {
                                object.insert("memories_archived".into(), json!(archived));
                            }
                        }
                        Err(e) => eprintln!("[gc] archive_stale_memories error: {}", e),
                    }
                    Ok(gc)
                }) {
                    Ok(result) => eprintln!("[gc] Global DB: {}", result),
                    Err(e) => eprintln!("[gc] Global DB error: {}", e),
                }
                if gc_server.has_project_db() {
                    match gc_server.with_project_store(|store: &mut MemoryStore| {
                        let mut gc = store
                            .gc_tables(&memory_core::GcConfig::default())
                            .map_err(|e| format!("{e}"))?;
                        let kanban_deleted =
                            gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
                        if let Some(object) = gc.as_object_mut() {
                            object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
                        }
                        // Auto-archive stale memories (configurable via MEMORY_GC_STALE_DAYS env var)
                        let stale_days: u32 = std::env::var("MEMORY_GC_STALE_DAYS")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(90);
                        match store.archive_stale_memories(stale_days) {
                            Ok(archived) => {
                                if archived > 0 {
                                    eprintln!(
                                        "[gc] Archived {} stale memories (project)",
                                        archived
                                    );
                                }
                                if let Some(object) = gc.as_object_mut() {
                                    object.insert("memories_archived".into(), json!(archived));
                                }
                            }
                            Err(e) => eprintln!("[gc] archive_stale_memories error: {}", e),
                        }
                        Ok(gc)
                    }) {
                        Ok(result) => eprintln!("[gc] Project DB: {}", result),
                        Err(e) => eprintln!("[gc] Project DB error: {}", e),
                    }
                }
            }
        });
    } else {
        eprintln!("Background GC disabled");
    }

    // ─── Clawdoctor: OpenClaw health monitor ─────────────────────────────────
    {
        let clawdoctor_enabled = cli
            .clawdoctor
            .or_else(|| parse_env_bool("CLAWDOCTOR_ENABLED"))
            .unwrap_or(false);

        if clawdoctor_enabled {
            let cd_url = cli
                .clawdoctor_url
                .clone()
                .or_else(|| std::env::var("CLAWDOCTOR_URL").ok())
                .unwrap_or_else(|| "http://127.0.0.1:18789".to_string());

            let cd_interval = cli
                .clawdoctor_interval_secs
                .or_else(|| parse_env_u64("CLAWDOCTOR_INTERVAL_SECS"))
                .unwrap_or(crate::clawdoctor::DEFAULT_CLAWDOCTOR_INTERVAL_SECS);

            let cd_threshold = cli
                .clawdoctor_fail_threshold
                .or_else(|| {
                    std::env::var("CLAWDOCTOR_FAIL_THRESHOLD")
                        .ok()
                        .and_then(|v| v.parse::<u32>().ok())
                })
                .unwrap_or(crate::clawdoctor::DEFAULT_CLAWDOCTOR_FAIL_THRESHOLD);

            eprintln!(
                "Clawdoctor enabled (url={}, interval={}s, threshold={})",
                cd_url, cd_interval, cd_threshold
            );

            let cd_server = server.clone();
            tokio::spawn(async move {
                crate::clawdoctor::run_clawdoctor(cd_server, cd_url, cd_interval, cd_threshold)
                    .await;
            });
        } else {
            eprintln!("Clawdoctor disabled (set CLAWDOCTOR_ENABLED=true to enable)");
        }
    }

    // Integrity check on global DB
    server
        .with_global_store(|store| {
            match store.quick_check() {
                Ok(true) => eprintln!("Global database integrity: OK"),
                Ok(false) => eprintln!("WARNING: Global database may be corrupted!"),
                Err(e) => eprintln!("WARNING: Could not check global database integrity: {e}"),
            }
            Ok(())
        })
        .map_err(|e| format!("startup check: {e}"))?;

    // Integrity check on project DB
    if project_db_path.is_some() {
        server
            .with_project_store(|store| {
                match store.quick_check() {
                    Ok(true) => eprintln!("Project database integrity: OK"),
                    Ok(false) => eprintln!("WARNING: Project database may be corrupted!"),
                    Err(e) => {
                        eprintln!("WARNING: Could not check project database integrity: {e}")
                    }
                }
                Ok(())
            })
            .map_err(|e| format!("startup check: {e}"))?;
    }

    // Load cached proxy tools from Hub
    {
        let load_proxy_tools = |store: &mut MemoryStore| -> Result<(), String> {
            let mcp_caps = store
                .hub_list(Some("mcp"), true)
                .map_err(|e| format!("hub list: {e}"))?;
            for cap in mcp_caps {
                let def: serde_json::Value = match serde_json::from_str(&cap.definition) {
                    Ok(def) => def,
                    Err(e) => {
                        eprintln!(
                            "[startup] skip MCP '{}' due to invalid definition JSON: {e}",
                            cap.id
                        );
                        continue;
                    }
                };
                if let Some(tools_json) = def.get("discovered_tools") {
                    match serde_json::from_value::<Vec<rmcp::model::Tool>>(tools_json.clone()) {
                        Ok(tools) => {
                            let server_name = cap.id.strip_prefix("mcp:").unwrap_or(&cap.id);
                            let filtered_tools = filter_mcp_tools_by_permissions(&def, tools);
                            lock_or_recover(&server.proxy_tools, "proxy_tools")
                                .insert(server_name.to_string(), filtered_tools);
                        }
                        Err(e) => {
                            eprintln!(
                                "[startup] skip cached tools for '{}' due to invalid tool payload: {e}",
                                cap.id
                            );
                        }
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = server.with_global_store(load_proxy_tools) {
            eprintln!("[startup] failed loading global MCP proxy cache: {e}");
        }
        if server.has_project_db() {
            if let Err(e) = server.with_project_store(load_proxy_tools) {
                eprintln!("[startup] failed loading project MCP proxy cache: {e}");
            }
        }
    }
    {
        let load_skill_tools = |store: &mut MemoryStore| -> Result<(), String> {
            let skill_caps = store
                .hub_list(Some("skill"), true)
                .map_err(|e| format!("hub list: {e}"))?;
            for cap in skill_caps {
                if should_expose_skill_tool(&cap) {
                    if let Err(e) = server.register_skill_tool(&cap) {
                        eprintln!(
                            "[startup] failed to register skill tool for '{}': {}",
                            cap.id, e
                        );
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = server.with_global_store(load_skill_tools) {
            eprintln!("[startup] failed loading global skill tools: {e}");
        }
        if server.has_project_db() {
            if let Err(e) = server.with_project_store(load_skill_tools) {
                eprintln!("[startup] failed loading project skill tools: {e}");
            }
        }
    }

    if server.pipeline_enabled {
        eprintln!("Pipeline workers: ENABLED (external)");

        let distill_interval_secs: u64 = std::env::var("DISTILL_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1800);

        let distill_server = server.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let mut interval = tokio::time::interval(Duration::from_secs(distill_interval_secs));
            eprintln!(
                "Distill scheduler: ENABLED (interval={}s)",
                distill_interval_secs
            );
            loop {
                interval.tick().await;
                match crate::foundry_runtime_ops::schedule_pending_distill_jobs(&distill_server)
                    .await
                {
                    Ok(count) => {
                        if count > 0 {
                            eprintln!("[distill] Scheduled {count} distill jobs");
                        }
                    }
                    Err(err) => eprintln!("[distill] Scheduler error: {err}"),
                }
            }
        });
    } else {
        eprintln!("Pipeline workers: DISABLED (set ENABLE_PIPELINE=true to enable)");
    }

    eprintln!("Starting Tachi MCP Server v{}", env!("CARGO_PKG_VERSION"));
    eprintln!(
        "Transport: {}",
        if cli.daemon {
            format!("HTTP daemon on port {}", cli.port)
        } else {
            "stdio".to_string()
        }
    );
    eprintln!("Global DB: {}", global_db_path.display());
    if let Some(ref p) = project_db_path {
        eprintln!("Project DB: {}", p.display());
    } else {
        eprintln!("Project DB: none (not in a git repository)");
    }
    eprintln!(
        "Vector search: global={}, project={}",
        server.global_vec_available, server.project_vec_available
    );
    eprintln!(
        "Tool surface: {}",
        server
            .active_tool_profile()
            .map(|profile| profile.as_str())
            .unwrap_or_else(|| "admin".to_string())
    );

    if cli.daemon {
        // HTTP daemon mode
        // In daemon mode, project DB auto-detection is disabled above to avoid
        // mixed project context. Users can still opt into single-project mode
        // via explicit --project-db.

        // PR-4 singleton enforcement: acquire ~/.tachi/daemon.lock (flock +
        // PID file) before binding the HTTP port so a duplicate daemon
        // fails fast with a clear error instead of racing the first one
        // for DB writes. The guard is held for the whole daemon lifetime
        // and Drop releases the flock + unlinks the file.
        let lock_path = app_home.join("daemon.lock");
        let _daemon_lock = match crate::daemon_lock::DaemonLock::acquire(&lock_path) {
            Ok(g) => {
                eprintln!(
                    "[daemon] acquired singleton lock at {} (pid {})",
                    lock_path.display(),
                    std::process::id()
                );
                g
            }
            Err(e) => {
                eprintln!(
                    "[daemon] refusing to start: another tachi daemon already holds {} ({e})",
                    lock_path.display()
                );
                return Err(format!("tachi daemon singleton lock unavailable: {e}").into());
            }
        };

        // PR-4 multi-DB scheduler: periodically scan ~/.tachi/manifest.json
        // and run a per-DB safety-net poll against foundry_jobs. Re-injects
        // pending jobs into the existing foundry_tx mpsc channel for
        // routable scopes (own global/project + named projects), counts
        // orphans for unroutable manifest entries (dark DBs).
        let manifest_path = app_home.join("manifest.json");
        let scheduler = crate::foundry_scheduler::FoundryScheduler::start(
            manifest_path.clone(),
            server.foundry_tx_clone(),
            server.global_db_path_buf(),
            server.project_db_path_buf(),
        );
        eprintln!(
            "[daemon] foundry scheduler started (manifest={})",
            manifest_path.display()
        );
        // Hold scheduler for the whole daemon lifetime; Drop cancels
        // the manifest watcher + per-DB workers.
        let _scheduler = scheduler;

        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };
        use tokio_util::sync::CancellationToken;

        let ct = CancellationToken::new();
        let ct_shutdown = ct.clone();
        let port = cli.port;

        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig {
                stateful_mode: true,
                cancellation_token: ct.child_token(),
                ..Default::default()
            },
        );

        let router = axum::Router::new().nest_service("/mcp", service);
        let bind_addr = format!("127.0.0.1:{port}");
        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

        eprintln!("Tachi daemon listening on http://{bind_addr}");

        // Write daemon discovery file so CLI invocations can forward writes
        // to the running daemon instead of contending for the DB write lock.
        let pid_path = app_home.join("daemon.pid");
        let pid_payload = serde_json::json!({
            "pid": std::process::id(),
            "port": port,
            "url": format!("http://{bind_addr}/mcp"),
            "global_db": global_db_path.display().to_string(),
            "project_db": project_db_path
                .as_ref()
                .map(|p| p.display().to_string()),
            "started_at": Utc::now().to_rfc3339(),
            "version": env!("CARGO_PKG_VERSION"),
        });
        if let Some(parent) = pid_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if let Err(e) = tokio::fs::write(
            &pid_path,
            serde_json::to_string_pretty(&pid_payload).unwrap_or_default(),
        )
        .await
        {
            eprintln!(
                "warning: failed to write daemon discovery file {}: {e}",
                pid_path.display()
            );
        }
        let pid_path_cleanup = pid_path.clone();

        tokio::select! {
            result = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct_shutdown.cancelled_owned().await }) => {
                if let Err(e) = result {
                    eprintln!("HTTP server error: {e}");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("Received SIGINT, shutting down gracefully...");
                ct.cancel();
            }
        }

        // Best-effort cleanup of daemon discovery file
        let _ = tokio::fs::remove_file(&pid_path_cleanup).await;
    } else {
        // stdio mode (default)
        let transport = (stdin(), stdout());
        let running = rmcp::service::serve_server(server, transport).await?;

        // Graceful shutdown: wait for either MCP quit or SIGINT/SIGTERM
        tokio::select! {
            quit_reason = running.waiting() => {
                eprintln!("Memory MCP Server stopped: {:?}", quit_reason);
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("Received SIGINT, shutting down gracefully...");
            }
        }
    }

    Ok(())
}

/// Backfill missing vector embeddings for a given DB.
async fn run_backfill_vectors(
    db_path: &PathBuf,
    batch_size: usize,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::llm::LlmClient;

    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;

    let store = MemoryStore::open(db_str)?;
    let (total, with_vec) = store.vector_stats()?;
    let missing = total - with_vec;

    println!("DB:      {}", db_path.display());
    println!("Total:   {total}");
    println!("Vectors: {with_vec}");
    println!("Missing: {missing}");

    if missing == 0 {
        println!("\n✅ All entries have vectors!");
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run mode, no changes made)");
        return Ok(());
    }

    let llm = LlmClient::new().map_err(|e| format!("LLM client init failed: {e}"))?;
    let entries = store.entries_missing_vectors()?;

    let batch_size = batch_size.min(128).max(1);
    let total_missing = entries.len();
    let mut processed = 0usize;

    println!("\nBackfilling {total_missing} entries (batch_size={batch_size})...\n");

    // Re-open as mutable for update_enrichment_fields
    drop(store);
    let mut store = MemoryStore::open(db_str)?;

    for chunk in entries.chunks(batch_size) {
        let texts: Vec<String> = chunk
            .iter()
            .map(|(_, text, summary, _)| {
                let t = text.trim();
                let s = if t.len() < 10 { summary.as_str() } else { t };
                if s.len() > 8000 {
                    s[..8000].to_string()
                } else {
                    s.to_string()
                }
            })
            .collect();

        match llm.embed_voyage_batch(&texts, "document").await {
            Ok(vecs) => {
                for (i, (id, _, _, revision)) in chunk.iter().enumerate() {
                    if i < vecs.len() {
                        match store.update_enrichment_fields(id, None, Some(&vecs[i]), *revision) {
                            Ok(true) => {}
                            Ok(false) => eprintln!("  WARN: revision mismatch for {id}, skipped"),
                            Err(e) => eprintln!("  WARN: DB write failed for {id}: {e}"),
                        }
                    }
                }
                processed += chunk.len();
                println!("  [{processed}/{total_missing}] ✓ batch of {}", chunk.len());
            }
            Err(e) => {
                eprintln!("  ERROR: Voyage API failed: {e}");
                eprintln!("  Stopping. {processed} entries saved successfully.");
                break;
            }
        }

        if processed < total_missing {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    let (total, final_vec) = store.vector_stats()?;
    println!("\n✅ Done! Vectors: {with_vec} → {final_vec} / {total}");
    Ok(())
}

/// Backfill missing summaries for a given DB.
async fn run_backfill_summaries(
    db_path: &PathBuf,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::llm::LlmClient;

    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;

    let store = MemoryStore::open(db_str)?;
    let total = store.stats(false)?.total;
    let entries = store.entries_missing_summaries()?;
    let missing = entries.len();
    let with_summary = total.saturating_sub(missing as u64);

    println!("DB:        {}", db_path.display());
    println!("Total:     {total}");
    println!("Summaries: {with_summary}");
    println!("Missing:   {missing}");

    if missing == 0 {
        println!("\n✅ All entries have summaries!");
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run mode, no changes made)");
        return Ok(());
    }

    let llm = LlmClient::new().map_err(|e| format!("LLM client init failed: {e}"))?;

    println!("\nBackfilling {missing} entries...\n");

    drop(store);
    let mut store = MemoryStore::open(db_str)?;
    let mut processed = 0usize;

    for (id, text, revision) in &entries {
        let input: String = text.chars().take(8000).collect();
        let summary = llm.generate_summary(&input).await?;

        match store.update_enrichment_fields(id, Some(&summary), None, *revision) {
            Ok(true) => {
                processed += 1;
                println!("  [{processed}/{missing}] ✓ {id}");
            }
            Ok(false) => eprintln!("  WARN: revision mismatch for {id}, skipped"),
            Err(e) => eprintln!("  WARN: DB write failed for {id}: {e}"),
        }
    }

    let final_missing = store.entries_missing_summaries()?.len();
    let final_with_summary = total.saturating_sub(final_missing as u64);
    println!("\n✅ Done! Summaries: {with_summary} → {final_with_summary} / {total}");
    Ok(())
}

/// Rebuild or backfill FTS5 full-text search index for a given DB.
async fn run_backfill_fts(
    db_path: &PathBuf,
    full: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;

    let store = MemoryStore::open(db_str)?;
    let (total, with_fts) = store.fts_stats()?;
    let missing = total.saturating_sub(with_fts);

    println!("DB:      {}", db_path.display());
    println!("Total:   {total}");
    println!("FTS:     {with_fts}");
    println!("Missing: {missing}");
    println!(
        "Mode:    {}",
        if full { "full rebuild" } else { "incremental" }
    );

    if !full && missing == 0 {
        println!("\n✅ All entries have FTS index!");
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run mode, no changes made)");
        return Ok(());
    }

    drop(store);
    let mut store = MemoryStore::open(db_str)?;

    if full {
        println!("\nDropping and rebuilding FTS table...");
        let inserted = store.rebuild_fts_full()?;
        println!("\n✅ Full rebuild done! {inserted} entries indexed.");
    } else {
        println!("\nBackfilling {missing} missing FTS entries...");
        let inserted = store.backfill_fts_missing()?;
        let (_, final_fts) = store.fts_stats()?;
        println!("\n✅ Done! FTS: {with_fts} → {final_fts} / {total} (+{inserted})");
    }

    Ok(())
}

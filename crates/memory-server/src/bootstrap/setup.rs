use super::*;

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
        .map(|entry| {
            let status = if env_vars
                .get(entry.key)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                "configured"
            } else if entry.deprecated {
                "deprecated-unset"
            } else {
                "missing"
            };
            format!("{}: {} ({})", entry.key, status, entry.label)
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

pub(super) fn render_setup_report(report: &SetupReport) -> String {
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

/// Top-level dispatcher for `tachi setup`.
///
/// Routing:
///   * `--json`               → emit machine-readable report (never interactive)
///   * `--non-interactive`    → render the human report and exit
///   * `--interactive`        → always run the 5-step wizard, even without a TTY
///   * default                → wizard when stdout is a TTY, otherwise report
pub(super) async fn run_setup_command(
    json_output: bool,
    interactive: bool,
    non_interactive: bool,
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

    let want_interactive = interactive || (!non_interactive && atty_stdout());

    if !want_interactive {
        println!("{}", render_setup_report(&report));
        return Ok(());
    }

    // Print the report first so the user sees the current state before
    // making decisions inside the wizard.
    println!("{}\n", render_setup_report(&report));

    let config_env_path = app_home.join("config.env");
    let outcome = super::setup_wizard::run_interactive_wizard(
        home,
        app_home,
        &config_env_path,
        &report,
        &env_vars,
        global_db_path,
    )
    .await?;

    if outcome.aborted {
        println!("\nSetup wizard aborted; no changes written.");
    } else if outcome.wrote_changes {
        println!(
            "\nWrote {} entries to {}.",
            outcome.changed_keys.len(),
            config_env_path.display()
        );
        println!("Restart the daemon for changes to take effect.");
    } else {
        println!("\nNo new values to write.");
    }

    Ok(())
}

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

pub(super) async fn run_setup_command(
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

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Password};

use super::super::{count_matching_entries, SetupReport, SETUP_API_KEYS};
use super::agent_rules::install_agent_memory_rules;
use super::env::{is_secret_key, looks_like_api_key, mask_secret, merge_config_env};
use super::vault::{init_vault_inline, store_collected_keys_in_vault};
use super::SetupWizardOutcome;

pub(in crate::bootstrap) async fn run_interactive_wizard(
    home: &Path,
    app_home: &Path,
    config_env_path: &Path,
    report: &SetupReport,
    env_vars: &HashMap<String, String>,
    global_db_path: &PathBuf,
) -> Result<SetupWizardOutcome, Box<dyn Error>> {
    let theme = ColorfulTheme::default();

    println!("───────────────────────────────────────────────");
    println!(" Welcome to Tachi setup — 5 interactive steps");
    println!(" Press Ctrl+C at any prompt to abort.");
    println!("───────────────────────────────────────────────");

    let mut new_entries: Vec<(String, String)> = Vec::new();

    // ─── [1/5] API Keys ────────────────────────────────────────────────────
    println!("\n[1/5] API Keys");
    println!(
        "  Tachi uses Voyage (vectors), SiliconFlow (extraction), and optional DeepSeek (distill/reasoning)."
    );
    for entry in SETUP_API_KEYS.iter().filter(|entry| !entry.deprecated) {
        let key = entry.key;
        let label = entry.label;
        let existing = env_vars
            .get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        let prompt = if let Some(current) = existing.as_ref() {
            format!("{key} ({label}) — detected: {}", mask_secret(current))
        } else {
            format!("{key} ({label}) — not set")
        };
        println!("  • {prompt}");

        let should_set = if existing.is_some() {
            Confirm::with_theme(&theme)
                .with_prompt(format!("    overwrite {key}?"))
                .default(false)
                .interact()?
        } else {
            // Required keys are the first two; others are optional.
            let required = matches!(key, "VOYAGE_API_KEY" | "SILICONFLOW_API_KEY");
            Confirm::with_theme(&theme)
                .with_prompt(if required {
                    format!("    set {key} now? (recommended)")
                } else {
                    format!("    set {key} now? (optional)")
                })
                .default(required)
                .interact()?
        };

        if should_set {
            let secret = Password::with_theme(&theme)
                .with_prompt(format!("    {key} value"))
                .allow_empty_password(true)
                .interact()?;
            let secret = secret.trim().to_string();
            if secret.is_empty() {
                println!("    (skipped — empty input)");
                continue;
            }
            if !looks_like_api_key(&secret) {
                let proceed = Confirm::with_theme(&theme)
                    .with_prompt("    value looks short/unusual — store anyway?")
                    .default(false)
                    .interact()?;
                if !proceed {
                    println!("    (skipped)");
                    continue;
                }
            }
            new_entries.push((key.to_string(), secret));
        }
    }

    // ─── [2/5] Skills ──────────────────────────────────────────────────────
    println!("\n[2/5] Skills");
    println!("  Scanning known skill roots…");
    let skill_roots = [
        ("tachi", app_home.join("skills")),
        ("claude", home.join(".claude").join("skills")),
        ("codex", home.join(".codex").join("skills")),
        ("gemini", home.join(".gemini").join("skills")),
        ("cursor", home.join(".cursor").join("rules")),
    ];
    let mut total_skills = 0usize;
    for (label, root) in skill_roots.iter() {
        let count = if root.exists() {
            count_matching_entries(root, &|p| p.is_dir() || p.is_file())
        } else {
            0
        };
        total_skills += count;
        let marker = if root.exists() { "✓" } else { "·" };
        println!(
            "    {marker} {label:<8} {count:>3} entries  ({})",
            root.display()
        );
    }
    println!("  Total: {total_skills} skill entries detected.");
    if total_skills == 0 {
        println!(
            "  Tip: place SKILL.md files under {} to make them discoverable.",
            app_home.join("skills").display()
        );
    }
    let _ack_skills = Confirm::with_theme(&theme)
        .with_prompt("  Continue?")
        .default(true)
        .interact()?;

    // ─── [3/5] Agents ──────────────────────────────────────────────────────
    println!("\n[3/5] Agents");
    let agent_markers = [
        ("Claude Code", home.join(".claude")),
        ("Cursor", home.join(".cursor")),
        ("Codex", home.join(".codex")),
        ("Gemini", home.join(".gemini")),
        ("Windsurf", home.join(".windsurf")),
    ];
    let mut detected = 0usize;
    for (name, path) in agent_markers.iter() {
        let present = path.exists();
        if present {
            detected += 1;
        }
        let marker = if present { "✓" } else { "·" };
        println!(
            "    {marker} {name:<12} {} ({})",
            if present { "detected" } else { "missing" },
            path.display()
        );
    }
    println!("  Total: {detected} agent(s) detected.");
    let _ack_agents = Confirm::with_theme(&theme)
        .with_prompt("  Continue?")
        .default(true)
        .interact()?;

    if detected > 0 {
        let install_rules = Confirm::with_theme(&theme)
            .with_prompt(
                "  Install/update Tachi agent memory rules in detected agent config files?",
            )
            .default(true)
            .interact()?;
        if install_rules {
            let installed = install_agent_memory_rules(home)?;
            if installed.is_empty() {
                println!("    (no supported writable agent rule files found)");
            } else {
                for path in installed {
                    println!("    updated {}", path.display());
                }
            }
        }
    }

    // ─── [4/5] Pipeline ────────────────────────────────────────────────────
    println!("\n[4/5] Pipeline");
    let current_pipeline = env_vars
        .get("ENABLE_PIPELINE")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    println!(
        "  Current ENABLE_PIPELINE: {}",
        if current_pipeline {
            "true"
        } else {
            "false (or unset)"
        }
    );
    let enable = Confirm::with_theme(&theme)
        .with_prompt("  Enable the extraction pipeline?")
        .default(true)
        .interact()?;
    let desired = if enable { "true" } else { "false" };
    if (enable && !current_pipeline) || (!enable && current_pipeline) {
        new_entries.push(("ENABLE_PIPELINE".to_string(), desired.to_string()));
    }

    // ─── [5/5] Vault (optional) ────────────────────────────────────────────
    println!("\n[5/5] Vault (optional)");
    let vault_already = report
        .items
        .iter()
        .find(|i| i.id == "vault")
        .map(|i| i.status == "configured")
        .unwrap_or(false);

    // Which of the keys the user just entered are API-key secrets eligible for
    // encrypted-vault storage (vs. plaintext config.env)?
    let collected_secret_keys: Vec<String> = new_entries
        .iter()
        .filter(|(k, _)| is_secret_key(k))
        .map(|(k, _)| k.clone())
        .collect();

    if !collected_secret_keys.is_empty() {
        // Default YES: funnel freshly-entered keys into the encrypted vault and
        // write only `KEY=vault:KEY` alias lines to config.env instead of the
        // plaintext values. Declining keeps the existing plaintext behavior.
        let prompt = if vault_already {
            "  Store the API keys you just entered in the encrypted vault (recommended)?"
        } else {
            "  Store the API keys you just entered in an encrypted vault instead of plaintext (recommended)?"
        };
        let use_vault = Confirm::with_theme(&theme)
            .with_prompt(prompt)
            .default(true)
            .interact()?;

        if use_vault {
            match store_collected_keys_in_vault(
                global_db_path,
                vault_already,
                &collected_secret_keys,
                &mut new_entries,
                &theme,
            ) {
                Ok(stored) => {
                    println!(
                        "  Stored {stored} key(s) in the vault; config.env will use `vault:` aliases (no plaintext)."
                    );
                }
                Err(err) => {
                    // tachi#1080: a stored-KDF-format failure means the vault's
                    // stored config is unusable. It MUST NOT be swallowed into
                    // the plaintext fallback — that would silently persist the
                    // freshly entered keys as plaintext to config.env (a worse
                    // outcome than the original bug). Detect it by the typed
                    // error (downcast, NOT a fragile string match) and abort.
                    if err
                        .downcast_ref::<crate::vault_crypto::KdfParamsFormatError>()
                        .is_some()
                    {
                        return Err(err);
                    }
                    // Fall back to plaintext (unchanged behavior) so the wizard
                    // never strands the user with half-applied state.
                    println!(
                        "  Vault storage failed ({err}); keeping plaintext config.env values."
                    );
                }
            }
        } else {
            println!("  Keeping plaintext config.env values for the entered keys.");
        }
    } else if vault_already {
        println!("  Vault already initialized — skipping.");
    } else {
        let want_vault = Confirm::with_theme(&theme)
            .with_prompt("  Initialize a master-password vault now?")
            .default(false)
            .interact()?;
        if want_vault {
            init_vault_inline(global_db_path, &theme)?;
            println!("  Vault initialized.");
        }
    }

    // ─── Summary + write ──────────────────────────────────────────────────
    println!("\n───── Summary ─────");
    if new_entries.is_empty() {
        println!("  No config.env changes proposed.");
        return Ok(SetupWizardOutcome {
            changed_keys: Vec::new(),
            wrote_changes: false,
            aborted: false,
        });
    }
    println!("  Pending writes to {}:", config_env_path.display());
    for (k, v) in &new_entries {
        // `vault:` alias lines hold no secret material — show them verbatim.
        let shown = if crate::provider_config::is_vault_alias(v) {
            v.clone()
        } else if is_secret_key(k) {
            mask_secret(v)
        } else {
            v.clone()
        };
        println!("    {k}={shown}");
    }

    let confirm = Confirm::with_theme(&theme)
        .with_prompt(format!("Write to {}?", config_env_path.display()))
        .default(true)
        .interact()?;
    if !confirm {
        return Ok(SetupWizardOutcome {
            changed_keys: new_entries.into_iter().map(|(k, _)| k).collect(),
            wrote_changes: false,
            aborted: true,
        });
    }

    if let Some(parent) = config_env_path.parent() {
        std::fs::create_dir_all(parent)?;
        // Pre-flight write check: create and delete a temp file to verify permissions
        let probe = parent.join(".tachi_write_probe");
        std::fs::write(&probe, b"")?;
        let _ = std::fs::remove_file(&probe);
    }
    let existing = std::fs::read_to_string(config_env_path).unwrap_or_default();
    let merged = merge_config_env(&existing, &new_entries);
    std::fs::write(config_env_path, merged)?;

    Ok(SetupWizardOutcome {
        changed_keys: new_entries.into_iter().map(|(k, _)| k).collect(),
        wrote_changes: true,
        aborted: false,
    })
}

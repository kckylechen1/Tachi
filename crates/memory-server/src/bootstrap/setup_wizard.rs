//! Interactive 5-step onboarding wizard for `tachi setup`.
//!
//! Steps:
//!   1. API keys      (Voyage + SiliconFlow are required; others optional)
//!   2. Skills        (scan ~/.tachi/skills + agent skill directories)
//!   3. Agents        (detect installed MCP clients)
//!   4. Pipeline      (toggle ENABLE_PIPELINE)
//!   5. Vault         (optional master-password vault init)
//!
//! Pure helpers (`merge_config_env`, `mask_secret`, `looks_like_api_key`) live
//! at the bottom of this file and are covered by unit tests; the dialoguer
//! flow itself is intentionally not unit-tested.

use super::*;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Password};
use std::path::Path;

/// Result of a wizard run, returned to the dispatcher in `setup.rs`.
pub(super) struct SetupWizardOutcome {
    /// Keys (with their new values) that the user accepted.
    pub changed_keys: Vec<String>,
    /// True when `config.env` was written.
    pub wrote_changes: bool,
    /// True when the user explicitly aborted before the write step.
    pub aborted: bool,
}

pub(super) async fn run_interactive_wizard(
    home: &Path,
    app_home: &Path,
    config_env_path: &Path,
    report: &SetupReport,
    env_vars: &HashMap<String, String>,
    global_db_path: &PathBuf,
) -> Result<SetupWizardOutcome, Box<dyn std::error::Error>> {
    let theme = ColorfulTheme::default();

    println!("───────────────────────────────────────────────");
    println!(" Welcome to Tachi setup — 5 interactive steps");
    println!(" Press Ctrl+C at any prompt to abort.");
    println!("───────────────────────────────────────────────");

    let mut new_entries: Vec<(String, String)> = Vec::new();

    // ─── [1/5] API Keys ────────────────────────────────────────────────────
    println!("\n[1/5] API Keys");
    println!("  Tachi uses Voyage (embeddings) and SiliconFlow (extraction).");
    for (key, label) in SETUP_API_KEYS.iter() {
        let existing = env_vars
            .get(*key)
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
            let required = matches!(*key, "VOYAGE_API_KEY" | "SILICONFLOW_API_KEY");
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
    if vault_already {
        println!("  Vault already initialized — skipping.");
    } else {
        let want_vault = Confirm::with_theme(&theme)
            .with_prompt("  Initialize a master-password vault now?")
            .default(false)
            .interact()?;
        if want_vault {
            match init_vault_inline(global_db_path) {
                Ok(()) => println!("  Vault initialized."),
                Err(e) => println!("  Vault init skipped: {e}"),
            }
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
        let shown = if is_secret_key(k) {
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

/// Inline vault initializer used by step 5. Mirrors `vault_cli::VaultAction::Init`
/// but keeps the wizard self-contained; on any error we surface it to the caller
/// so the wizard can continue gracefully.
fn init_vault_inline(global_db_path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let store = open_cli_store_read_only(global_db_path)?;
    if store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .is_some()
    {
        return Err("vault already initialized".into());
    }
    drop(store);

    let password = rpassword::prompt_password("    New vault password: ")?;
    if password.is_empty() {
        return Err("password cannot be empty".into());
    }
    let confirm = rpassword::prompt_password("    Confirm password: ")?;
    if password != confirm {
        return Err("passwords do not match".into());
    }

    let salt = crate::vault_crypto::generate_salt();
    let key = crate::vault_crypto::derive_key(&password, &salt)?;
    let verifier = crate::vault_crypto::create_verifier(&key)?;
    let salt_b64 = B64.encode(salt);
    let now = chrono::Utc::now().to_rfc3339();

    let store = open_cli_store(global_db_path)?;
    store
        .vault_set_config(&memory_core::vault::VaultConfig {
            salt: salt_b64,
            verifier,
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: r#"{"m":65536,"t":3,"p":4}"#.to_string(),
            cipher: "aes-256-gcm".to_string(),
            created_at: now.clone(),
            updated_at: now,
        })
        .map_err(|e| format!("vault_set_config: {e}"))?;

    Ok(())
}

// ─── Pure helpers (unit-tested) ──────────────────────────────────────────────

/// Merge a list of (key, value) pairs into an existing `KEY=VALUE`-style file
/// body. Lines that already define the same key (commented or not) are replaced
/// in place; otherwise the new entry is appended. The output always ends with
/// a single trailing newline.
pub(super) fn merge_config_env(existing: &str, updates: &[(String, String)]) -> String {
    let mut lines: Vec<String> = existing.lines().map(String::from).collect();

    for (key, value) in updates {
        let prefix = format!("{key}=");
        let commented_prefix = format!("# {key}=");
        let mut replaced = false;
        for line in lines.iter_mut() {
            let trimmed = line.trim_start();
            if trimmed.starts_with(&prefix) || trimmed.starts_with(&commented_prefix) {
                *line = format!("{key}={value}");
                replaced = true;
                break;
            }
        }
        if !replaced {
            lines.push(format!("{key}={value}"));
        }
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// True for config keys that should be redacted in any user-visible output.
pub(super) fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("KEY")
        || upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
}

/// Render a redacted preview of a secret value (`voy_••••abcd` style).
pub(super) fn mask_secret(value: &str) -> String {
    let v = value.trim();
    let chars: Vec<char> = v.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    if n <= 4 {
        return "•".repeat(n);
    }
    if n <= 8 {
        let tail: String = chars[n - 2..].iter().collect();
        return format!("••••{tail}");
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[n - 4..].iter().collect();
    format!("{head}••••{tail}")
}

/// Cheap heuristic for "this looks like a real API key" — we don't enforce a
/// specific provider format, but we do flag obviously-too-short strings so
/// users get a confirmation prompt before storing them.
pub(super) fn looks_like_api_key(value: &str) -> bool {
    let v = value.trim();
    if v.len() < 16 {
        return false;
    }
    // Must be printable ASCII; rule out accidental shell paste of whitespace.
    v.chars().all(|c| c.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_appends_new_keys() {
        let body = "FOO=bar\n";
        let updates = vec![("BAZ".to_string(), "qux".to_string())];
        let out = merge_config_env(body, &updates);
        assert_eq!(out, "FOO=bar\nBAZ=qux\n");
    }

    #[test]
    fn merge_replaces_existing_key_in_place() {
        let body = "VOYAGE_API_KEY=old\nENABLE_PIPELINE=false\n";
        let updates = vec![("VOYAGE_API_KEY".to_string(), "new".to_string())];
        let out = merge_config_env(body, &updates);
        assert_eq!(out, "VOYAGE_API_KEY=new\nENABLE_PIPELINE=false\n");
    }

    #[test]
    fn merge_replaces_commented_key() {
        let body = "# VOYAGE_API_KEY=placeholder\nOTHER=1\n";
        let updates = vec![("VOYAGE_API_KEY".to_string(), "voy_real".to_string())];
        let out = merge_config_env(body, &updates);
        assert!(out.contains("VOYAGE_API_KEY=voy_real"));
        assert!(!out.contains("# VOYAGE_API_KEY=placeholder"));
        assert!(out.contains("OTHER=1"));
    }

    #[test]
    fn merge_always_ends_with_newline() {
        let body = "FOO=bar"; // no trailing newline
        let updates = vec![("BAZ".to_string(), "qux".to_string())];
        let out = merge_config_env(body, &updates);
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn merge_handles_empty_existing_file() {
        let updates = vec![("A".to_string(), "1".to_string())];
        let out = merge_config_env("", &updates);
        assert_eq!(out, "A=1\n");
    }

    #[test]
    fn merge_preserves_unrelated_lines_and_order() {
        let body = "# comment\nA=1\nB=2\nC=3\n";
        let updates = vec![("B".to_string(), "200".to_string())];
        let out = merge_config_env(body, &updates);
        assert_eq!(out, "# comment\nA=1\nB=200\nC=3\n");
    }

    #[test]
    fn mask_secret_long_value() {
        let m = mask_secret("voy_1234567890abcdef");
        assert!(m.starts_with("voy"));
        assert!(m.ends_with("cdef"));
        assert!(m.contains("•"));
        assert!(!m.contains("12345"));
    }

    #[test]
    fn mask_secret_short_values() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("ab"), "••");
        assert_eq!(mask_secret("abcdef"), "••••ef");
    }

    #[test]
    fn is_secret_key_recognises_common_prefixes() {
        assert!(is_secret_key("VOYAGE_API_KEY"));
        assert!(is_secret_key("GITHUB_TOKEN"));
        assert!(is_secret_key("OPENAI_SECRET"));
        assert!(is_secret_key("VAULT_PASSWORD"));
        assert!(!is_secret_key("ENABLE_PIPELINE"));
        assert!(!is_secret_key("TACHI_DAEMON_PORT"));
    }

    #[test]
    fn looks_like_api_key_basic_rules() {
        assert!(!looks_like_api_key("short"));
        assert!(!looks_like_api_key("has spaces in it nope"));
        assert!(looks_like_api_key("voy_1234567890abcdef"));
        assert!(looks_like_api_key("sk-proj-ABCDEFG1234567890"));
    }
}

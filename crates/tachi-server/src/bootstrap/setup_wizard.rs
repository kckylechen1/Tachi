use std::path::{Path, PathBuf};

pub(crate) mod agent_rules;
mod env;
#[cfg(test)]
mod tests;
mod vault;
mod wizard;

pub(super) use wizard::run_interactive_wizard;

/// Result of a wizard run, returned to the dispatcher in `setup.rs`.
#[derive(Debug, Default, Clone)]
pub(super) struct SetupWizardOutcome {
    /// Keys (with their new values) that the user accepted.
    pub changed_keys: Vec<String>,
    /// True when `config.env` was written.
    pub wrote_changes: bool,
    /// True when the user explicitly aborted before the write step.
    pub aborted: bool,
    /// Agent rule files updated in step 3.
    pub installed_rules: Vec<PathBuf>,
    /// Number of API keys stored in the encrypted vault in step 5.
    pub vault_keys_stored: usize,
    /// True when the vault database was newly initialized in step 5.
    pub vault_initialized: bool,
}

impl SetupWizardOutcome {
    pub(super) fn has_any_earlier_changes(&self) -> bool {
        !self.installed_rules.is_empty() || self.vault_keys_stored > 0 || self.vault_initialized
    }

    pub(super) fn summary_lines(
        &self,
        config_env_path: &Path,
        global_db_path: &Path,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        if self.aborted {
            if self.has_any_earlier_changes() {
                lines.push(format!(
                    "Setup wizard aborted before writing {}.",
                    config_env_path.display()
                ));
                lines.push("Earlier side effects applied:".to_string());
                if !self.installed_rules.is_empty() {
                    lines.push(format!(
                        "  • Updated {} agent rule file(s).",
                        self.installed_rules.len()
                    ));
                }
                if self.vault_keys_stored > 0 {
                    lines.push(format!(
                        "  • Stored {} key(s) in vault.",
                        self.vault_keys_stored
                    ));
                }
                if self.vault_initialized {
                    lines.push(format!(
                        "  • Initialized vault at {}.",
                        global_db_path.display()
                    ));
                }
                lines.push("No changes were written to config.env.".to_string());
            } else {
                lines.push("Setup wizard aborted; no changes written.".to_string());
            }
        } else if self.wrote_changes {
            lines.push(format!(
                "Wrote {} entries to {}.",
                self.changed_keys.len(),
                config_env_path.display()
            ));
            lines.push("Restart the daemon for changes to take effect.".to_string());
        } else if self.has_any_earlier_changes() {
            lines.push("No config.env values to write.".to_string());
            lines.push("Earlier changes applied:".to_string());
            if !self.installed_rules.is_empty() {
                lines.push(format!(
                    "  • Updated {} agent rule file(s).",
                    self.installed_rules.len()
                ));
            }
            if self.vault_keys_stored > 0 {
                lines.push(format!(
                    "  • Stored {} key(s) in vault.",
                    self.vault_keys_stored
                ));
            }
            if self.vault_initialized {
                lines.push(format!(
                    "  • Initialized vault at {}.",
                    global_db_path.display()
                ));
            }
        } else {
            lines.push("No new values to write.".to_string());
        }
        lines
    }
}

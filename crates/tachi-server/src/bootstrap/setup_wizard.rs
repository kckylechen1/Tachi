pub(crate) mod agent_rules;
mod env;
#[cfg(test)]
mod tests;
mod vault;
mod wizard;

pub(super) use wizard::run_interactive_wizard;

/// Result of a wizard run, returned to the dispatcher in `setup.rs`.
pub(super) struct SetupWizardOutcome {
    /// Keys (with their new values) that the user accepted.
    pub changed_keys: Vec<String>,
    /// True when `config.env` was written.
    pub wrote_changes: bool,
    /// True when the user explicitly aborted before the write step.
    pub aborted: bool,
}

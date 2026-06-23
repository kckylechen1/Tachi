use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvBinding {
    pub env_name: String,
    pub secret_name: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvIgnoredLine {
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvPlan {
    pub cwd: String,
    pub bindings_path: Option<String>,
    pub output_path: Option<String>,
    pub binding_count: usize,
    pub missing_count: usize,
    pub bindings: Vec<ProjectEnvBindingStatus>,
    pub ignored_lines: Vec<ProjectEnvIgnoredLine>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvBindingStatus {
    pub env_name: String,
    pub secret_name: String,
    pub line: usize,
    pub status: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ProjectEnvSyncReport {
    pub(super) cwd: String,
    pub(super) bindings_path: String,
    pub(super) output_path: String,
    pub(super) binding_count: usize,
    pub(super) written: bool,
    pub(super) dry_run: bool,
}

pub(super) struct UnlockedVaultStore {
    pub(super) store: memory_core::MemoryStore,
    pub(super) key: crate::vault_crypto::DerivedVaultKey,
}

// ─── `tachi env` handler ────────────────────────────────────────────────────

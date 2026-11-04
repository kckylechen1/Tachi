use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvPlan {
    pub cwd: String,
    pub bindings_path: Option<String>,
    pub binding_count: usize,
    pub missing_count: usize,
    pub bindings: Vec<ProjectEnvBindingStatus>,
    pub ignored_lines: Vec<crate::vault_ops::ProjectEnvIgnoredLine>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvBindingStatus {
    pub env_name: String,
    pub secret_name: String,
    pub line: usize,
    pub status: String,
    pub source: String,
}

pub(super) struct UnlockedVaultStore {
    pub(super) store: memcore::MemoryStore,
    pub(super) key: crate::vault_crypto::DerivedVaultKey,
}

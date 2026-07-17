use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_project_db_relpath() -> String {
    format!(".tachi/{}", memcore::MEMORY_DB_FILENAME)
}

// ─── Project DB ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct InitProjectDbParams {
    /// Target repository root. Required (kckylechen1/tachi#1120 PR2): this no
    /// longer defaults to the server process's own cwd when omitted — on a
    /// shared daemon that cwd belongs to the daemon, not the calling session,
    /// and silently resolving the wrong repo's project DB from it was a
    /// caller-cwd-blindness bug. A bound HTTP/stdio session can skip this
    /// tool entirely: declaring `X-Tachi-Workspace-Root` at session init
    /// auto-registers the project DB instead.
    #[serde(default)]
    pub project_root: Option<String>,
    /// Relative DB path under the repo root.
    #[serde(default = "default_project_db_relpath")]
    pub db_relpath: String,
}

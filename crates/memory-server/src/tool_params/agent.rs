use super::*;

fn default_true() -> bool {
    true
}

#[allow(dead_code)]
fn default_foundry_evidence_weight() -> f64 {
    1.0
}

#[allow(dead_code)]
fn default_evolution_memory_query_limit() -> usize {
    5
}

fn default_foundry_list_limit() -> usize {
    10
}

fn default_copilot_top_k() -> usize {
    6
}

// ─── Agent Registration ─────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AgentRegisterParams {
    /// Unique agent identifier (e.g. "claude-code", "openclaw", "cursor", "codex")
    pub agent_id: String,

    /// Human-readable display name
    #[serde(default)]
    pub display_name: Option<String>,

    /// Agent capabilities / feature flags (e.g. ["code-gen", "file-edit", "web-search"])
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Optional tool allowlist — if set, only these tools will be returned in list_tools.
    /// Supports glob patterns like "hub_*", "save_memory", "tachi_skill_*".
    #[serde(default)]
    pub tool_filter: Option<Vec<String>>,

    /// Optional per-agent rate limit override (requests per minute, 0 = use server default)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub rate_limit_rpm: Option<u64>,

    /// Optional per-agent burst limit override (0 = use server default)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub rate_limit_burst: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AgentWhoamiParams {
    /// No parameters needed — returns the current agent profile for this session.
    #[serde(default)]
    pub _placeholder: Option<String>,
}

// ─── Handoff ────────────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct HandoffLeaveParams {
    /// Summary of what was accomplished in this session
    pub summary: String,

    /// List of incomplete tasks / next steps for the receiving agent
    #[serde(default)]
    pub next_steps: Vec<String>,

    /// Optional target agent ID (e.g. "claude-code", "cursor"). If omitted, any agent can pick up.
    #[serde(default)]
    pub target_agent: Option<String>,

    /// Optional context (file paths, error messages, etc.)
    #[serde(default)]
    pub context: Option<serde_json::Value>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct HandoffCheckParams {
    /// Agent ID checking for handoff memos. If omitted, returns all pending memos.
    #[serde(default)]
    pub agent_id: Option<String>,

    /// If true, mark retrieved memos as acknowledged (default: true)
    #[serde(default = "default_true")]
    pub acknowledge: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct HandoffPromoteIssueParams {
    /// Handoff memo ID to promote (with or without "handoff:" prefix)
    pub memo_id: String,
    /// GitHub repo in "owner/repo" format
    pub repo: String,
    /// Issue title (defaults to first 120 chars of memo summary)
    #[serde(default)]
    pub title: Option<String>,
    /// Issue labels (defaults to ["handoff"])
    #[serde(default)]
    pub labels: Vec<String>,
    /// Shell flow ID for artifact linkage (writes status.json + events.jsonl)
    #[serde(default)]
    pub flow_id: Option<String>,
    /// Force re-promote even if memo already has a GitHub issue link
    #[serde(default)]
    pub force: bool,
}

// ─── Copilot / Task Guidance ────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TaskBriefParams {
    /// Natural-language task the agent is about to work on.
    pub task: String,

    /// Optional canonical agent id for sandbox filtering and context scoping.
    #[serde(default)]
    pub agent_id: Option<String>,

    /// Optional named project DB.
    #[serde(default)]
    pub project: Option<String>,

    /// Optional memory path prefix for non-wiki context search.
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional domain filter.
    #[serde(default)]
    pub domain: Option<String>,

    /// Number of wiki and memory hits to return.
    #[serde(default = "default_copilot_top_k")]
    pub top_k: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct ProgressCheckParams {
    /// Natural-language task currently being attempted.
    pub task: String,

    /// Attempts already made, in chronological order.
    #[serde(default)]
    pub attempts: Vec<String>,

    /// Latest error, symptom, or failed observation.
    #[serde(default)]
    pub latest_error: Option<String>,

    /// Optional canonical agent id for sandbox filtering and context scoping.
    #[serde(default)]
    pub agent_id: Option<String>,

    /// Optional named project DB.
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain filter.
    #[serde(default)]
    pub domain: Option<String>,

    /// Number of relevant wiki hits to return.
    #[serde(default = "default_copilot_top_k")]
    pub top_k: usize,

    /// Optional flow id for append-only progress.jsonl logging under .tachi/runs/<flow_id>.
    #[serde(default)]
    pub flow_id: Option<String>,
}

// ─── Agent Evolution ────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AgentEvolutionDocumentParams {
    /// Document kind: identity | agents | latest_truths | routing_policy | tool_policy | memory_policy | other
    pub kind: String,

    /// Optional source path for provenance / later projection
    #[serde(default)]
    pub path: Option<String>,

    /// Current document content
    pub content: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AgentEvolutionDocumentPathParams {
    /// Document kind: identity | agents | latest_truths | routing_policy | tool_policy | memory_policy | other
    pub kind: String,

    /// Filesystem path to read
    pub path: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AgentEvolutionEvidenceParams {
    /// Evidence kind: memory | reflection | tooluse | eval | ghost | session_outcome | skill_telemetry | profile_snapshot | proposal | other
    pub kind: String,

    /// Optional short evidence title
    #[serde(default)]
    pub title: Option<String>,

    /// Raw evidence text or summary
    pub content: String,

    /// Optional evidence identifier for traceability
    #[serde(default)]
    pub source_ref: Option<String>,

    /// Optional filesystem or logical path
    #[serde(default)]
    pub path: Option<String>,

    /// Relative evidence weight (default: 1.0)
    #[serde(default = "default_foundry_evidence_weight")]
    pub weight: f64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AgentEvolutionEvidencePathParams {
    /// Evidence kind: memory | reflection | tooluse | eval | ghost | session_outcome | skill_telemetry | profile_snapshot | proposal | other
    pub kind: String,

    /// Filesystem path to read
    pub path: String,

    /// Optional short evidence title
    #[serde(default)]
    pub title: Option<String>,

    /// Optional evidence identifier for traceability
    #[serde(default)]
    pub source_ref: Option<String>,

    /// Relative evidence weight (default: 1.0)
    #[serde(default = "default_foundry_evidence_weight")]
    pub weight: f64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AgentEvolutionMemoryQueryParams {
    /// Search query used to pull supporting evidence from memory
    pub query: String,

    /// Optional short evidence title
    #[serde(default)]
    pub title: Option<String>,

    /// Optional path prefix filter
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Relative evidence weight (default: 1.0)
    #[serde(default = "default_foundry_evidence_weight")]
    pub weight: f64,

    /// Max memories to bundle into one evidence record
    #[serde(default = "default_evolution_memory_query_limit")]
    pub top_k: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SynthesizeAgentEvolutionParams {
    /// Canonical target agent id
    pub agent_id: String,

    /// Optional display name for synthesis context
    #[serde(default)]
    pub display_name: Option<String>,

    /// Current profile documents that should inform the proposal
    #[serde(default)]
    pub documents: Vec<AgentEvolutionDocumentParams>,

    /// Document source paths that Tachi should load directly
    #[serde(default)]
    pub document_paths: Vec<AgentEvolutionDocumentPathParams>,

    /// Supporting evidence items
    #[serde(default)]
    pub evidence: Vec<AgentEvolutionEvidenceParams>,

    /// Evidence source paths that Tachi should load directly
    #[serde(default)]
    pub evidence_paths: Vec<AgentEvolutionEvidencePathParams>,

    /// Memory queries that should be materialized into evidence bundles
    #[serde(default)]
    pub memory_queries: Vec<AgentEvolutionMemoryQueryParams>,

    /// Optional operator goals or focus areas
    #[serde(default)]
    pub goals: Vec<String>,

    /// If true, do not call the LLM. Return the normalized job and request payload only.
    #[serde(default)]
    pub dry_run: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct ListAgentEvolutionProposalsParams {
    /// Canonical target agent id
    pub agent_id: String,

    /// Optional status filter: proposed | approved | rejected | applied
    #[serde(default)]
    pub status: Option<String>,

    /// Number of proposals to return
    #[serde(default = "default_foundry_list_limit")]
    pub limit: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct ReviewAgentEvolutionProposalParams {
    /// Derived proposal id returned by synthesize/queue/list operations
    pub proposal_id: String,

    /// Review status: approved | rejected | applied
    pub status: String,

    /// Optional reviewer note
    #[serde(default)]
    pub note: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct ProjectAgentProfileParams {
    /// Canonical target agent id
    pub agent_id: String,

    /// Current host documents that should receive approved proposals
    #[serde(default)]
    pub documents: Vec<AgentEvolutionDocumentParams>,

    /// Optional subset of persisted proposal ids to project
    #[serde(default)]
    pub proposal_ids: Vec<String>,

    /// If true, only project approved proposals (default: true)
    #[serde(default = "default_true")]
    pub approved_only: bool,

    /// If true, write projected content back to document paths on disk
    #[serde(default)]
    pub write: bool,
}

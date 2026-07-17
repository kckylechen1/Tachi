use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

// ─── Facade: shared helpers ──────────────────────────────────────────────────

fn string_enum_schema(
    values: &[&str],
    description: &str,
    _: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    rmcp::schemars::json_schema!({
        "type": "string",
        "enum": values,
        "description": description
    })
}

pub(crate) fn memory_scope_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["all", "memory", "wiki", "patterns", "sft"],
        "Recall scope: all (default), memory, wiki, patterns, or sft.",
        generator,
    )
}

pub(crate) fn tachi_memory_scope_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "all", "memory", "wiki", "patterns", "sft", "note", "user", "project", "general",
            "global",
        ],
        "Action-polymorphic tachi_memory scope: recall scopes, save note routing, and storage/routing scopes.",
        generator,
    )
}

pub(crate) fn save_kind_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["memory", "note", "wiki"],
        "What to save: memory (full entry), note (quick), or wiki (knowledge page).",
        generator,
    )
}

pub(crate) fn memory_category_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "fact",
            "decision",
            "experience",
            "preference",
            "entity",
            "other",
            "kanban",
            "handoff",
            "ghost",
            "wiki",
            "guide",
            "eval",
        ],
        "Memory category for typed recall filtering.",
        generator,
    )
}

pub(crate) fn retention_policy_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["ephemeral", "durable", "permanent", "pinned"],
        "Retention policy controlling GC behavior.",
        generator,
    )
}

fn tachi_event_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "emit",
            "query",
            "metrics",
            "project",
            "promote",
            "context",
            "a2a",
            "label_eval",
        ],
        "Required Tachi event ledger action.",
        generator,
    )
}

fn tachi_wiki_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["search", "browse", "read", "write"],
        "Required Tachi wiki facade action.",
        generator,
    )
}

fn tachi_skill_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["discover", "run", "bundle", "loadout", "from_pattern"],
        "Required Tachi skill facade action.",
        generator,
    )
}

mod memory;
pub use memory::{
    clamp_facade_top_k, TachiMemoryParams, TachiSaveParams, TachiSearchParams,
    TachiWebSearchParams, MAX_FACADE_TOP_K,
};

// ─── Facade: research verb (tachi#530) ───────────────────────────────────────

fn default_research_action() -> String {
    "feed".to_string()
}

/// Parameters for the `tachi_research` verb (tachi#530).
///
/// P1 implements **feed mode** only: `action="feed"` with a `url` → fetch the
/// page (treated as UNTRUSTED input), digest it, and emit an impact-routing
/// PROPOSAL. The verb's only writes are report artifacts in its run dir; every
/// routed finding is a proposal the leader/owner ratifies (2-gate). Flat schema
/// per #495's shape law.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiResearchParams {
    /// Research action. P1 supports only "feed" (drop a URL → fetch + digest +
    /// impact-routing proposal, no fan-out). Question mode / lifecycle edges are
    /// later phases.
    #[serde(default = "default_research_action")]
    #[schemars(
        description = "Research action. P1: 'feed' (URL in → fetch + digest + impact-routing proposal, no fan-out)."
    )]
    pub action: String,

    /// Feed-mode source URL (http/https). The fetched page is UNTRUSTED data:
    /// it is quoted into the report, never interpreted as instructions.
    #[serde(default)]
    #[schemars(
        description = "[action=feed|required] http/https source URL. Fetched page is untrusted data."
    )]
    pub url: Option<String>,

    /// Optional issue/spec reference to bias impact routing (e.g. "owner/repo#123").
    /// Format-validated at the ops boundary (`#N`, `repo#N`, `owner/repo#N`, or an
    /// http(s) URL) before the fetch runs — malformed refs are rejected, not silently
    /// coerced, since this string is rendered into the report/proposals.
    #[serde(default)]
    #[schemars(
        description = "Optional issue/spec ref to bias impact routing: '#N', 'repo#N', 'owner/repo#N', or an http(s) URL. Rejected at the ops boundary if it matches none of those shapes."
    )]
    pub issue_ref: Option<String>,

    /// Optional short note on why this source is being researched. Length-capped
    /// and control-character-stripped at the ops boundary before rendering.
    #[serde(default)]
    #[schemars(
        description = "Optional note on why this source is being researched. Capped to 500 chars and control-characters stripped before rendering."
    )]
    pub note: Option<String>,

    /// Response shape. Defaults to JSON; pass "markdown" for the human report.
    #[serde(default)]
    #[schemars(
        description = "Response shape. Defaults to JSON; 'markdown' returns the human report."
    )]
    pub format: Option<String>,
}

// ─── Facade: append-only continuity events ───────────────────────────────────

fn default_tachi_event_limit() -> usize {
    20
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiEventParams {
    #[schemars(
        schema_with = "tachi_event_action_schema",
        description = "Required. emit appends a domain-neutral continuity event; query lists recent events; metrics returns read-only continuity metrics; project materializes candidate events into stable memory projections; promote explicitly creates review artifacts for a mature pattern; context returns projected continuity memory for prompt/read-model use and records seen feedback; a2a returns the read-only evidence/open-thread bundle without feedback writes; label_eval compares session.outcome labels to session.outcome.review gold labels."
    )]
    pub action: String,
    #[serde(default, alias = "output_format")]
    #[schemars(description = "Response shape. Defaults to JSON.")]
    pub format: Option<String>,

    #[serde(default)]
    #[schemars(description = "[action=emit] Optional event id. Defaults to a generated UUID.")]
    pub id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Source repo or product surface that produced the event, e.g. sigil, quant, romanbath."
    )]
    pub source_repo: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Adapter/client that submitted the event, e.g. tachi-server, codex, openclaw."
    )]
    pub adapter: Option<String>,
    #[serde(default)]
    #[schemars(description = "Named project DB selector and event project label.")]
    pub project: Option<String>,
    /// #1114: see `SaveMemoryParams::project_explicit` — same signal, same
    /// wire key. `session_identity::enforce_session_project` stamps this on
    /// every bound-session tool call generically (not just save_memory), so
    /// `action=project`'s write-affinity scrutiny (`continuity_ops`) can
    /// distinguish a caller-supplied `project=` from a transport-injected
    /// session-binding default the same way the S1 gate already does.
    #[serde(default, rename = "__tachi_project_explicit")]
    #[schemars(skip)]
    pub project_explicit: bool,
    #[serde(default)]
    #[schemars(description = "Optional domain label, e.g. bonding, coding, project.")]
    pub domain: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional session/conversation/run id.")]
    pub session_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional actor label, e.g. user, agent, strategy, runtime.")]
    pub actor: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=emit|required, action=query|filter] Event type, e.g. pattern.observed, affect.sample, evidence.gate."
    )]
    pub event_type: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Authority level: collect_only, review_signal_only, interaction_routing_only, tone_and_reminder_only, advisory, raw_fact, derived_evidence, blocker, execution_gate."
    )]
    pub authority: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Effect scopes after projection: recall, prompt, routing, tone, scoring, execution, memory_write, project_cycle, domain_state."
    )]
    pub effects: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Projection hints and filters: pattern, outcome, affect, bonding, world_book, project_cycle, domain_profile, evidence_gate."
    )]
    pub projection_hints: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=emit|promote] Domain payload. For promote, supports force, skip_wiki_draft, skip_skill_candidate, and skip_agent_profile_proposal."
    )]
    pub payload: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(
        description = "[action=emit] Provenance/evidence JSON, e.g. files, source ids, sample_n, timestamps."
    )]
    pub provenance: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(description = "[action=emit] Event time. Defaults to now.")]
    pub created_at: Option<String>,
    #[serde(default = "default_tachi_event_limit")]
    #[schemars(
        description = "[action=query|metrics|project|context] Maximum events or projection rows to inspect/return, default 20, max 500."
    )]
    pub limit: usize,
    #[serde(default)]
    #[schemars(
        description = "[action=context] Optional projected memory path prefix. If omitted, inferred from projection_hints."
    )]
    pub path_prefix: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=project|promote] Preview writes without upserting memories or creating review artifacts."
    )]
    pub dry_run: bool,
}

/// Domain-specific adapter facade for repo-derived memory shapes.
///
/// This surface keeps fork/domain conventions out of the generic save/search
/// structs while still letting Tachi own the canonical event/projection behavior.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiDomainAdapterParams {
    #[schemars(description = "Action: lorebook_import.")]
    pub action: String,

    #[serde(default)]
    #[schemars(description = "Named project DB for imported events/projections.")]
    pub project: Option<String>,

    /// #1114 (codex round-1 B1): see `TachiEventParams::project_explicit` —
    /// same signal, same wire key. Without this field, `domain_adapter_ops`
    /// had to re-derive "was `project` a caller decision" from
    /// `project.is_some()` alone, which is exactly the F2-class bug: a
    /// transport-injected session default (stamped
    /// `__tachi_project_explicit: false` by
    /// `session_identity::enforce_session_project`, since
    /// `tachi_domain_adapter` IS in `project_defaults_to_bound_project`'s
    /// allowlist) was silently discarded on deserialization and
    /// re-inferred as `true`, bypassing the two `TachiEventParams` calls'
    /// write-affinity scrutiny entirely.
    #[serde(default, rename = "__tachi_project_explicit")]
    #[schemars(skip)]
    pub project_explicit: bool,

    #[serde(default)]
    #[schemars(description = "Domain label for imported events/projections.")]
    pub domain: Option<String>,

    #[serde(default)]
    #[schemars(description = "Actor/agent label for event imports.")]
    pub actor: Option<String>,

    #[serde(default)]
    pub session_id: Option<String>,

    #[serde(default)]
    #[schemars(description = "Character/card name for lorebook imports.")]
    pub character: Option<String>,

    #[serde(default)]
    #[schemars(description = "Lorebook entries, using RomanBath/SillyTavern field names.")]
    pub entries: Vec<serde_json::Value>,

    #[serde(default)]
    #[schemars(description = "Project lorebook events after import. Defaults false.")]
    pub project_events: bool,

    #[serde(default)]
    pub dry_run: bool,
}

// ─── Facade: unified handoff ─────────────────────────────────────────────────

/// Shared serde default for bool fields that default to true. Referenced
/// cross-module as `super::default_true` by facade/dispatch.rs and
/// facade/task.rs — do not remove while those consumers exist (#1194's
/// inert-surface sweep missed the `super::`-qualified callers once).
fn default_true() -> bool {
    true
}

/// #1099: `leave`/`check` were retired (see #1016 — sticky/orchestrator
/// replace them) along with the briefing projection and GC branch that only
/// existed to serve them. `promote_issue` is the one capability that never
/// got a replacement, so it is the sole surviving action here — this struct
/// only carries fields it needs. Existing callers that still pass
/// `action='leave'|'check'` get a loud, actionable error (never a silent
/// accept) instead of a dropped/ignored field.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiHandoffParams {
    /// Action: only "promote_issue" (create/link a GitHub issue from an
    /// existing handoff memo) is supported. "leave"/"check" were retired in
    /// #1099 — use tachi_memory(action='sticky_leave'|'sticky_check') or
    /// tachi_orchestrator(action='handoff_write'|'handoff_read') instead.
    pub action: String,

    /// Handoff memo ID to promote (required for action="promote_issue", with or without "handoff:" prefix)
    #[serde(default)]
    pub memo_id: Option<String>,

    /// GitHub repo in "owner/repo" format (required for action="promote_issue")
    #[serde(default)]
    pub repo: Option<String>,

    /// Issue title override (used by action="promote_issue"; defaults to memo summary truncated to 120 chars)
    #[serde(default)]
    pub title: Option<String>,

    /// Issue labels (used by action="promote_issue"; defaults to ["handoff"])
    #[serde(default)]
    pub labels: Vec<String>,

    /// Shell flow ID for artifact linkage (optional for action="promote_issue"; writes status.json + events.jsonl)
    #[serde(default)]
    pub flow_id: Option<String>,

    /// Force re-promote even if memo already has a GitHub issue link (used by action="promote_issue")
    #[serde(default)]
    pub force: bool,
}

mod dispatch;
pub use dispatch::{
    AdjudicationParams, CompletionPredicate, DispatchMcpAccessParams, ExecutionLevel,
    RulingRecordParams, SignatureRecordParams, TachiApproveMergeParams, TachiCompleteParams,
    TachiDispatchParams, TachiSubagentEvalParams,
};

// ─── Facade: wiki (search / browse / write) ──────────────────────────────────

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiWikiParams {
    /// Action: "search", "browse", "read", or "write"
    #[schemars(schema_with = "tachi_wiki_action_schema")]
    pub action: String,
    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default, alias = "indexed_tags")]
    #[schemars(description = "Wiki tags for recall/FTS.")]
    pub keywords: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Related repos, tools, or people.")]
    pub entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "super::coerce::opt_number_from_string_or_number_schema")]
    pub importance: Option<f64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Named project library under ~/.tachi/projects/<name>/memory.db. When set, wiki recall targets ONLY that library."
    )]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional area tag on the wiki entry.")]
    pub domain: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional JSON object merged into wiki metadata before provenance.")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub force: bool,

    /// External references (URLs, absolute paths, GitHub shorthands). Validated on write.
    #[serde(default)]
    pub references: Vec<String>,

    /// Recall projected continuity patterns and persist references in wiki metadata.
    #[serde(default)]
    pub include_patterns: bool,

    /// Optional pattern search/filter text. Defaults to title + summary when include_patterns=true.
    #[serde(default)]
    pub pattern_query: Option<String>,

    /// Maximum pattern references to attach.
    #[serde(default)]
    pub pattern_top_k: Option<usize>,
}

// ─── Facade: component governance read model (Issue #796) ────────────────────

fn tachi_component_action_schema(gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
    string_enum_schema(
        &["list", "show", "check", "plan"],
        "Component governance action.",
        gen,
    )
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiComponentParams {
    /// Action: "list" (compact records), "show" (full record + relation edges), "check" (read-only downstream classifier, Issue #797), or "plan" (read-only cutover checklist, Issue #798).
    #[schemars(schema_with = "tachi_component_action_schema")]
    pub action: String,
    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
    #[serde(default)]
    pub format: Option<String>,
    /// Required for action="show" and action="plan" (--from): the component_id (e.g. "tachi-memory-kernel").
    #[serde(default)]
    pub component_id: Option<String>,
    /// Optional filter for action="list": one of kernel, runtime_adapter, workflow_bridge, frontend_app_shell.
    #[serde(default)]
    pub component_type: Option<String>,
    /// Include archived/stale records in list/show output. Defaults to false.
    #[serde(default)]
    pub include_archived: Option<bool>,
    /// Cap on the number of records returned by action="list".
    #[serde(default)]
    pub limit: Option<usize>,
    /// Named project library under ~/.tachi/projects/<name>/memory.db. Component records are global;
    /// this only scopes read-forward behavior, mirroring other read-only facade tools.
    #[serde(default)]
    pub project: Option<String>,
    /// action="check": filesystem path to classify. action="plan" (--to): target checkout path, consumer component_id, or owner_repo.
    #[serde(default)]
    #[schemars(
        description = "For action='check': repo checkout path. For action='plan': --to target (path, component_id, or owner_repo)."
    )]
    pub repo: Option<String>,
}

// ─── Facade: workflow closure (Issue → Doc → Memory) ─────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiWorkflowParams {
    /// close_loop | build_references
    pub action: String,
    #[serde(default)]
    pub issue_ref: Option<String>,
    /// Optional PR reference ("owner/repo#123" or URL); when set, close_loop also
    /// posts the write-back comment to the PR.
    #[serde(default)]
    pub pr_ref: Option<String>,
    #[serde(default)]
    pub doc_paths: Vec<String>,
    /// Spec/contract file paths this change touched. Used for the close_loop
    /// spec-drift advisory (specs are the source of truth and must stay current).
    #[serde(default)]
    pub spec_paths: Vec<String>,
    #[serde(default)]
    pub related_issues: Vec<String>,
    /// Whether close_loop posts the write-back comment to the issue/PR.
    /// Defaults to true (best-effort; never fails the closure if GitHub is down).
    #[serde(default)]
    pub post_comment: Option<bool>,
    /// Optional Tachi flow id. When set and wiki_title/wiki_text are omitted,
    /// close_loop drafts them from the flow's result.md (lowers the activation
    /// energy to actually close the loop).
    #[serde(default)]
    pub flow_id: Option<String>,
    /// Optional free-form notes. When wiki_title/wiki_text are omitted and
    /// result.md is unavailable, close_loop drafts the wiki body from notes
    /// (#925). Prefer explicit wiki_* fields for durable lessons.
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub wiki_title: Option<String>,
    #[serde(default)]
    pub wiki_text: Option<String>,
    #[serde(default)]
    pub wiki_path: Option<String>,
    #[serde(default)]
    pub wiki_topic: Option<String>,
    #[serde(default)]
    pub wiki_summary: Option<String>,
    #[serde(default)]
    pub wiki_category: Option<String>,
    #[serde(default)]
    pub wiki_keywords: Vec<String>,
    #[serde(default)]
    pub wiki_entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "super::coerce::opt_number_from_string_or_number_schema")]
    pub wiki_importance: Option<f64>,
    #[serde(default)]
    pub wiki_scope: Option<String>,
    #[serde(default)]
    pub wiki_domain: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub force: bool,
}

// ─── Facade: skill (discover / run / bundle / loadout) ───────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiSkillParams {
    /// Action: "discover", "run", "bundle", "loadout", or "from_pattern"
    #[schemars(schema_with = "tachi_skill_action_schema")]
    pub action: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub cap_type: Option<String>,
    #[serde(default)]
    pub enabled_only: Option<bool>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub skill_id: Option<String>,
    #[serde(default)]
    pub args: Option<serde_json::Value>,
    /// DispatchProfile name for action="loadout", e.g. "claude_plan".
    #[serde(default)]
    pub profile: Option<String>,
    /// Optional host/runtime name for bundle preparation, e.g. "codex".
    #[serde(default)]
    pub host: Option<String>,
    /// Max skill recommendations in a capability bundle.
    #[serde(default)]
    pub skill_limit: Option<usize>,
    /// Max supporting capabilities in a capability bundle.
    #[serde(default)]
    pub capability_limit: Option<usize>,
    /// Include a ready-to-inject markdown section in bundle responses.
    #[serde(default)]
    pub include_section: Option<bool>,
}

mod task;
pub use task::TachiTaskParams;

mod action_enums;
mod action_inventory;
pub use action_enums::{TachiTaskAction, TachiVerifyAction};
pub use action_inventory::{
    TACHI_GH_ACTIONS, TACHI_GH_ACTION_SOFT_MAX, TACHI_MEMORY_ACTIONS, TACHI_MEMORY_ACTION_SOFT_MAX,
    TACHI_TASK_PRIMARY_ACTION_SOFT_MAX, TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS,
};

mod orchestration;
pub use orchestration::{
    MirrorEvalAdjudicateParams, MirrorEvalGetParams, MirrorEvalObserveParams,
    MirrorEvalRegisterParams, TachiAgentEvalParams, TachiAgentsParams, TachiArenaParams,
    TachiBoardParams, TachiOrchestratorParams, TachiShellDispatchSliceParams, TachiShellParams,
    TachiVerifyCheckItem, TachiVerifyParams,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_top_k_is_capped() {
        assert_eq!(clamp_facade_top_k(0), 1);
        assert_eq!(clamp_facade_top_k(6), 6);
        assert_eq!(clamp_facade_top_k(10_000), MAX_FACADE_TOP_K);
    }
}

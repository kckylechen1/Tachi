# Delegated Task

We need to refine the Tachi MCP standard tool surface. Implement these changes in Rust. Work in /Users/kckylechen/Desktop/Sigil.

High-level goals:
1. Add domain facade `tachi_wiki` for wiki search/browse/write.
2. Add domain facade `tachi_skill` for hub_discover/run_skill.
3. Add domain facade `tachi_task` for plan/dispatch/board/merge. Do NOT include tachi_complete.
4. Enhance `tachi_save` with `kind="extract_facts"` / `kind="facts"` dispatching to existing `handle_extract_facts`.
5. Remove from standard profile: `tachi_complete`, `recall_context`, `vault_unlock`, `vault_lock`, `tachi_browse`, `hub_discover`, `run_skill`, `tachi_dispatch`, `approve_merge`, `tachi_board`, `tachi_plan` where replaced by facades. Keep `vault_status` optional in standard. Keep `tachi_gh` conditional.
6. Fix GH call_tool visibility bug: `server_handler.rs::call_tool` must allow GH tools if GH_TOKEN exists, mirroring list_tools, not use raw `tool_visible` only.

Implementation details:

A) `/crates/memory-server/src/tool_params/facade.rs`
Add these structs:

```rust
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiWikiParams {
    /// Action: "search", "browse", or "write"
    pub action: String,
    #[serde(default)] pub query: Option<String>,
    #[serde(default)] pub category: Option<String>,
    #[serde(default)] pub top_k: Option<usize>,
    #[serde(default)] pub limit: Option<usize>,
    #[serde(default)] pub title: Option<String>,
    #[serde(default)] pub text: Option<String>,
    #[serde(default)] pub path: Option<String>,
    #[serde(default)] pub topic: Option<String>,
    #[serde(default)] pub summary: Option<String>,
    #[serde(default)] pub keywords: Vec<String>,
    #[serde(default)] pub entities: Vec<String>,
    #[serde(default)] pub importance: Option<f64>,
    #[serde(default)] pub scope: Option<String>,
    #[serde(default)] pub project: Option<String>,
    #[serde(default)] pub domain: Option<String>,
    #[serde(default)] pub force: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiSkillParams {
    /// Action: "discover" or "run"
    pub action: String,
    #[serde(default)] pub query: Option<String>,
    #[serde(default)] pub cap_type: Option<String>,
    #[serde(default)] pub enabled_only: Option<bool>,
    #[serde(default)] pub skill_id: Option<String>,
    #[serde(default)] pub args: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiTaskParams {
    /// Action: "plan", "dispatch", "board", or "merge"
    pub action: String,
    // plan fields
    #[serde(default)] pub task: Option<String>,
    #[serde(default)] pub agent_id: Option<String>,
    #[serde(default)] pub domain: Option<String>,
    #[serde(default)] pub path_prefix: Option<String>,
    #[serde(default)] pub top_k: Option<usize>,
    // dispatch fields
    #[serde(default)] pub agent: Option<String>,
    #[serde(default)] pub cwd: Option<String>,
    #[serde(default)] pub skills: Vec<String>,
    #[serde(default)] pub context_query: Option<String>,
    #[serde(default)] pub model: Option<String>,
    #[serde(default)] pub timeout_secs: Option<u64>,
    #[serde(default)] pub permission_profile: Option<String>,
    #[serde(default)] pub allowed_tools: Vec<String>,
    #[serde(default)] pub max_turns: Option<u32>,
    #[serde(default)] pub sandbox: Option<String>,
    #[serde(default)] pub inject_tachi_mcp: Option<bool>,
    #[serde(default)] pub inject_hub_mcps: Option<bool>,
    #[serde(default)] pub command: Vec<String>,
    #[serde(default)] pub project: Option<String>,
    #[serde(default)] pub stage: Option<String>,
    // board fields
    #[serde(default)] pub state_filter: Option<String>,
    #[serde(default)] pub limit: Option<usize>,
    // merge fields
    #[serde(default)] pub worktree: Option<String>,
    #[serde(default)] pub branch: Option<String>,
    #[serde(default)] pub strategy: Option<String>,
    #[serde(default = "default_true")] pub delete_worktree: bool,
    #[serde(default)] pub confirm: bool,
}
```

B) `/crates/memory-server/src/tools.rs`
Add imports as needed (TachiWikiParams/TachiSkillParams/TachiTaskParams and handler params already in scope likely). Add new tool methods near facade section:

`tachi_wiki`: match action:
- search: require query, call `handle_tachi_wiki_search(self, WikiSearchParams { query, path_prefix: None, category, top_k: top_k.unwrap_or(10), include_archived: false, agent_role: None, project, domain, weights: None })`
- browse: call `handle_wiki_browse(self, WikiBrowseParams { category, limit: limit.unwrap_or(50), project: project.unwrap_or_else(|| "wiki".to_string()) })` (check actual WikiBrowseParams fields first!)
- write: require title + text, call `handle_tachi_wiki_write` with existing params. Use category "experience", retention "permanent", importance default 0.85.

`tachi_skill`: match action:
- discover: call `handle_hub_discover(self, HubDiscoverParams { query, cap_type, enabled_only: enabled_only.unwrap_or(true) })` (check actual fields first)
- run: require skill_id, call existing `handle_run_skill(self, RunSkillParams { skill_id, args })` (check actual fields first)

`tachi_task`: match action:
- plan: require task, call `handle_tachi_task_brief` with TaskBriefParams.
- dispatch: require agent + task, call `handle_tachi_dispatch` with TachiDispatchParams, using defaults (timeout_secs unwrap_or 600).
- board: call `handle_tachi_board` with TachiBoardParams.
- merge: require worktree, call `handle_approve_merge` with TachiApproveMergeParams.

Enhance existing `tachi_save` match: treat `kind` "facts" or "extract_facts" as call to `handle_extract_facts(self, ExtractFactsParams { text: params.text.clone(), source: params.source? })`. TachiSaveParams currently lacks `source`; add it to TachiSaveParams in facade.rs as optional source string and default "tachi_save".

C) `/crates/memory-server/src/profiles.rs`
- Add `tachi_wiki` to OBSERVE/REMEMBER? It has write capability, so put in REMEMBER bundle, not observe. But if it includes search/browse and write, remember bundle is okay for standard.
- Add `tachi_skill` to REMEMBER or OBSERVE? Since it can run skills, put in REMEMBER (existing run_skill is remember).
- Add `tachi_task` to COORDINATE.
- Standard allowlist should become roughly:
  `tachi_task`, `tachi_search`, `tachi_web_search`, `tachi_wiki`, `tachi_save`, `tachi_handoff`, `tachi_skill`, `vault_status`.
  GH remains conditional via GH_TOOL_PATTERNS.
  Remove: `tachi_plan`, `recall_context`, `tachi_browse`, `hub_discover`, `run_skill`, `tachi_dispatch`, `approve_merge`, `tachi_board`, `tachi_complete`, `vault_unlock`, `vault_lock`.
- Delegate can keep `run_skill` and `tachi_complete` for now unless tests require; do not touch delegate unless needed.
- Update tests accordingly.

D) `/crates/memory-server/src/server_handler.rs`
Fix call_tool visibility. Current call_tool uses `tool_visible`. Add check:
```
let has_gh_token = crate::vault_ops::vault_has_secret(self, "GH_TOKEN");
let visible = if crate::profiles::is_gh_tool_name(name) { ... } else { tool_visible(...) };
```
Expose a pub(super) helper in profiles.rs if needed: `pub(super) fn gh_tool_visible(tool_name: &str, profile: Option<ToolProfile>, has_gh_token: bool) -> bool` returning same as filter logic.

Important: compile and run `cargo test -p memory-server`. Keep code minimal, no extra comments/docs beyond what is already necessary for schema descriptions.

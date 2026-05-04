# Completion Report: Tachi MCP Tool Facade Refinement

## Summary
All 6 goals implemented and verified. 3 new domain facades added (tachi_wiki, tachi_skill, tachi_task), tachi_save enhanced with extract_facts dispatch, standard profile allowlist updated to use new facades (removing 11 old tools), and GH call_tool visibility bug fixed. All 260 tests pass.

## Files Changed

1. **`crates/memory-server/src/tool_params/facade.rs`**
   - Added `source: Option<String>` field to `TachiSaveParams`
   - Added `TachiWikiParams` struct (action=search/browse/write)
   - Added `TachiSkillParams` struct (action=discover/run)
   - Added `TachiTaskParams` struct (action=plan/dispatch/board/merge)

2. **`crates/memory-server/src/tools.rs`**
   - Enhanced `tachi_save` to dispatch `kind="facts"`/`kind="extract_facts"` → `handle_extract_facts`
   - Added `tachi_wiki` facade tool method (search → wiki search, browse → wiki browse, write → wiki write)
   - Added `tachi_skill` facade tool method (discover → hub_discover, run → run_skill)
   - Added `tachi_task` facade tool method (plan → task_brief, dispatch → tachi_dispatch, board → tachi_board, merge → approve_merge)

3. **`crates/memory-server/src/profiles.rs`**
   - Added `tachi_wiki`, `tachi_skill`, `tachi_task` to OBSERVE bundle
   - Added `tachi_wiki`, `tachi_skill` to REMEMBER bundle
   - Added `tachi_task` to COORDINATE bundle
   - Updated STANDARD_MINIMAL: now includes `tachi_task`, `tachi_search`, `tachi_web_search`, `tachi_wiki`, `tachi_save`, `tachi_handoff`, `tachi_skill`, `vault_status` (removed: tachi_plan, recall_context, tachi_browse, hub_discover, run_skill, tachi_dispatch, tachi_complete, approve_merge, tachi_board, vault_unlock, vault_lock)
   - Added `is_gh_tool_name()` and `gh_tool_visible()` helpers
   - Updated all profile tests for new standard allowlist

4. **`crates/memory-server/src/server_handler.rs`**
   - Fixed call_tool GH visibility: now checks `is_gh_tool_name` + `gh_tool_visible` (mirrors list_tools logic), resolving the bug where GH tools were inaccessible in call_tool even with GH_TOKEN present

5. **`crates/memory-server/src/main.rs`**
   - Added `tachi_task`, `tachi_wiki`, `tachi_skill` to CACHEABLE_TOOLS
   - Added `tachi_task`, `tachi_wiki`, `tachi_skill` to CACHE_INVALIDATING_TOOLS

6. **`crates/memory-server/src/daily_pipeline.rs`** — Added `source: None` to TachiSaveParams usage
7. **`crates/memory-server/src/tests.rs`** — Added `source: None` to 3 TachiSaveParams usages

## Commands Run
- `cargo test -p memory-server` — all 260 tests pass
- `cargo test -p memory-server -- profiles` — all 15 profile tests pass

## Verification Performed
- Full compilation with no warnings related to new code
- All 260 existing tests pass (including 15 profile bundle/allowlist tests)
- Profile test `every_standard_and_delegate_allow_list_entry_exists_in_tool_router` confirms all new facades are routed
- Profile test `every_real_tool_is_either_bundled_or_explicitly_admin_only` confirms new tools are properly bundled

## Remaining Risks or Blockers
- None. All changes are backward compatible — old individual tools (tachi_plan, hub_discover, run_skill, etc.) still exist and are functional for non-standard profiles (admin, delegate, etc.)
- Delegate profile was intentionally left unchanged per task instructions

# Execution Plan: Tachi MCP Tool Facade Refinement

## Overview
Add 3 domain facades (wiki, skill, task), enhance tachi_save with extract_facts dispatch, update standard profile allowlist, and fix GH call_tool visibility bug.

## Step-by-step

### 1. facade.rs — Add param structs
- Add `TachiWikiParams`, `TachiSkillParams`, `TachiTaskParams`
- Add `source: Option<String>` to existing `TachiSaveParams`

### 2. tools.rs — Add 3 facade tool methods + enhance tachi_save
- `tachi_wiki`: dispatches to wiki search/browse/write based on `action`
- `tachi_skill`: dispatches to hub_discover/run_skill based on `action`
- `tachi_task`: dispatches to plan/dispatch/board/merge based on `action` (no complete)
- Enhance `tachi_save`: match kind="facts"|"extract_facts" → handle_extract_facts

### 3. profiles.rs — Update bundles and allowlists
- Add `tachi_wiki` to OBSERVE + REMEMBER bundles
- Add `tachi_skill` to OBSERVE + REMEMBER bundles  
- Add `tachi_task` to OBSERVE + COORDINATE bundles
- Update STANDARD_MINIMAL: replace old tools with facades, remove tachi_complete, recall_context, vault_unlock, vault_lock, tachi_browse, hub_discover, run_skill, tachi_dispatch, approve_merge, tachi_board, tachi_plan
- New standard: tachi_task, tachi_search, tachi_web_search, tachi_wiki, tachi_save, tachi_handoff, tachi_skill, vault_status
- Add `is_gh_tool_name()` helper for server_handler.rs
- Update test assertions

### 4. main.rs — Cache lists
- Add new facades to CACHEABLE_TOOLS (read actions) and CACHE_INVALIDATING_TOOLS (write actions)
- Add tachi_wiki, tachi_skill, tachi_task appropriately

### 5. server_handler.rs — Fix GH call_tool visibility
- In `call_tool`, check GH_TOKEN before rejecting GH tools (mirror list_tools logic)

### 6. Compile & test
- `cargo test -p memory-server`

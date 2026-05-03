# Execution Plan: Tachi P0 Notes + Dispatch V2 MVP

## Summary of Changes

### 1. Notes Layer (files + tachi_save scope=note)
- **`dispatch_ops.rs`**: Add `notes_root()`, `ensure_notes_dirs()`, `resolve_note_path()`, `write_note_file()` helpers
- **`tools.rs`** (tachi_save): Modify note branch to call filesystem writer before DB index
- Trigger: `kind="note"` OR `scope="note"` (even if kind empty)
- Directories: inbox/, brainstorm/, dispatch/, handoff/, reflections/, proposals/
- Path rules: relative `.md` → write directly; no `.md` → `<dir>/<ts>-<slug>.md`; block absolute escape
- Frontmatter: title/created_at/topic/category/keywords/source
- DB index via `handle_remember` with path `/notes/<relative>`, category `note`, retention `durable`
- Return `note_file` in JSON response

### 2. TachiDispatchParams.stage
- **`tool_params/facade.rs`**: Add `stage: Option<String>` field
- **`dispatch_ops.rs`**: Apply stage-based default skill injection before assemble_prompt
  - `plan` → inject `skill:superpowers-writing-plans`
  - `execute` → inject `skill:superpowers-executing-plans`
  - `auto` → inject `skill:superpowers-writing-plans` + add "plan first, wait for review" instruction

### 3. assemble_prompt_v2 (enhance assemble_prompt in dispatch_ops.rs)
- Default context_query = task itself if none provided, top_k=5
- Title context as `## Relevant context from Tachi memory/wiki`
- Inject stage default skills
- Avoidance: search task + "failure OR partial OR watchdog" over `/eval`, top_k=3, section `## Prior pitfalls / avoidance notes` (silently skip on failure)
- Add `## Operating instructions` section

### 4. trajectory.jsonl audit files (in dispatch_ops.rs handle_tachi_dispatch)
- Write `prompt.md` (full assembled prompt)
- Write `context.md` (context/skills/avoidance summary)
- Write `trajectory.jsonl` with `dispatch_started` and after subprocess: `subprocess_finished`

### 5. post_complete_hooks MVP (in complete_ops.rs)
- On failure/partial + non-empty notes: save lesson to `/eval/lessons/<date>/<task_id>`, category `lesson`, importance 0.75
- On success + trajectory: existing distill logic (keep)
- Add `post_complete_hooks` to pipeline status in review_bundle

## Files to modify
1. `crates/memory-server/src/tool_params/facade.rs` — add `stage` to TachiDispatchParams
2. `crates/memory-server/src/dispatch_ops.rs` — notes helpers, assemble_prompt_v2, trajectory files, stage logic
3. `crates/memory-server/src/complete_ops.rs` — post_complete_hooks
4. `crates/memory-server/src/tools.rs` — tachi_save note branch with filesystem write

## Verification
- `cargo test -p memory-server`

# Completion Report: Tachi P0 Notes + Dispatch V2 MVP

## Summary
Implemented all 5 required features for Notes layer + Dispatch V2 MVP:

1. **Notes filesystem layer** — `tachi_save` with `kind="note"` or `scope="note"` writes human-readable markdown with frontmatter to `~/.tachi/notes/`, indexes in DB, returns `note_file`/`note_path` in JSON
2. **Dispatch stage field** — `TachiDispatchParams.stage` (plan/execute/auto/empty) injects default skills when caller didn't provide explicit skills
3. **assemble_prompt v2** — default context_query from task, avoidance search over `/eval`, operating instructions section, stage skill injection
4. **trajectory.jsonl audit** — writes `prompt.md`, `context.md`, and `trajectory.jsonl` with `dispatch_started` + `subprocess_finished` events
5. **post_complete_hooks MVP** — auto-saves lesson entries on failure/partial outcomes to `/eval/lessons/<date>/<task_id>`, reports hook status in pipeline

## Files Changed

| File | Change |
|------|--------|
| `crates/memory-server/src/tool_params/facade.rs` | Added `stage: Option<String>` to `TachiDispatchParams` |
| `crates/memory-server/src/dispatch_ops.rs` | Added notes helpers (notes_root, ensure_notes_dirs, resolve_note_path, build_note_markdown, write_note_file), rewrote assemble_prompt with v2 features (default context query, avoidance injection, operating instructions, stage skill resolution), added trajectory audit file writes, added subprocess_finished event to spawn |
| `crates/memory-server/src/complete_ops.rs` | Added `post_complete_hooks` logic (lesson auto-save on failure/partial), added `post_complete_hooks` to pipeline status |
| `crates/memory-server/src/tools.rs` | Modified `tachi_save` note branch: detect `scope="note"`, write to filesystem via `write_note_file`, DB index with `/notes/<rel>` path, append `note_file`/`note_path` to response |

## Commands Run
- `cargo build -p memory-server` — compiled successfully
- `cargo test -p memory-server` — **255 passed, 0 failed**

## Verification
- All 255 existing tests pass without modification
- Build compiles cleanly with no warnings related to changes
- Notes path security: absolute paths and `..` traversal are rejected
- Filesystem write failures are non-fatal (log warning, continue with DB-only)

## Remaining Risks / Notes
- **Notes filesystem**: `notes_root()` creates directories lazily on first write. If the filesystem is read-only, notes degrade gracefully to DB-only.
- **Stage skills**: `skill:superpowers-writing-plans` and `skill:superpowers-executing-plans` must be registered in the Hub for stage injection to produce skill content. If not registered, the section is silently empty.
- **Avoidance search**: queries `/eval` path prefix with task + "failure OR partial OR watchdog" — silently skipped on search failure.
- **Trajectory.jsonl**: appended in the spawned async task, so a process crash before spawn completion could lose the `subprocess_finished` event. The `dispatch_started` event is written synchronously before spawn.
- **Workspace cleanup**: existing behavior preserved — workspace deleted on success, kept on failure. Trajectory files persist in workspace until cleanup.

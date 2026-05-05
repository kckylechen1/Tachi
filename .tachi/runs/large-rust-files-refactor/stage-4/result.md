# Stage 4 — extract facade tool bodies into ops modules

## Baseline
- Branch: `refactor/tools-stage-4` (from `refactor/large-rust-files` @ 5d150be)
- Baseline `cargo test -p memory-server`: **270 passed; 0 failed**
- `tools.rs` before: **2087 lines**

## Facades audited

`tools.rs` defines the `#[tool]` wrappers under `impl MemoryServer` (rmcp constraint:
all tool methods must live in the same impl block). The wrappers fall into two camps:

### Already-thin wrappers — left untouched
These delegate to existing `*_ops` modules in one line; no work needed:

- `tachi_handoff` → `handle_handoff_leave` / `handle_handoff_check` (small action router; left alone — it's just two arms)
- `tachi_plan`, `tachi_unstick`, `tachi_browse` → `copilot_ops` / `wiki_ops`
- `tachi_dispatch`, `tachi_board`, `approve_merge` → `dispatch_ops`
- `tachi_complete` → `complete_ops`
- `tachi_wiki` → action router (search/browse/write); already calls existing `wiki_ops` / `copilot_ops` handlers — small enough to leave inline
- `tachi_skill`, `tachi_task` → action routers; the bodies are pure parameter-shuffling and call existing `*_ops` modules. Considered for extraction but they only re-build params; no business logic. Left alone (matches the "keep mechanical wrappers in tools.rs" pattern).
- `tachi_gh`, `tachi_shell` → already 1-line delegations.

### Heavy bodies extracted
The four facades named in the plan all had substantial inline logic and were
extracted verbatim into sibling `*_ops.rs` modules:

| Facade            | Lines extracted | New module                                |
|-------------------|-----------------|-------------------------------------------|
| `tachi_save`      | ~165            | `crates/memory-server/src/facade_save_ops.rs` (181 L) |
| `tachi_search`    | ~70             | `crates/memory-server/src/facade_search_ops.rs` (82 L) |
| `tachi_memory`    | ~88             | `crates/memory-server/src/facade_memory_ops.rs` (104 L) |
| `tachi_web_search`| ~95 + 5 helper fns (`first_text_blocks`, `matches_web_search_backend`, `discovered_tool_schema`, `choose_web_search_tool`, `web_search_arguments`) | `crates/memory-server/src/web_search_ops.rs` (252 L) |

Each `#[tool]` wrapper now reads:

```rust
pub(crate) async fn tachi_save(
    &self,
    Parameters(params): Parameters<TachiSaveParams>,
) -> Result<String, String> {
    crate::facade_save_ops::handle_tachi_save(self, params).await
}
```

Notes:
- `tachi_memory` previously called `self.tachi_save(...)` and `self.tachi_search(...)`. The
  extracted `handle_tachi_memory` calls the new free `handle_tachi_save` /
  `handle_tachi_search` directly, eliminating an unnecessary `Parameters(...)` wrap.
  Behavior is unchanged because the wrappers themselves are now zero-logic delegations
  to the same handlers.
- The 5 helper fns used only by `tachi_web_search` (`first_text_blocks`,
  `matches_web_search_backend`, `discovered_tool_schema`, `choose_web_search_tool`,
  `web_search_arguments`) moved with it into `web_search_ops.rs` as private fns.
- `crate::hub_helpers::capability_callable` import moved to `web_search_ops.rs`
  (no longer used by `tools.rs`).

## tools.rs size

| Before | After | Δ        |
|--------|-------|----------|
| 2087 L | 1536 L | **−551 L (−26 %)** |

## main.rs additions

```rust
mod facade_memory_ops;
mod facade_save_ops;
mod facade_search_ops;
// ...
mod web_search_ops;
```

## MCP tool surface

Diff of `#[tool(...)]` lines before vs after — **empty** (zero changes).

```
$ diff /tmp/tools_before.txt /tmp/tools_after.txt
(no output)
```

All `#[tool]` wrappers remain in the single `impl MemoryServer` block under
`#[tool_router(vis = "pub(crate)")]` — required by rmcp.

## Tests

`cargo test -p memory-server` after: **270 passed; 0 failed; 0 ignored**
(matches baseline).

## Commit

`refactor(memory-server): extract facade tool bodies into ops modules`
SHA: see `git log -1 --format=%H` on `refactor/tools-stage-4`.

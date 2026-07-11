# Briefing hot-path — evidence

Branch: `feat/perf-briefing-hotpath`
Scope: `crates/tachi-server/**` only (per packet).

## Item 1 — parallelize `feature_briefing`'s sequential awaits (DONE)

File: `crates/tachi-server/src/copilot_ops/feature_briefing/handlers.rs`
Function: `handle_tachi_feature_briefing`

### Before (sequential, 4 round trips back to back)

```rust
let board = feature_board(server, params, top_k).await;
...
let wiki_rows = search_memory_rows(server, ...).await.unwrap_or_default();
let memory_rows = search_memory_rows(server, ...).await.unwrap_or_default();
let eval_rows = search_memory_rows(server, ...).await.unwrap_or_default();
```

Four independent, non-dependent async calls (`feature_board`, and three
`search_memory_rows` calls against `/wiki`, the caller's `path_prefix`, and
`/eval`) were each fully awaited one after another. None of the four reads
the output of another — this was pure serialization of independent I/O.

### After (concurrent, mirrors the existing `tachi_memory` briefing pattern)

```rust
let (board, wiki_rows, memory_rows, eval_rows) = tokio::join!(
    feature_board(server, params, top_k),
    search_memory_rows(server, SearchMemoryParams { path_prefix: Some("/wiki".into()), .. }, false),
    search_memory_rows(server, SearchMemoryParams { path_prefix: params.path_prefix.clone(), .. }, !params.include_global),
    search_memory_rows(server, SearchMemoryParams { path_prefix: Some("/eval".into()), .. }, !params.include_global),
);
let wiki_rows = wiki_rows.unwrap_or_default();
let memory_rows = memory_rows.unwrap_or_default();
let eval_rows = eval_rows.unwrap_or_default();
```

This mirrors `facade_memory_ops/briefing_ops.rs:188`'s existing
`tokio::join!(handle_search_memory(...), async { ... }, async { ... })`
pattern in the sibling `tachi_memory` briefing path — same shape, same crate,
same `MemoryServer` reference semantics (all callees take `&MemoryServer`,
`Send`-safe to join concurrently).

Wall-clock effect: 4 sequential round trips collapse to 1 round trip whose
duration is the max of the four, not the sum — this is a latency win
regardless of DB size (the task's stated invariant), since it removes
scheduling/await overhead stacking rather than depending on data volume.
The in-memory SQLite test fixtures are too fast (microsecond scale) to show
a measurable wall-clock delta in `cargo test`, so the evidence here is the
structural diff plus parity with the sibling path that already ships this
pattern in production.

Compiles clean; no behavior change (same four values produced, same
`unwrap_or_default()` fallback semantics preserved — previously the
`.await` was followed immediately by `.unwrap_or_default()` inline; now the
`tokio::join!` returns the four `Result`s and `.unwrap_or_default()` is
applied right after, identically).

## Item 2 — serde_json::to_string cache-write guard (SKIPPED — not applicable)

Packet target: "(memory) facade handlers.rs:~77" move serialize inside
`if let Some(key)` cache-write block.

Investigated `crates/tachi-server/src/memory_search_ops/search_memory/handlers.rs`
(100 lines total; the `serde_json::to_string` call is at line 76-77, inside
`handle_search_memory_with_access` — this is the only `handlers.rs` in the
crate with a serialize call positioned before an `if let Some(key)`
cache-write block, so it is the intended target).

Verified the `serialized` binding produced at line 76-77 is used in **two**
places:
- line 91: `recall_cache_store(&key, ..., &serialized, ...)` (cache write)
- line 99: `Ok(serialized)` (the function's return value — required on
  every call, cache on or off)

Every caller (`tools/memory_facade.rs`, `facade_search_ops.rs`,
`facade_memory_ops/briefing_ops.rs`, `bootstrap/poke_cli/probes/memory.rs`,
`bootstrap/cli_tool.rs`) consumes the returned `String` directly — it is not
dead weight when the cache is off. Moving the `serde_json::to_string` call
inside `if let Some(key) = cache_key { ... }` would either:
- leave `Ok(serialized)` referencing a variable not defined outside that
  block (compile error), or
- force a second, duplicate serialization to reconstruct the return value
  when `cache_key` is `None` (strictly worse than today: today it serializes
  **once** and reuses the string for both the cache write and the return).

Confirmed via `grep` there is no second/duplicate serialize call anywhere in
this function or in `recall_cache_store` (which takes `&str` and does not
re-serialize). The current code already does the minimal single
serialization; there is nothing to save on the cache-off path without
changing the function's return type (a signature change, out of the
packet's declared scope, and risky — `parse_evidence_array` at
`facade_memory_ops/evidence_format.rs:564` round-trips this same string back
into a `Value` at one call site, which is a separate, real, but
out-of-scope optimization opportunity).

Per the packet's explicit instruction to not broaden scope and to skip
items whose stated precondition isn't present, this edit was **not
applied** — applying it as literally described would either fail to compile
or regress correctness/perf. No other `handlers.rs` in the crate matches the
"~77, cache-write block" description any better than this one, and this one
does not exhibit the described defect in the current codebase.

## Item 3 — git-remote OnceLock (SKIPPED — not present)

Packet: "if `feature_briefing` resolves the git remote per-call, cache it in
a `OnceLock` ... skip if not found."

Searched `crates/tachi-server/src/copilot_ops/feature_briefing/**` and the
full crate for any per-call git-remote resolution reachable from
`handle_tachi_feature_briefing`'s call path
(`feature_board`, `project_work_records`, `canonical_doc_refs`,
`feature_briefing_query`, `feature_run_artifacts`, `scan_open_loops`, etc.):

- No `Command::new("git")` invocation exists anywhere under
  `copilot_ops/feature_briefing/`.
- The one repo-root git shell-out in the general vicinity,
  `shell_ops/flow.rs:cached_git_root()`, **already uses `OnceLock`**
  (`static GIT_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();`) — so if
  this is the code opus meant, it is already fixed upstream of this task.
- Other `git remote get-url origin` call sites in the crate
  (`component_governance_ops/mod.rs`, `gh_ops/ship.rs`) are unrelated
  modules not reachable from the briefing hot path.

Per the packet's own "skip if not found" clause, this item was not applied.

## Verification

```
cargo check -p tachi-server            # clean
cargo test -p tachi-server briefing    # 28 passed, 0 failed
cargo test -p tachi-server feature_briefing  # 3 passed, 0 failed (subset of above)
cargo fmt --check -p tachi-server      # clean, no diff
cargo clippy -p tachi-server --all-targets --no-deps -- -D warnings  # clean
                                        # (pre-existing "multiple build targets"
                                        #  Cargo.toml warning reproduced on main
                                        #  too — not clippy, not introduced here)
```

HEAD SHA: see `git rev-parse HEAD` after commit/push below.

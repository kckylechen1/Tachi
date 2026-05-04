# Completion Report

## Summary
Fixed both bugs:
1. **Bug 1 (scope fallback)**: `tachi_search` now normalizes unrecognized scope values (e.g. "project", "global", "user") to "all", and prepends a helpful note when this fallback is applied.
2. **Bug 2 (CLI --project)**: `tachi search` now accepts `--project <name>` and routes through `dispatch_cli_tool` (same pattern as `WikiSearch`/`Remember`) so the full `MemoryServer` + project DB resolution pipeline is used.

## Files Changed
- `crates/memory-server/src/tools.rs` — Lines 1274-1324: Added scope normalization (`effective_scope` via `match`), `scope_remapped` tracking, and conditional note prepended to output.
- `crates/memory-server/src/cli.rs` — `Commands::Search` variant: Added `--project` option (`project: Option<String>`).
- `crates/memory-server/src/bootstrap.rs` — `Commands::Search` handler: Replaced direct `open_cli_store_read_only` + `store.search` with `dispatch_cli_tool("search_memory", ...)` routing, passing `query`, `path_prefix`, `top_k`, and `project` as args. This leverages the full `MemoryServer` pipeline including named project DB resolution.

## Commands Run
- `cargo check -p memory-server` — ✅ Clean (no warnings)
- `cargo test -p memory-server -- tachi_search` — ✅ 1 test passed, 0 failed

## Verification Performed
- Compilation succeeds with no errors or warnings
- Existing test `tachi_search_wiki_scope_defaults_to_named_wiki_project` still passes
- Code follows the same `dispatch_cli_tool` pattern used by `WikiSearch`, `WikiWrite`, and `Remember` commands

## Remaining Risks or Blockers
- The old `Commands::Search` handler did embedding inline via `LlmClient::embed_voyage`. The new handler routes through `handle_search_memory` which manages its own embedding pipeline. If there are edge cases where the embedding behavior differs, results may change slightly.
- No new test was added for the scope fallback behavior or `--project` flag. A follow-up task could add dedicated unit/integration tests for these paths.
- The `open_cli_store_read_only` + `SearchOptions` direct-search path is no longer used for the CLI `search` command. If other code paths relied on that specific behavior (e.g., `record_access: false`, specific `candidates_per_channel` defaults), they would now use `SearchMemoryParams` defaults instead.

# Execution Plan

## Bug 1: `tachi_search(scope="project")` returns empty results
**File**: `crates/memory-server/src/tools.rs` (lines 1274-1324)

1. After lowercasing scope, add a `match` to map unrecognized scope values ("project", "global", "user", etc.) to `"all"`.
2. Track whether remapping occurred (`scope_remapped`).
3. Use `effective_scope` in the existing `if` branches instead of raw `scope`.
4. When `scope_remapped` is true, prepend a note to the output explaining the fallback.

## Bug 2: `tachi search` CLI doesn't support `--project`
**Files**: `crates/memory-server/src/cli.rs` (Search variant) and `crates/memory-server/src/bootstrap.rs` (handler)

1. Add `--project` option to `Commands::Search` in `cli.rs`.
2. In `bootstrap.rs`, refactor the `Commands::Search` handler to route through `dispatch_cli_tool` (same pattern as `WikiSearch`), passing the `project` parameter. This way the full `MemoryServer` + project DB resolution pipeline is used.

## Verification
1. `cargo check -p memory-server`
2. `cargo test -p memory-server -- tachi_search` (if tests exist)

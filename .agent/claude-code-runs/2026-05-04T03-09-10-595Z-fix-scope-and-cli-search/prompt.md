# Delegated Task

Fix two bugs in the Tachi memory server codebase at /Users/kckylechen/Desktop/Sigil:

## Bug 1: `tachi_search(scope="project")` returns empty results

**File**: `crates/memory-server/src/tools.rs`, lines 1274-1324

The `tachi_search` facade's `scope` parameter only matches `"wiki"`, `"memory"`, or `"all"`. When users pass `scope="project"` (which is valid in the save context), neither branch executes and the result is empty.

**Fix**: In the `tachi_search` method, add fallback handling so that unrecognized scope values (especially "project", "global", "user") are treated as "all" (search both wiki and memory). Add a `scope_note` field in the response when this fallback is applied.

The fix should look something like:
```rust
let scope = params.scope.to_ascii_lowercase();
let mut parts = Vec::new();

// Normalize scope: "wiki", "memory", "all" are the valid subsystem selectors.
// "project", "global", "user" are DB-target hints that callers sometimes pass
// by analogy with save_memory's scope parameter. Treat them as "all".
let effective_scope = match scope.as_str() {
    "wiki" | "memory" | "all" => scope.as_str(),
    _ => "all",
};
let scope_remapped = effective_scope != scope.as_str();

if effective_scope == "wiki" || effective_scope == "all" {
    // ... existing wiki search code
}

if effective_scope == "memory" || effective_scope == "all" {
    // ... existing memory search code
}
```

And if scope_remapped is true, prepend a note like:
```
> **Note**: scope='project' was interpreted as 'all' (search both wiki and memory). The `scope` parameter selects *which subsystems* to search (wiki/memory/all), not which DB. Use the `project` parameter to target a specific project DB.\n\n
```

## Bug 2: `tachi search` CLI doesn't support `--project`

**File**: `crates/memory-server/src/bootstrap.rs`, around line 1874

The CLI `tachi search` command opens `db_path` (global DB) directly with `open_cli_store_read_only(db_path)`. It doesn't accept a `--project` parameter.

**Fix**: 
1. Add a `--project` option to the `Search` variant in the CLI enum (find the `Commands` enum definition in `cli.rs` or wherever it's defined)
2. In the search handler in `bootstrap.rs`, when `--project` is provided, resolve the named project DB path using `MemoryServer::resolve_named_project_db_path(project_name)` and open that DB instead of `db_path`

Look at how `Commands::Remember` handles its `--project` parameter for reference — it delegates to `dispatch_cli_tool` which spins up a temporary MemoryServer. The Search command should do something similar, or simply resolve the DB path and open it.

## After fixing

1. Run `cargo check -p memory-server` to verify compilation
2. Run `cargo test -p memory-server -- tachi_search` to check related tests
3. If tests pass, report the exact changes made

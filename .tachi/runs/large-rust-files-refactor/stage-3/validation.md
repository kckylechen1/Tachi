# Stage 3 — Validation

## `cargo check -p memory-server --tests` (last 5 lines)

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.41s
```

(Earlier full build output produced no warnings or errors related to the
refactor; `cargo check` re-uses the incremental build cache. A from-scratch
`cargo test --no-run` was run earlier and finished in 32s with the
`Checking memory-server v1.1.0 … Finished` line as the only memory-server
diagnostic.)

## `cargo test -p memory-server` — full result lines

```
test result: ok. 270 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 12.77s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

(First line = `memory_server` unittests binary including `tests::*` submodules.
Second line = `tachi_hub` binary, which has no inline tests of its own.)

## Test count: baseline vs after

| Stage              | Passed | Failed |
|--------------------|-------:|-------:|
| **Baseline** (5d150be) |  270  |   0   |
| **After refactor**     |  270  |   0   |

Match: ✅

## Sample test paths (new module structure)

```
test tests::memory_tests::save_memory_clamps_importance_into_valid_range ... ok
test tests::wiki_tests::wiki_export_obsidian_writes_markdown_index_and_wikilinks ... ok
```

Each themed submodule is reachable as `tests::<submod>::<fn_name>`, confirming
the directory module replacement worked and the bucketing is exposed in test
paths.

## File layout verification

```
$ ls crates/memory-server/src/tests.rs
ls: crates/memory-server/src/tests.rs: No such file or directory

$ ls crates/memory-server/src/tests/
bootstrap_tests.rs
dispatch_tests.rs
facade_tests.rs
handoff_tests.rs
hub_tests.rs
kanban_tests.rs
memory_tests.rs
mod.rs
pack_tests.rs
profile_tests.rs
proxy_tests.rs
sandbox_tests.rs
skill_tests.rs
vault_tests.rs
vc_tests.rs
wiki_tests.rs
```

## File sizes (all < 1000 lines target)

```
 247  bootstrap_tests.rs
 163  dispatch_tests.rs
 130  facade_tests.rs
 148  handoff_tests.rs
 622  hub_tests.rs
 362  kanban_tests.rs
 635  memory_tests.rs
 269  mod.rs
 586  pack_tests.rs
 178  profile_tests.rs
 222  proxy_tests.rs
 309  sandbox_tests.rs
 870  skill_tests.rs   <- largest themed file
 636  vault_tests.rs
 234  vc_tests.rs
 625  wiki_tests.rs
6236  total
```

Largest themed file: `skill_tests.rs` at 870 lines (under the 1000-line target).

## External reference check

```
$ grep -rn "crate::tests::\|super::tests::" crates/memory-server/src/
(no matches)
```

`mod tests;` remains a `#[cfg(test)]`-gated module declared in
`crates/memory-server/src/main.rs:653`; no other source file references the
test helpers, confirming the refactor is contained.

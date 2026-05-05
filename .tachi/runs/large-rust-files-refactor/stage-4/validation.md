# Stage 4 — validation

## 1. MCP `#[tool]` surface diff (must be empty)

```
$ grep -E '^\s*#\[tool\(' crates/memory-server/src/tools.rs | sort > /tmp/tools_after.txt
$ git show refactor/large-rust-files:crates/memory-server/src/tools.rs \
    | grep -E '^\s*#\[tool\(' | sort > /tmp/tools_before.txt
$ diff /tmp/tools_before.txt /tmp/tools_after.txt
$ echo $?
0
```

**Result: zero diff.** All MCP tool macro attributes (names + descriptions) are
byte-identical.

## 2. cargo check

```
$ cargo check -p memory-server 2>&1 | tail -3
    Checking memory-server v1.1.0 (.../crates/memory-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 40.39s
```

```
$ cargo check -p memory-server 2>&1 | grep -E "warning|error"
(no output)
```

**Result: clean. Zero warnings, zero errors.**

## 3. cargo test

Baseline (`refactor/large-rust-files` @ 5d150be):
```
test result: ok. 270 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

After Stage 4:
```
test result: ok. 270 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 18.58s
```

**Result: 270 == 270, identical pass count.**

## 4. tools.rs line count

```
$ wc -l crates/memory-server/src/tools.rs
1536 crates/memory-server/src/tools.rs
```

Down from 2087 (−551 lines, target was < 1500–1000; 1536 is just over the
soft target because several "thin enough" facade routers were left in place
to avoid pointless module proliferation — see result.md).

## 5. New ops modules

```
$ wc -l crates/memory-server/src/facade_*_ops.rs crates/memory-server/src/web_search_ops.rs
 181 crates/memory-server/src/facade_save_ops.rs
  82 crates/memory-server/src/facade_search_ops.rs
 104 crates/memory-server/src/facade_memory_ops.rs
 252 crates/memory-server/src/web_search_ops.rs
```

All under the 400-line per-file ceiling.

## 6. impl MemoryServer block integrity

All `#[tool]` methods remain inside the single `impl MemoryServer` block opened
at `tools.rs:144` (`#[tool_router(vis = "pub(crate)")]`) and closed at the file
end (line 1536). No tool methods were moved out of the block — only their bodies
were rewritten as one-line delegations. Verified by inspection that the `impl`
block is unbroken between the first and last `#[tool(...)]`.

## 7. Module wiring

`crates/memory-server/src/main.rs` updated to register the new modules
(alphabetically inserted alongside existing `*_ops` siblings):

```
mod facade_memory_ops;
mod facade_save_ops;
mod facade_search_ops;
mod web_search_ops;
```

# Code Quality Report

Scope: `crates/memory-server/src/`, `crates/memory-core/src/`

## Baseline

| phase | file | line range | severity | finding | action taken |
| --- | --- | --- | --- | --- | --- |
| Baseline | workspace | n/a | info | Starting state included pre-existing edits in `crates/memory-server/src/shell_ops.rs` and unrelated untracked files. | Preserved existing work; scoped edits around it. |
| Baseline | workspace | n/a | info | Baseline verification before edits. | `cargo test -p memory-server` passed: 271 tests. `cargo clippy -p memory-server -p memory-core` passed. |

## Phase 1 - unwrap/expect Safety Audit

| phase | file | line range | severity | finding | action taken |
| --- | --- | --- | --- | --- | --- |
| Phase 1 | `crates/memory-server/src/bootstrap.rs` | 2484-2491 | medium | Production log writer used `Mutex::lock().unwrap()`, which can panic after poison. | Converted poison to `std::io::Error` in `Write` implementation. |
| Phase 1 | `crates/memory-server/src/wiki_ops.rs` | 104-114, 983-993 | low | `as_object_mut().expect(...)` followed a local object reset but still encoded the invariant as a panic. | Replaced with `if let`/early return fallback while preserving behavior. |
| Phase 1 | `crates/memory-server/src/mcp_pool.rs` | 443-465 | medium | Semaphore map lookup used `unwrap()` after insert/rebuild. | Replaced with contextual `rmcp::ErrorData::internal_error` if the map invariant fails. |
| Phase 1 | `crates/memory-server/src/repair/inventory.rs` | 91-95 | low | Single-hit selection used `next().unwrap()` after length check. | Replaced with `hits.into_iter().next()`. |
| Phase 1 | `crates/memory-server/src/repair/report.rs` | 141-154 | low | JSON report rendering used `to_string_pretty(...).unwrap()`. | Replaced with explicit render error output. |
| Phase 1 | `crates/memory-server/src/foundry_runtime_ops/maintenance.rs` | 778-802 | medium | Coherent bucket fallback used `expect("non-empty")` after a prior emptiness check. | Replaced with structured `Skipped(no_coherent_bucket)` fallback for invariant drift. |
| Phase 1 | `crates/memory-server/src/main.rs` | 479-491 | medium | Project hot-swap state used `expect(...)` for coupled `Option` values. | Replaced with tuple match that only constructs project state when all required values are present. |
| Phase 1 | `crates/memory-server/src/pipeline_ops.rs` | 513-517, 547-552, 654-673, 706-712, 806-827, 967-971, 999-1041 | medium | Multiple response serialization paths used `serde_json::to_string(...).unwrap()` on runtime responses. | Added local `serialize_json` helper and propagated serialization errors as `Result<String, String>`. |
| Phase 1 | `crates/memory-core/src/noise.rs` | 11-117 | info | Regex construction unwraps use static literals. | Classified safe: literals are fixed at compile time and fail only on programmer error during startup. Left unchanged. |
| Phase 1 | `crates/memory-server/src/foundry_runtime_ops/handlers.rs` | 644-677 | info | Regex construction unwraps use static literals. | Classified safe: literals are fixed at compile time and fail only on programmer error during startup. Left unchanged. |
| Phase 1 | `crates/memory-server/src/daily_pipeline.rs` | 1115-1116 | info | `FixedOffset::east_opt(8 * 3600).expect(...)` uses a constant valid offset. | Classified safe and left unchanged. |

### Phase 1 Completion Summary

Replaced unsafe production unwrap/expect sites in runtime paths with error propagation or explicit fallback. Left test-only unwrap/expect calls untouched and classified static regex/constant-time invariants as safe. Verification passed: `cargo test -p memory-server` (271 tests) and `cargo clippy -p memory-server -p memory-core`.

## Phase 2 - Dead Code Cleanup

| phase | file | line range | severity | finding | action taken |
| --- | --- | --- | --- | --- | --- |
| Phase 2 | `crates/memory-server/src/prompts.rs` | 26-34 | low | `CURATION_PROMPT` had `#[allow(dead_code)]` and no call sites in `memory-server` or `memory-core`. | Removed the unused constant and attribute. |
| Phase 2 | `crates/memory-server/src/dispatch_ops.rs` | 836-853 | low | `parse_claude_output` had `#[allow(dead_code)]` and no call sites; dispatch writes raw output directly. | Removed the unused helper and attribute. |
| Phase 2 | `crates/memory-server/src/tool_params/*.rs` | multiple | info | Most `#[allow(dead_code)]` occurrences are externally deserialized MCP/tool parameter structs and fields. Rust does not see field reads performed by JSON/schema clients. | Retained to preserve public tool schema compatibility and avoid public API changes. |
| Phase 2 | `crates/memory-server/src/vault_ops.rs`, `crates/memory-server/src/kanban.rs` | multiple | info | Parameter structs are part of the MCP tool surface and deserialized externally. | Retained; not true dead code. |
| Phase 2 | `crates/memory-server/src/main.rs`, `crates/memory-server/src/cli_client.rs`, `crates/memory-server/src/mcp_pool.rs`, `crates/memory-server/src/daemon_lock.rs`, `crates/memory-server/src/foundry_scheduler.rs`, `crates/memory-server/src/repair/mod.rs`, `crates/memory-server/src/foundry_runtime_ops/maintenance.rs` | multiple | info | Remaining suppressions are internal runtime helpers, state fields, compatibility payloads, or values intentionally carried for observability/future correlation. | Retained; no test-only-only item was identified for `#[cfg(test)]` gating. |
| Phase 2 | `crates/memory-core/src/` | n/a | info | No `#[allow(dead_code)]` occurrences were found under `memory-core/src`. | No action needed. |

### Phase 2 Completion Summary

Removed two truly unused items and left externally deserialized schema/runtime compatibility suppressions intact to avoid public API changes. No items were identified as used only in tests. Verification passed: `cargo test -p memory-server` (271 tests) and `cargo clippy -p memory-server -p memory-core`.

## Phase 3 - Unsafe Audit

| phase | file | line range | severity | finding | action taken |
| --- | --- | --- | --- | --- | --- |
| Phase 3 | `crates/memory-server/src/bootstrap.rs` | 1084-1085 | low | `isatty(STDOUT_FILENO)` used FFI for a terminal check. | Replaced with safe `std::io::IsTerminal`. |
| Phase 3 | `crates/memory-server/src/daemon_lock.rs` | 119-127 | medium | `flock(LOCK_UN)` in `Drop` lacked a SAFETY comment. | Audited fd lifetime and added SAFETY comment. |
| Phase 3 | `crates/memory-server/src/daemon_lock.rs` | 137-140 | medium | `flock(LOCK_EX | LOCK_NB)` lacked a SAFETY comment. | Audited valid `File` fd usage and added SAFETY comment. |
| Phase 3 | `crates/memory-server/src/daemon_lock.rs` | 170-178 | medium | `kill(pid, 0)` lacked a SAFETY comment. | Audited non-signaling probe semantics and added SAFETY comment. |
| Phase 3 | `crates/memory-core/src/db/sqlite_vec.rs` | 6-16 | high | `sqlite3_auto_extension` registration transmutes the sqlite-vec init function pointer and lacked a SAFETY comment. | Audited ABI expectations, one-time registration, and added SAFETY comment. |
| Phase 3 | `crates/memory-server/src/shell_ops.rs` | 736-748 | info | The only unsafe block in `shell_ops.rs` is inside `#[cfg(test)]` for `std::env::set_var` and already has a SAFETY note. | Left test code unchanged per the no-test-code constraint outside Phase 4. |

### Phase 3 Completion Summary

Removed one avoidable unsafe terminal check and documented the remaining required FFI calls with local SAFETY comments. No undefined-behavior issue was found in the audited production unsafe blocks. Verification passed: `cargo test -p memory-server` (271 tests) and `cargo clippy -p memory-server -p memory-core`.

## Phase 4 - Test Coverage Gaps

| phase | file | line range | severity | finding | action taken |
| --- | --- | --- | --- | --- | --- |
| Phase 4 | `crates/memory-server/src/shell_ops.rs` | 382-398, 565-576 | medium | Public shell handler branches lacked integration coverage for invalid actions and invalid status `flow_id`. | Added `tachi_shell_rejects_invalid_action` and `tachi_shell_status_rejects_invalid_flow_id` in `crates/memory-server/src/tests.rs`. |
| Phase 4 | `crates/memory-server/src/dispatch_ops.rs` | 155-215 | medium | Direct note file helper lacked edge coverage for non-ASCII titles with no ASCII slug tokens. | Added `write_note_file_falls_back_when_slug_has_no_ascii_tokens`; fixed slug fallback to `note`. |
| Phase 4 | `crates/memory-server/src/wiki_ops.rs` | 1186-1286 | low | Wiki browse had related-entry behavior only covered for small limits. | Added `wiki_browse_large_limit_keeps_related_entries_empty`. |
| Phase 4 | `crates/memory-server/src/capability_ops.rs` | 609-629 | low | Capability recommendation limit normalization had no edge test for `limit=0`. | Added `recommend_capability_limit_zero_normalizes_to_one`. |
| Phase 4 | `crates/memory-server/src/foundry_ops.rs` | 904-982 | medium | Foundry public proposal handlers had limited direct handler coverage for empty lists and invalid review statuses. | Added `list_agent_evolution_proposals_empty_result_accepts_zero_limit` and `review_agent_evolution_proposal_rejects_invalid_status`. |
| Phase 4 | `crates/memory-server/src/copilot_ops.rs` | 221-448 | info | Existing coverage already exercises public wiki write/search/task brief/progress flows plus inline helper tests. | Mapped coverage; no additional test needed in this pass. |

### Phase 4 Completion Summary

Added seven focused tests in `crates/memory-server/src/tests.rs` and one small dispatch slug fallback fix. Verification passed: `cargo test -p memory-server` (278 tests) and `cargo clippy -p memory-server -p memory-core`.

## Phase 5 - Documentation Sweep

| phase | file | line range | severity | finding | action taken |
| --- | --- | --- | --- | --- | --- |
| Phase 5 | `docs/agent-brief-v4-review.md`, `docs/code-review-superpowers.md`, `docs/final-review-2026-03-26.md`, `docs/tachi-evolution-report-2026-03-25.md`, `docs/audit-2026-04-30.md` | whole files | low | Completed/merged review and design artifacts were still in the active docs directory. | Moved to `docs/archive/`. |
| Phase 5 | `docs/INSTALL.md` | 5-220 | medium | Install guide did not describe current core/server architecture and had stale profile/default wording. | Added architecture summary, profile-first MCP examples, `tachi_task`/`tachi_shell`, and current profile default guidance. |
| Phase 5 | `README.md` | Tool Surface section | medium | README mentioned profile bundles but did not describe the current core/server/surface split or newer facade tools. | Updated tool-surface wording and added current architecture notes. |
| Phase 5 | `docs/handoff_gh_and_brainstorming.md`, `docs/dispatch-v2-two-stage-design.md` | reference sections | low | Active docs linked to an archived code-review artifact at its old path. | Updated references to `docs/archive/code-review-superpowers.md`. |

### Phase 5 Completion Summary

Archived stale completed docs, updated install/readme architecture and profile guidance, and fixed active references to archived artifacts. Verification passed: `cargo test -p memory-server` (278 tests) and `cargo clippy -p memory-server -p memory-core`.

# Phase 2 Completion Report — Tachi LLM 3-Layer Migration

## Summary

Successfully migrated the Rust backend LLM client from 4 independent lanes to a 3-layer architecture:
- **Layer 1 — Embedding/Rerank**: Voyage (unchanged)
- **Layer 2 — Front-line LLM**: Extract + Summary (27B-class, hub skills, scans)
- **Layer 3 — Foundry LLM**: Reasoning + Distill (strong model, distillation, evolution)

All 255 tests pass. No regressions.

## Files Changed

### 1. `crates/memory-server/src/llm.rs`
- **Summary lane**: fallback changed from `SUMMARY_* → DISTILL_* → SILICONFLOW_*` to `SUMMARY_* → EXTRACT_* → SILICONFLOW_*`
- **Distill lane**: fallback changed from `DISTILL_* → SUMMARY_* → SILICONFLOW_*` to `DISTILL_* → REASONING_*`
- **Reasoning lane**: fallback changed from `REASONING_* → SILICONFLOW_*` to `REASONING_* → DISTILL_*`
- Build order adjusted: `extract → summary → reasoning → distill` (avoids circular references)
- Added `#[allow(dead_code)]` to `call_llm` (no longer called, kept for backward compat)

### 2. `crates/memory-server/src/hub_ops/call.rs` (line 87)
- `call_reasoning_llm` → `call_extract_llm` (hub skill execution)

### 3. `crates/memory-server/src/server_methods.rs` (line 741)
- `call_llm` → `call_extract_llm` (generic skill execution via MCP)

### 4. `crates/memory-server/src/hub_ops/register.rs` (line 214)
- `call_reasoning_llm` → `call_extract_llm` (skill registration analysis)

### 5. `crates/memory-server/src/hub_ops/security_scan.rs` (line 335)
- `call_reasoning_llm` → `call_extract_llm` (skill security scanning)

### 6. `crates/memory-server/src/foundry_runtime_ops/recall_cache.rs` (line 303)
- `call_reasoning_llm` → `call_extract_llm` (recall cache query generation)

## Commands Run
- `cargo build -p memory-server` — clean build, no warnings
- `cargo test -p memory-server` — 255 passed, 0 failed

## Verification Performed
- All existing env vars (`EXTRACT_*`, `SUMMARY_*`, `DISTILL_*`, `REASONING_*`) continue to work
- Foundry call sites (`evolve.rs:104`, `foundry_ops.rs:460`, `recall.rs:214`) confirmed unchanged
- No hub call sites remain on reasoning/legacy lane
- `call_llm` preserved with `#[allow(dead_code)]` for backward compatibility

## Remaining Risks or Blockers
- **Pre-existing `memory-python` linker error** (pyo3 symbol issue on arm64) — unrelated to this change, exists on main branch
- **Runtime validation**: The 3-layer model should be validated end-to-end with actual LLM calls once deployed, to confirm fallback chains work correctly when specific env vars are missing

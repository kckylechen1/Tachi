# Execution Plan

## Goal
Fix two documentation/comment issues found by Codex Review.

## Changes

### 1. `docs/INSTALL.md` line ~109
- Replace the blanket "Omitted lanes fall back to SILICONFLOW_*" with an accurate per-lane fallback description:
  - EXTRACT / SUMMARY → fall back to SILICONFLOW_*
  - DISTILL ↔ REASONING → cross-fallback only (no SILICONFLOW fallthrough)
  - At least one of DISTILL_* or REASONING_* must be set to use Foundry LLM features

### 2. `.env.example` line ~6
- Update the comment block to clarify SILICONFLOW_* is the fallback for front-line / extraction lanes only

### 3. `crates/memory-server/src/foundry_runtime_ops/recall_cache.rs` line ~268
- Change "reasoning lane" → "extract lane" (or "front-line LLM") in the doc comment

### 4. Verify
- Run `cargo check -p memory-server` to confirm no breakage

# Phase 2 Execution Plan — Rust Backend LLM 3-Layer Migration

## Current State
- 4 lanes: Extract, Distill, Reasoning, Summary
- Extract fallback: `EXTRACT_* → SILICONFLOW_*` ✅ (already correct)
- Distill fallback: `DISTILL_* → SUMMARY_* → SILICONFLOW_*` → needs change
- Reasoning fallback: `REASONING_* → SILICONFLOW_*` → needs change
- Summary fallback: `SUMMARY_* → DISTILL_* → SILICONFLOW_*` → needs change

## Step 1 — No code changes (read-only, already completed)

## Step 2 — Modify fallback chains in `llm.rs::new()`

| Lane | Current fallback chain | New fallback chain |
|------|----------------------|-------------------|
| Extract | `EXTRACT_* → SILICONFLOW_*` | **No change** |
| Summary | `SUMMARY_* → DISTILL_* → SILICONFLOW_*` | `SUMMARY_* → EXTRACT_* → SILICONFLOW_*` |
| Distill | `DISTILL_* → SUMMARY_* → SILICONFLOW_*` | `DISTILL_* → REASONING_*` |
| Reasoning | `REASONING_* → SILICONFLOW_*` | `REASONING_* → DISTILL_*` |

Changes in `llm.rs::new()`:
- **distill**: api_key_envs → `DISTILL_* , REASONING_*`; base_url_envs → `DISTILL_* , REASONING_*`; model_envs → `DISTILL_* , REASONING_*`; default_model → `&reasoning.model`
- **reasoning**: api_key_envs → `REASONING_* , DISTILL_*`; base_url_envs → `REASONING_* , DISTILL_*`; model_envs → `REASONING_* , DISTILL_*`; default_model → `&distill.model`
- **summary**: api_key_envs → `SUMMARY_* , EXTRACT_* , SILICONFLOW_*`; base_url_envs → `SUMMARY_* , EXTRACT_* , SILICONFLOW_*`; model_envs → `SUMMARY_* , EXTRACT_* , SILICONFLOW_*`; default_model → `&extract.model`

⚠️ Ordering constraint: distill depends on reasoning, reasoning depends on distill → circular. We need to break this carefully. Since we can't have circular defaults, we'll use `DEFAULT_REASONING_MODEL` for reasoning's default, and `&reasoning.model` for distill's default. Build order: extract → reasoning → distill → summary.

## Step 3 — Downgrade 5 hub call sites to front-line LLM

| File | Line | Current | New |
|------|------|---------|-----|
| hub_ops/call.rs | 87 | `call_reasoning_llm` | `call_extract_llm` |
| server_methods.rs | 741 | `call_llm` (defaults to Reasoning) | `call_extract_llm` |
| hub_ops/register.rs | 214 | `call_reasoning_llm` | `call_extract_llm` |
| hub_ops/security_scan.rs | 335 | `call_reasoning_llm` | `call_extract_llm` |
| foundry_runtime_ops/recall_cache.rs | 303 | `call_reasoning_llm` | `call_extract_llm` |

### NOT changed (Foundry stays on strong model):
- hub_ops/evolve.rs:104 — `call_reasoning_llm` (Foundry evolve)
- foundry_ops.rs:460 — `call_reasoning_llm` (Foundry ops)
- foundry_runtime_ops/recall.rs:214 — `call_distill_llm` (Foundry recall)

## Step 4 — Verify: `cargo test`

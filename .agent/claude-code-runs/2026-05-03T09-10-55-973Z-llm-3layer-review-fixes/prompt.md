# Delegated Task


## 任务：修复 Codex Review 发现的两个问题

### 问题 1 (Medium): 文档与代码不一致

Distill/Reasoning lane 不再 fallback 到 SILICONFLOW_*，需要更新文档：

1. `docs/INSTALL.md` 第 109 行附近 — 更新 fallback 描述：
   - Extract/Summary → 仍然 fallback 到 SILICONFLOW_*
   - Distill ↔ Reasoning → 互为 fallback，不再 fallback 到 SILICONFLOW_*
   - 说明：如果要用 Foundry LLM，必须配 DISTILL_* 或 REASONING_* 至少一组

2. `.env.example` 第 6 行附近 — 更新注释，说明 SILICONFLOW_* 现在只是前台 LLM 的 fallback

### 问题 2 (Low): 注释过时

`crates/memory-server/src/foundry_runtime_ops/recall_cache.rs` 第 268 行附近 — 把 "reasoning lane" 相关注释改为 "extract lane" 或 "front-line LLM"

### 约束
- 只改注释和文档，不改代码逻辑
- 改完后跑 `cargo check -p memory-server` 确认无 break


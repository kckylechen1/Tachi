# Delegated Task


## 任务：Tachi LLM 三层模型 Phase 2 — Rust 后端迁移

### 背景
Tachi 的 LLM 调用正在从 4 条独立 lane（Extract/Distill/Reasoning/Summary）收敛为 3 层：
- **Embedding / Rerank** — Voyage 专用模型
- **前台 LLM** — 27B 级（Extract + Summary lane），负责提取、摘要、hub skill、扫描
- **Foundry LLM** — 强模型（Distill + Reasoning lane），负责蒸馏、进化、eval

### 具体任务

**第 1 步：读代码，理解现状**

1. 读 `crates/memory-server/src/llm.rs`，找到 `load_lane` 函数，理解当前 fallback 链
2. 读以下 5 个文件，找到每个调用点当前用的是哪个 lane：
   - `crates/memory-server/src/call.rs:87` — hub_call 技能执行
   - `crates/memory-server/src/server_methods.rs:741` — 通用 LLM 调用
   - `crates/memory-server/src/register.rs:214` — skill 注册后分析
   - `crates/memory-server/src/security_scan.rs:335` — skill 安全扫描
   - `crates/memory-server/src/recall_cache.rs:303` — recall cache query 生成

**第 2 步：修改 fallback 链**

在 `load_lane` 中调整 fallback 逻辑：
- Extract: `EXTRACT_* → SILICONFLOW_*`（前台默认）
- Summary: `SUMMARY_* → EXTRACT_* → SILICONFLOW_*`（跟前台一致）
- Distill: `DISTILL_* → REASONING_*`（Foundry 内部 fallback）
- Reasoning: `REASONING_* → DISTILL_*`（Foundry 内部 fallback）

**第 3 步：Hub 调用点降到前台 LLM**

把以下 5 个调用点从 `call_reasoning_llm` 改为 `call_extract_llm`（或对应的前台 lane）：
- `call.rs:87`
- `server_methods.rs:741`
- `register.rs:214`
- `security_scan.rs:335`
- `recall_cache.rs:303`

**第 4 步：验证**

运行 `cargo test` 确认所有测试通过。

### 约束
- 不改动 Embedding / Rerank 相关代码（Voyage 不变）
- 不新增 env var，复用现有的 EXTRACT_*/SUMMARY_*/DISTILL_*/REASONING_*
- 不改 Foundry 调用点（evolve.rs, foundry_ops.rs, distill_trajectory 已确认正确）
- 现有 per-lane env vars 继续生效，不 break 旧配置


# LLM 三层模型重构计划

> 日期: 2026-05-03
> 状态: Go `tachi-helper` 已退休；当前配置入口由 Rust `tachi setup --interactive` 与 Vault/setup CLI 承接。

## 目标

将 Tachi 模型调用从 4 条独立 lane（Extract/Distill/Reasoning/Summary）收敛为 **3 层**：

| 层 | 用途 | 默认模型 | 未来方向 |
|---|---|---|---|
| **Embedding / Rerank** | 向量化 + 重排序 | Voyage voyage-4 / rerank-2.5 | 本地 bge-m3 / bge-reranker |
| **前台 LLM** | 提取、摘要、hub skill、扫描 | Qwen3.5-27B (SiliconFlow) | 本地 27B 量化 |
| **Foundry LLM** | 蒸馏、进化、eval、compaction | DeepSeek V4 Flash | 保持云端 |

```
现状 (4 lane):                     目标 (3 层):
  Extract  ─┐                        Embedding/Rerank  ← Voyage / 硅基流动
  Summary  ─┤ → 全走同一个模型          前台 LLM          ← 27B 级（提取、技能、扫描）
  Distill  ─┤                        Foundry LLM       ← 强模型（蒸馏、进化、eval）
  Reasoning ┘ → 拆成前台/后台
```

## 调用点归属

### Embedding / Rerank（Voyage 专用模型，7 个调用点）

| 文件 | 方法 | 场景 |
|---|---|---|
| memory_search_ops.rs:283 | embed_voyage (query) | 搜索向量化 |
| handlers.rs:317,980 | embed_voyage_batch (document) | session 记忆向量化 |
| enrichment.rs:94 | embed_voyage_batch (document) | 新记忆入库 |
| bootstrap.rs:1596,3245 | embed_voyage / batch | backfill CLI |
| recall.rs:253 | rerank_voyage | recall 结果重排序 |

### 前台 LLM（27B 级，11 个调用点）

| 文件 | 当前方法 | 场景 |
|---|---|---|
| handlers.rs:864 | call_extract_llm | session capture 提取记忆 |
| pipeline_ops.rs:617,802 | extract_facts | 结构化事实提取 |
| wiki_ops.rs:535 | call_extract_llm | wiki ingest 元数据 |
| enrichment.rs:120 | generate_summary | 新记忆自动摘要 |
| bootstrap.rs:3321 | generate_summary | backfill summaries |
| call.rs:87 | call_reasoning_llm | hub_call 技能执行 |
| server_methods.rs:741 | call_llm | 通用 LLM 调用 |
| register.rs:214 | call_reasoning_llm | skill 注册后分析 |
| security_scan.rs:335 | call_reasoning_llm | skill 安全扫描 |
| recall_cache.rs:303 | call_reasoning_llm | recall cache query 生成 |

### Foundry LLM（强模型，5 个调用点）

| 文件 | 当前方法 | 场景 |
|---|---|---|
| maintenance.rs:826 | generate_distill | 定期记忆蒸馏 |
| recall.rs:214 | call_distill_llm | context compaction |
| evolve.rs:104 | call_reasoning_llm | skill prompt 进化 |
| complete_ops.rs:198 → call.rs:115 | distill_trajectory（已退役） | 历史 trajectory 蒸馏；当前不再由 completion 调用 |
| foundry_ops.rs:460 | call_reasoning_llm | agent evolution 综合 |

### OpenClaw harness 参考数据

Foundry LLM 选型参考 `~/.openclaw/agents/yaya/harness/eval/`：

| 模型 | 蒸馏 | 审计 | 速度 | 备注 |
|---|---|---|---|---|
| MiniMax M2.7 | 10 | 10 | 快 | 蒸馏+审计双冠 |
| GLM-5.1 | 9-10 | 9 | 慢 | 推理强但慢 |
| DeepSeek V4 Flash | — | — | 快 | 新选项，性价比高 |

## 实施步骤

### Phase 1: Setup 配置入口 ✅

- [x] Rust `tachi setup --interactive` 成为当前 onboarding/config 入口
- [x] `tachi vault setup-keys` 与 setup wizard 复用 provider key 定义
- [x] config.env 继续兼容: 前台 → EXTRACT_* + SUMMARY_*，Foundry → DISTILL_* + REASONING_*
- [x] Go `tools/tachi-helper` 已不再作为产品入口维护

### Phase 2: Rust llm.rs 后端迁移

1. **Hub 调用点降到前台 LLM**
   - call.rs:87 → 走 Extract lane（或新增 frontend lane）
   - server_methods.rs:741 → 同上
   - register.rs:214 → 同上
   - security_scan.rs:335 → 同上
   - recall_cache.rs:303 → 同上

2. **确保 Foundry 调用点走 Distill/Reasoning lane**
   - evolve.rs:104 → 已走 Reasoning ✓
   - foundry_ops.rs:460 → 已走 Reasoning ✓
   - [历史设计，已退役] distill_trajectory → 走 hub_call → skill 执行 → Reasoning；当前不提供该自动 trajectory-to-skill 管线

3. **load_lane fallback 链调整**
   - Extract: `EXTRACT_* → SILICONFLOW_*`（前台默认）
   - Summary: `SUMMARY_* → EXTRACT_* → SILICONFLOW_*`（跟前台一致）
   - Distill: `DISTILL_* → REASONING_*`（Foundry 内部 fallback）
   - Reasoning: `REASONING_* → DISTILL_*`（Foundry 内部 fallback）

### Phase 3: 未来本地化（远期）

- 本地 27B 量化替代前台 API 调用
- 本地 Whisper 语音转文字
- 本地 embedding/rerank 模型

## 兼容性

- 现有 per-lane env vars (EXTRACT_*, DISTILL_* 等) 继续生效
- Rust setup wizard / vault setup 命令写入或导入对应 env var，新用户开箱即用
- 不引入新的 env var 命名（不搞 LOCAL_LLM / CLOUD_LLM）

## 验证

- [ ] 全部 251+ tests 通过
- [ ] config.env 兼容性：旧配置不 break
- [ ] Rust setup wizard 正确写入 EXTRACT_*/SUMMARY_* 和 DISTILL_*/REASONING_*
- [ ] hub_call / security_scan / skill_analysis 走前台 LLM
- [x] 自动 trajectory-to-skill generation 已退役；`tachi_skill` 仅支持静态、已审查 skill 的 discover/run，`tachi_complete` 报告退役且不创建 skill

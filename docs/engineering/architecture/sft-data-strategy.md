# SFT 数据战略：从 1,654 条高质量样本到 Tachi 全系统提升

**Status:** Historical research plan; superseded for production gating by
[`model-training-eval-gate.md`](model-training-eval-gate.md)
**Date:** 2026-06-03
**Boundary update:** 2026-06-09
**Factory retired:** 2026-07-03 (PR #474) — the daily SFT generator (`run_daily_sft_distillation`) and ~1,800 `/sft` recall rows were deleted; this document is now fully historical, not merely superseded-for-gating.
**Source:** `/Volumes/Storage/agent_logs/sft/redistilled_v4_all_engineering.jsonl` (1,654 entries)
**Scope:** Tachi 全系统 — Agent Router · Dispatch · Shell · Wiki · Memory · Briefing · Foundry

## 0. 2026-06-09 Production Boundary

This document is the original research map for using the 1,654 engineering SFT
samples. It is no longer the production implementation contract for model
training or route-policy mutation.

Current production rule:

- SFT rows remain isolated from ordinary recall.
- Dispatch can use SFT examples only as style-only exemplars.
- `tachi_agent_eval(action="aggregate_live")`, DispatchProfile, MBIT cards, and
  reviewed route-policy/profile-card overlays are the production routing layer.
- Qwen/LoRA/fine-tune work is deferred until the eval target and artifact
  promotion gate in
  [`model-training-eval-gate.md`](model-training-eval-gate.md)
  is satisfied.
- Model-training artifacts stay under run-scoped `foundry-runs` paths and never
  become production memory/wiki/docs by default.

The unchecked LoRA/vector-DB tasks below should be read as research options,
not instructions to import SFT into normal Tachi recall or to replace the
dispatch policy layer.

---

## 1. 数据总览

### 1.1 核心数据集

| 文件 | 条目数 | 领域 | 质量标记 |
|------|--------|------|---------|
| `redistilled_v4_all_engineering.jsonl` | **1,654** | quant_engineering | redistilled_demo_standard_v1 |
| `redistilled_v4_final.jsonl` | 3,082 | 混合 (engineering + trading) | redistilled_demo_standard_v1 |
| `rewrite_output_amp.jsonl` + batch2 | 95 | Amp sessions | rewrite |
| `rewrite_output_antigravity_batch1-3` | 99 | Antigravity sessions | rewrite |

### 1.2 数据特征

| 特征 | 数值 | 说明 |
|------|------|------|
| 平均消息数 | **3.0** | 极度精炼：system → user → assistant |
| 中英比例 | 56% EN / 44% ZH | 符合 Tachi 用户场景 |
| 含 verification | **70.1%** | `cargo test` / `clippy` / `rg` 已成标配 |
| 含 risk assessment | 14.0% | 评估风险后再动手 |
| 含 counter-proposal | 37.2% | 不只给方案，还给"不要怎么做" |

### 1.3 结构化输出标记分布

```
[结论]  — 386 条 (23.3%)  → 一句话总结
[根因]  — 425 条 (25.7%)  → 为什么发生
[方案]  — 444 条 (26.8%)  → 建议怎么做
[反方案] — 499 条 (30.2%)  → 不建议怎么做
[验证]  — 535 条 (32.3%)  → 怎么验证
```

**这套 5 段式标记是经过验证的有效结构**，可以直接作为 Tachi 的强制输出标准。

### 1.4 用户意图分布（Agent Router 训练标签）

Phase 1 dispatch fleet is **four agents** only — see [`agent-fleet.md`](agent-fleet.md). Legacy labels in raw SFT (`gemini`, `qwen`, `copilot`) are remapped at train time.

| 意图 | 占比 | 建议路由 (Phase 1) | Legacy label remap |
|------|------|-------------------|-------------------|
| `other` (综合任务) | 53.9% | Classifier 判断 | — |
| `fix_request` (bug/修复) | 16.3% | → `claude` | — |
| `review_request` (代码审查) | 15.1% | → `kimi` (long context) | `gemini` → `kimi` |
| `plan_request` (架构设计) | 7.3% | → `claude` | — |
| `test_request` (测试) | 3.0% | → `claude` 或 `codex` | `copilot` → `codex` |
| `refactor_request` (重构) | 2.3% | → `codex` | — |
| `explain_request` (解释) | 2.2% | → `kimi` | `qwen` → `kimi` |

---

## 2. 对 Tachi 六大子系统的映射

### 2.1 Agent Router — Task Classification 训练集

**现状：** Agent Router Phase 1 是硬编码 registry，没有自动分类能力。

**数据价值：** 1,654 条样本天然带有用户意图标签，可以直接训练 classifier。

**2026-06-09 boundary:** classifier training is candidate work only. It must
beat the current DispatchProfile / MBIT / live `/eval` policy baseline on an
isolated fixture before it can affect production routing.

**训练目标：**
```json
{
  "input": "Rust 项目拆分进度评估：main.rs 已从 5593 行降到 5123 行，拆出了 5 个模块...",
  "output": {
    "intent": "review_request",
    "agent": "kimi",
    "stage": "review",
    "skills": ["code-review", "rust-analysis"]
  }
}
```

**实现方式：**
- 用 Hugging Face 权重做 LoRA/QLoRA fine-tune；Apple Silicon 优先用 MLX 工具链，CUDA/cloud 可用 Axolotl 或 LLaMA-Factory
- 输入：user prompt 前 100 字
- 输出：`{"intent", "agent", "stage", "skills"}` JSON
- 预期准确率：>85%（基于 7 类意图）
- production gate: see
  [`model-training-eval-gate.md`](model-training-eval-gate.md);
  no trained classifier may directly replace route recommendation without a
  reviewed promotion proposal.

---

### 2.2 Dispatch Prompt Assembly — Few-Shot + 结构化模板

**现状：** `crates/tachi-server/src/dispatch_ops/prompt.rs` 的 `assemble_prompt()` 拼接 briefing + skills + avoidance + task，但缺乏动态示例。

**数据价值：** 样本展示了高质量 assistant 的回复模式：
- 先给结论，再给根因，最后给方案
- 每个方案配反方案和验证步骤
- Tool chain：`rg` → `cat` → `cargo test`

**实现方式：**
1. 按意图分类（fix/review/plan/refactor/test/explain）建立 `crates/tachi-server/src/dispatch_ops/prompt_examples/` 目录
2. `assemble_prompt()` 根据 task 类型注入对应的 few-shot example（2-3 条）
3. 强制要求 agent 输出 `[结论]/[根因]/[方案]/[反方案]/[验证]` 结构

**示例注入：**
```rust
fn few_shot_for_intent(intent: &str) -> Vec<&str> {
    match intent {
        "fix_request" => vec![FIX_EXAMPLE_1, FIX_EXAMPLE_2],
        "review_request" => vec![REVIEW_EXAMPLE_1, REVIEW_EXAMPLE_2],
        "plan_request" => vec![PLAN_EXAMPLE_1],
        _ => vec![],
    }
}
```

---

### 2.3 Wiki / Memory — 结构化输出标准

**现状：** wiki write 的格式靠 agent 自觉遵守，没有强制结构。Obsidian 导出时也没有统一的 section 模板。

**数据价值：** `[结论]/[根因]/[方案]/[反方案]/[验证]` 这套标记在 23-32% 的样本中出现，是**经过验证的有效结构**。

**实现方式：**

1. **写入 `tachi_wiki_write` 的 prompt：**
   > "请用以下结构回复你的分析：
   > - [结论] 一句话总结
   > - [根因] 为什么发生
   > - [方案] 建议怎么做
   > - [反方案] 不建议怎么做
   > - [验证] 怎么验证"

2. **Wiki 渲染：** `markdown_for_obsidian` 解析这些标记 → 生成结构化页面：
   ```markdown
   # Title
   
   ## Conclusion
   ...
   
   ## Root Cause
   ...
   
   ## Proposal
   ...
   
   ## Counter-Proposal
   ...
   
   ## Verification
   ...
   ```

3. **Memory 自动提取：** `tachi_memory save` 时自动从文本中提取 `[结论]`/`[方案]` → 生成 keywords + entities

---

### 2.4 Shell Ops — Retained Dispatch 的动态示例

**当前现状：** Shell 只保留 `dispatch` 和只读 `status` 两个 action；其中只有
`dispatch` 映射到 `skill/superpowers/skills/executing-plans/SKILL.md` SOP。
早期研究设想的 `brainstorm → plan → dispatch → review → ship` 五阶段 lifecycle
已经删除，不再是当前架构或 skill mapping。

**历史数据价值：** 下表仍记录原五阶段研究如何将 SFT 样本分类为动态示例，
但这些分类不是现行 Shell action inventory。

| Stage | 数据中的对应 | 样本数 |
|-------|------------|--------|
| `brainstorm` | explain_request + plan_request | ~156 |
| `plan` | 含 planning 内容 | ~251 |
| `dispatch` | fix_request + refactor_request | ~307 |
| `review` | review_request | ~250 |
| `ship` | verification 内容 | ~1,160 |

**实现方式：**
- 不替换静态 SKILL.md，而是作为 **dynamic context** 注入
- 若未来为保留的 `dispatch` 增加 SFT few-shot，应从同类型数据中检索 2-3 条样本作为 dynamic context；`status` 不注入 SOP 或示例
- 未来可以用隔离的 SFT scope 或 run-scoped fixture 检索；不要把 SFT 数据导入生产普通 recall vector DB

---

### 2.5 Briefing Ops — Context Recovery 的摘要风格

**现状：** briefing 返回 memory + wiki hits，但没有"怎么呈现给用户"的标准格式。

**数据价值：** 3-turn 精炼对话模式 = **最优 briefing 格式**：
- User prompt 平均 50-100 字（具体、不解释背景）
- Assistant 结构化输出（5 段式）
- 无 tool trace，无 thinking，无审批文字

**实现方式：** briefing 输出模仿这种风格：

```markdown
## 当前状态
一句话总结上次 session 的结果

## 关键决策
- Decision 1 (from memory)
- Decision 2 (from wiki)

## 未完成任务
- [ ] Task 1 (from kanban)
- [ ] Task 2 (from kanban)

## 风险注意
- Risk 1 (from memory with risk tag)

## 下一步建议
1. Action item 1
2. Action item 2
```

---

### 2.6 Foundry / Skill Evolution — Gold Standard

**现状：** Foundry 从 session 中 distill memory，但质量不稳定（ historically 65% garbage distill 率）。

**数据价值：** 这些已经是 **v2 → v4 多层过滤的高质量蒸馏产物**，可以直接作为 gold standard。

**实现方式：**
1. 作为 `handle_distill_trajectory()` 的质量 benchmark
2. 从样本中提取高频 pattern → 自动生成 SKILL.md（frontmatter + body）
3. 作为 `synthesize_agent_evolution()` 的 training data

---

## 3. 训练策略

### 3.1 优先级

| 优先级 | 模块 | 训练目标 | 数据量 | 预期提升 |
|--------|------|---------|--------|---------|
| **P0** | Agent Router Classifier | intent → agent/stage/skills | 1,654 | 路由准确率 >85% |
| **P0** | Wiki/Memory Format | 强制 5 段式结构化输出 | 1,654 | wiki 可读性 ↑ |
| **P0** | Dispatch Prompt | few-shot example 注入 | 1,654 | agent 输出质量 ↑ |
| **P1** | Briefing Style | 结构化 briefing 格式 | 1,654 | session 恢复效率 ↑ |
| **P1** | Shell Stage Examples | 动态 skill example | 1,654 | stage 执行质量 ↑ |
| **P1** | Foundry Benchmark | 蒸馏质量评估 | 1,654 | distill 质量 ↑ |
| **P2** | Qwen Secretary | LoRA fine-tune | 500-1000 | 格式化/提取准确率 ↑ |

### 3.2 技术方案

#### 方案 A：本地 Qwen LoRA（推荐）

```bash
# 基础模型：下载标准 Hugging Face 权重，不使用 Ollama 包做训练
huggingface-cli download Qwen/Qwen2.5-32B-Instruct

# Apple Silicon: 用 mlx-lm；CUDA/cloud: 用 axolotl 或 llama-factory
# 数据：1,654 条 engineering 样本
# 目标：7 类 intent 分类 + 5 段式结构化输出
# 硬件：32B 在 M3 Max 36GB 上很紧张，优先 7B/14B 或云端 32B
```

**优点：** 本地运行，零 API 成本，隐私安全  
**缺点：** 32B 本地训练资源紧张；M3 Max 36GB 更适合 7B/14B MLX LoRA，32B 建议云端或专用 CUDA GPU

#### 方案 B：SiliconFlow API Fine-Tune

```bash
# 用 Qwen 27B 或 72B 做 full fine-tune
# 成本：~$5-10（1,654 条 × 3 epochs）
# 时间：30 分钟
```

**优点：** 更快，准确率可能更高  
**缺点：** 依赖外部 API，有数据隐私顾虑

#### 方案 C：混合策略（最终推荐）

| 任务 | 模型 | 方式 |
|------|------|------|
| Agent Router Classifier | Qwen 2.5 32B 本地 | LoRA |
| Wiki Format Enforcement | 规则引擎 + Qwen 辅助 | 本地 |
| Dispatch Few-Shot | 向量检索（SFT 数据向量化） | 本地 |
| Briefing Style | Prompt 工程 + 模板 | 无需训练 |
| Foundry Benchmark | 规则 + Qwen 评分 | 本地 |

---

## 4. 实施 Checklist

### Phase 1: Infrastructure（historical; gated before production）

- [ ] 将 SFT 数据加载到隔离的 `/sft` scope 或 run-scoped fixture（用于显式检索；不得进入普通 recall）
- [ ] 定义 `[结论]/[根因]/[方案]/[反方案]/[验证]` 的解析规则（regex）
- [ ] 更新 `wiki-references-spec.md` 加入结构化输出章节
- [ ] 创建 `crates/tachi-server/src/dispatch_ops/prompt_examples/` 目录，按 intent 分类存放样本

### Phase 2: Agent Router Classifier（historical; deferred to #262）

- [ ] 从 SFT 数据生成训练集（input: prompt, output: intent+agent+stage JSON）
- [ ] LoRA 训练 Qwen 2.5 32B（或 SiliconFlow API fine-tune）
- [ ] 评估准确率（目标 >85%）
- [ ] 在隔离 fixture 上对比当前 DispatchProfile / MBIT / live `/eval` baseline
- [ ] 只通过 reviewed route-policy/profile-card proposal 影响生产；不得直接替换硬编码安全边界

### Phase 3: Wiki / Memory Format（下周）

- [ ] 更新 `tachi_wiki_write` prompt 强制要求 5 段式结构
- [ ] 更新 `crates/tachi-server/src/wiki_ops.rs`：`markdown_for_obsidian` 解析 5 段标记
- [ ] 更新 `crates/tachi-server/src/memory_ops.rs`：自动提取 `[结论]`/`[方案]` 作为 keywords
- [ ] 更新 `crates/tachi-server/src/briefing_ops.rs`：采用 5 段式 briefing 格式

### Phase 4: Foundry Integration（本月）

- [ ] 将 SFT 数据作为 `distill_trajectory` 的 gold standard
- [ ] 从样本中聚类提取高频 pattern → 自动生成 SKILL.md
- [ ] 评估 distill 质量是否提升

---

## 5. 风险与对策

| 风险 | 对策 |
|------|------|
| 数据 bias（quant_engineering 占 57%） | 补充其他领域数据后再平衡 |
| 中文比例 44% 可能影响英文场景 | 按语言分开训练两个 LoRA |
| LoRA 准确率不足 | fallback 到 rule-based（intent keyword 匹配） |
| 结构化标记干扰 agent 自由输出 | 只在 `stage=review/plan` 时强制，其他 stage 可选 |

---

## 6. 附录：数据加载路径

```
/Volumes/Storage/agent_logs/sft/
  ├── redistilled_v4_all_engineering.jsonl    # 1,654 entries (Tachi 主数据)
  ├── redistilled_v4_final.jsonl              # 3,082 entries (全量)
  ├── rewrite_output_amp.jsonl                # 63 entries (Amp 风格)
  ├── rewrite_output_amp_batch2.jsonl         # 32 entries (Amp 风格)
  ├── rewrite_output_antigravity_batch1.jsonl # 34 entries (Antigravity)
  ├── rewrite_output_antigravity_batch2.jsonl # 35 entries (Antigravity)
  ├── rewrite_output_antigravity_batch3.jsonl # 30 entries (Antigravity)
  ├── HIGH_QUALITY_DISTILL_PLAYBOOK.md        # 蒸馏方法论
  └── REDISTILL_SUPPLEMENT.md                 # 补充说明
```

**蒸馏方法论要点：**
1. 先选样本，再改写，不要直接批量拼接
2. 一个样本只表达一个清晰任务
3. 保留工程判断、边界、验证、风险
4. 不保留工具痕迹、审批文字、批处理日志
5. 能用 repo 相对路径，就不要泄漏本机绝对路径

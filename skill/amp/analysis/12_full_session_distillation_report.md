# Agent Session 全量蒸馏报告

> 日期：2026-05-30 | 数据源：2,842 sessions | 7 个 Agent 平台 | 跨 4 个月

---

## 一、数据源全景

### 1.1 原始 Session 分布

| 来源 | Sessions | 总 Turns | 文件大小 | 时间跨度 | 模型 |
|---|---|---|---|---|---|
| **Claude Code** | 1,071 | 21,399 | 277MB | 2026-01 ~ 05 | Claude Opus 4.x / Sonnet 4.x |
| **Codex** | 717 | 93,447 | 270MB | 2026-01 ~ 05 | GPT-5 / 5.4 / 5.5 |
| **Hermes** | 550 | 13,657 | 153MB | 2026-03 ~ 05 | GLM-5.1 / GPT-5.5 / Gemini |
| **Windsurf** | 65 | 4,549 | 253MB | 2026-04 ~ 05 | Claude Opus 4.6/4.7 + GPT-5.5 |
| **Antigravity** | 154 | 43,379 | 107MB | 2026-03 ~ 05 | Gemini 3.1 Pro / Claude 4.6 |
| **OpenCode** | 247 | 45,120 msgs | 62MB | 2026-01 ~ 05 | 多模型聚合 |
| **Qwen** | 37 | 1,277 | 9.8MB | 2026-04 ~ 05 | Qwen 3.7 Max |
| **合计** | **2,842** | **~278,000** | **1.1GB** | — | — |

### 1.2 数据完整性

所有原始 session JSON 文件均已导出并存储在 `/Volumes/Storage/agent_logs/` 下：

```
agent_logs/
├── claude_code/json/          # 1,071 files ✓
├── codex/json/                # 717 files ✓
├── hermes/json/               # 550 files ✓
├── windsurf_json/cascade/     # 50 files ✓
├── windsurf_legacy_json/cascade/  # 15 files ✓
├── antigravity_json/          # 54 files ✓
├── antigravity_legacy_json/   # 100 files ✓
├── opencode/json/             # 248 files ✓ (从 SQLite 导出)
├── qwen/json/                 # 37 files ✓
├── coding_knowledge/          # 576 high-value session 索引
├── trading_knowledge/         # 46 条结构化交易知识
└── antigravity_stats/         # Antigravity 聚合统计
```

---

## 二、各平台深度分析

### 2.1 Codex (GPT-5 系列) — 自主性之王

**核心特征**：Fire-and-forget 模式，73.6% session 只有 1 条用户消息。

| 指标 | 值 |
|---|---|
| 中位 Turns/Session | 60 |
| 中位 Steering Ratio | **2.2%** (最低) |
| 最长自主间隔 | 194 events 无干预 |
| Burst Pattern | 84% sessions |
| >1000 turns | 10 sessions |

**长任务成败**（Top 15 mega sessions）：
- COMPLETED: 2/15 (13%)
- STALLED: 10/15 (67%)
- EXPLORATORY: 3/15 (20%)

**成功模式**：单一目标 + 短指令 + subagent 协作 + 验证门
**失败模式**：范围蔓延 (40%) > 环境泥潭 (27%) > 方向摇摆 (13%) > 信息黑洞 (13%) > 空转 (7%)

**关键发现**：
- 0-5% steering ratio 是最优自主区间
- 31+ 条用户消息的长任务**无一完成**
- "Mission Directive" 军事风格简报触发最长自主执行
- /goal 模式有 5 阶段验证门，是最佳执行模式

### 2.2 Claude Code — 双模态工具人

**核心特征**：88% 闪电任务（≤2 turns），12% 深度马拉松（1000+ turns）。

| 指标 | 值 |
|---|---|
| 中位 Turns | 2 |
| 工具密度 | **2.47 tools/user** (最高) |
| 中位 Steering Ratio | 50% (最高) |
| 最大 Session | 2,588 turns |

**模式 A（闪电）**：
```
用户: "帮我改一下这个文件"
CC: [改完]
```

**模式 B（马拉松）**：
```
用户: [874 条消息, 平均 893 chars]
CC: [2,004 turns, 2,494 tool_use, 0 条文本回复]
```

**关键发现**：
- 深度模式下 assistant 文本输出为 0——信息黑洞
- 擅长多层根因诊断（503 三层诊断）
- 长任务 0% COMPLETED

### 2.3 Antigravity — 编排器而非助手

**核心特征**：93.5% mixed session（交易+工程），71.4% 自纠错率，373 次跨 agent dispatch。

| 指标 | New (Gemini 3.1) | Legacy (Claude 4.6) |
|---|---|---|
| Sessions | 54 | 100 |
| 平均 Turns | 270 | 288 |
| Steering Ratio | 9.5% | 11.4% |
| 自纠错率 | 68.5% | 70% |
| 工具密度 | **1.86** tools/user | 1.43 tools/user |
| MCP 调用 | 12 | **37** |

**工具 Top 5**：run_command (36.8%), view_file (16.6%), task_boundary (8.4%), grep_search (6.6%), command_status (5.1%)

**独特行为**：
- 91.6% session 有 Tachi memory 检索
- 最长自主链：119 次连续 tool calls
- 用户用 radare2 patch 了二进制移除输出限制
- 373 次 dispatch Codex/Claude Code 做子任务

### 2.4 OpenCode — 多模型聚合平台

**核心特征**：2.3GB SQLite，6 个 provider，丰富的元数据（cost/tokens/TODO/subagent）。

| 指标 | 值 |
|---|---|
| Main Sessions | 247 |
| Subagent Sessions | 537 |
| 总 Tokens | 426M |
| 总 Cost | $303.90 |
| TODO 完成率 | **80.4%** (656/816) |

**模型表现**：

| 模型 | Sessions | TODO 完成率 | Subagent 率 | Cost |
|---|---|---|---|---|
| Claude Sonnet 4.6 | 3 | **91.7%** | 33% | $0 |
| Kimi K2P6 | 2 | **100%** | 50% | $0 |
| GLM 5.1 | 2 | **100%** | 1000% | $0.05 |
| GPT-5.5 (Copilot) | 11 | 69.6% | 118% | $0 |
| Claude Opus 4.7 | 5 | 51.9% | 320% | $0 |
| DeepSeek V4 Pro | 3 | 47.8% | 600% | $36 |
| Qwen 3.7 Max | 3 | **20%** | **900%** | **$189** |

### 2.5 Hermes — 交易+工程混合体

**核心特征**：85.1% mixed session，MCP 是骨架，架构决策嵌入交易对话。

| 指标 | 值 |
|---|---|
| 中位 Turns | 21 |
| 主力模型 | GLM-5.1 (73%) |
| MCP 提及率 | **81.8%** |
| delegate_task | 11.6% sessions |

**架构决策在交易中诞生**：
- Tachi vs Hermes Memory 权限边界——讨论持仓时决定
- Subagent delegation 模式——扫描股票时设计
- Async research dispatch——跑回测时构思

### 2.6 Windsurf — Memory 驱动的长上下文

**核心特征**：Tachi Memory 注入 + 多模型混用 + 最长 session 时长。

| 指标 | 值 |
|---|---|
| 中位 Turns | 59 |
| Memory 注入 | 4-18 条/session |
| 最大 Session | 1,098 turns (22MB) |
| 多模型混用 | claude-opus-4-6/4-7 + gpt-5-5 |

---

## 三、Claude 4.6 vs Gemini 3.1 — Antigravity 内对比

### 3.1 量化对比

| 维度 | Claude 4.6 | Gemini 3.1 | 胜者 |
|---|---|---|---|
| Steering Ratio | 0.184 | 0.183 | **平手** |
| Tool/User Ratio | 1.43 | **1.86** | Gemini |
| 用户消息长度 | 109 chars | **206 chars** | Gemini |
| 自纠错总量 | **697** | 417 | Claude |
| 领域术语密度 | **43.8/session** | 38.8/session | Claude |
| Retry/Giveup 比 | **0.35** | 0.23 | Claude |
| MCP 调用 | **37** | 12 | Claude |
| Dispatch prompt 质量 | **3000+ chars** | 中等 | Claude |

### 3.2 风格对比

| 维度 | Claude 4.6 | Gemini 3.1 |
|---|---|---|
| **自纠错指向** | 系统根因（"算法被 X 糊住了"） | 自身认知（"注意力吸附效应"） |
| **表达方式** | 人格化 + 代码细节 | 结构化诊断（表格+状态码） |
| **说人话** | 表演过度（"Hapi 会生气的"） | **更像正常分析师** |
| **错误处理** | 更坚持（retry 0.35） | 更容易换路线 |
| **编排主动性** | 主动组装 3000+ char prompt | 简洁 dispatch |

### 3.3 核心结论

- **Claude 是更有直觉的参谋**：知道代码为什么这么写、错误根因在哪、怎么给下游写好 prompt
- **Gemini 是更有纪律的参谋**：结构化输出更好、工具使用更密集、说人话不装
- **理想编排大脑** = Gemini 的表达纪律 + Claude 的诊断深度

---

## 四、国产模型 vs Codex/GPT-5.5

### 4.1 量化对比

| 指标 | Qwen 3.7 | DeepSeek V4 | GLM 5.1 | Kimi K2P6 | GPT-5.5 |
|---|---|---|---|---|---|
| Sessions | 3 | 3 | 2 | 2 | 15 |
| 总 Tokens | 15.3M | 5.2M | 1.4M | 1.5M | 8.8M |
| 总 Cost | **$189** | $36 | $0.05 | $0 | $0 |
| TODO 完成率 | **20%** | 47.8% | **100%** | **100%** | 69.6% |
| Subagent 率 | **900%** | 600% | 1000% | 50% | 118% |
| 用户消息长度 | 312c | 144c | 183c | 38c | **1,027c** |
| 短指令占比 | 79% | 78% | 91% | 70% | **19%** |
| 停滞 | 几乎不 | 中等 | 频繁 | **零** | **最严重** |
| 完成状态 | 0/3 done | 2/3 done | **2/2 done** | **2/2 done** | 10/15 done |

### 4.2 交互风格

| 模型 | 风格 | 典型行为 |
|---|---|---|
| **Qwen 3.7** | 对话式迭代 | 用户当思考伙伴，快速短指令+偶尔超长粘贴。唯一触发脏话的模型 |
| **DeepSeek V4** | 给上下文然后放手 | 16.5h 过夜 session，规划很细但零执行 |
| **GLM 5.1** | 推着走 | 6 次"继续"，91% 短指令，但推着走完了 |
| **Kimi K2P6** | 纯任务委派 | 最短消息(38c)，零停滞零沮丧，最干净 |
| **GPT-5.5** | Mega-prompt + auto-continue | 1-3 条超长 prompt，15 次 auto-continue，最不交互 |

### 4.3 成本效率

| 模型 | 总 Cost | 每完成 TODO | 评价 |
|---|---|---|---|
| Kimi K2P6 | $0 | $0 | **免费 + 100% 完成** |
| GLM 5.1 | $0.05 | $0.008 | **最便宜** |
| GPT-5.5 | $0 | $0 | 免费但停滞严重 |
| DeepSeek V4 | $36 | $3.25 | 中等 |
| Qwen 3.7 | **$189** | **$94.5** | **最贵最低效** |

### 4.4 核心结论

- **国产模型适合交互式短任务**（用户在场推着走）
- **GPT-5.5 适合 fire-and-forget 长任务**（但需要 mega-prompt 前置上下文）
- **Kimi K2P6 是黑马**：零停滞、零成本、100% 完成
- **Qwen 最贵最低效**：$189 cost，20% TODO 完成，900% subagent 率
- **GLM 最便宜**：$0.05 完成所有任务

---

## 五、跨模型通用发现

### 5.1 长任务成败铁律

| # | 铁律 | 证据 |
|---|---|---|
| 1 | 单一目标 + 短指令 = 完成 | db.rs 拆分：33 条短消息，COMPLETED |
| 2 | 多目标 + 频繁改方向 = 停滞 | 10/15 STALLED session 有方向变更 |
| 3 | Subagent 协作提高完成率 | COMPLETED session 都 dispatch 了 sidecar |
| 4 | 成功标记 > 失败标记 = 正循环 | COMPLETED 比率 1.8，STALLED 0.3 |
| 5 | 0-5% steering ratio 最优 | 超过 10% 的长任务全部停滞 |
| 6 | 31+ 条用户消息 = 必死 | 无一完成 |

### 5.2 架构铁律

| # | 铁律 | 证据 |
|---|---|---|
| 7 | Agent 写 scratch 脚本 = 接口缺失 | CLI 只输出人类可读报告 |
| 8 | MCP 必须是 thin proxy | fat adapter 导致业务逻辑重复 |
| 9 | 数据库边界必须代码强制 | hapi.db 被 710K 行实验数据污染 |
| 10 | Python/Rust 参数分歧 = 静默偏差 | chan theory 默认参数不一致 |
| 11 | Silent degradation 是最差失败模式 | Vault 凭证缺失不报错 |

### 5.3 模型选择矩阵

| 场景 | 推荐模型 | 原因 |
|---|---|---|
| 单一目标重构 | **Codex /goal** | 5 阶段验证门，COMPLETED |
| 交互式探索 | **Antigravity (Gemini)** | 说人话，结构化，自纠错 |
| 深度代码审阅 | **Claude Code** | 工具密度最高 |
| 便宜跑杂活 | **GLM 5.1** | $0.025/session |
| 干净短任务 | **Kimi K2P6** | 零停滞，零成本 |
| Fire-and-forget | **GPT-5.5 (Copilot)** | 免费，自主性强 |
| 编排多 agent | **Antigravity (Claude)** | 37 次 dispatch，prompt 质量最高 |
| 交易+工程混合 | **Hermes (GPT-5.5)** | MCP 骨架 + delegate_task |
| 大量代码阅读 | **Qwen 3.7 Max** | 1M 上下文，但贵 |

---

## 六、系统架构蒸馏

### 6.1 三层架构

```
Agent 层 (Hermes / Codex / Claude / GLM / Kimi / Gemini)
    │ MCP / delegate_task / JSON-RPC
    ▼
hapi-edge (Go MCP Server, thin proxy)
    │ JSON-RPC
    ▼
hapi-server (Python FastAPI, LS1 编排)
    │
    ├── warpcore (Rust, 25,014 行)
    ├── hapi.db (SQLite, WAL)
    └── autoresearch.db (SQLite, 独立)
        │
        ▼
radar_daemon (Rust, 纯 I/O, 3-Slot 轮换)
    │
    ▼
rust_gateway.db (SQLite, 单写者)
```

### 6.2 三库隔离

| 数据库 | 职责 | Writer | 写入模式 |
|---|---|---|---|
| hapi.db | 运营真相（持仓/交易/日志） | hapi-server | WAL |
| rust_gateway.db | 市场数据湖 | radar_daemon **唯一** | 单写者 |
| autoresearch.db | 实验数据 | autoresearch_lab | 独立 |

### 6.3 MCP Thin Proxy 原则

Agent 写 scratch 脚本不是 agent 笨，是**缺少结构化工具接口**。MCP 必须：
1. 不直接 import engine
2. 不直接读 SQLite
3. 不内部执行 LS1
4. 工具设计为 intent-level：hunt, guard, batch_snapshot, portfolio

---

## 七、调试 Playbook

### 7.1 503 三层根因

```
层 1: Google 端点 Bug (daily-cloudcode-pa → wrapper script 替换)
层 2: 容量耗尽 (MODEL_CAPACITY_EXHAUSTED → 无解)
层 3: IPv6 (路由器 DHCPv6 仍在广播 → 路由器层禁用)
```

### 7.2 Python/Rust 静默偏差

```
Python: divergence_rate=inf, max_bs2_rate=0.9999, macd_algo="peak"
Rust:   divergence_rate=1.0, max_bs2_rate=0.618, macd_algo="area"
→ 不报错但结果不一致
```

### 7.3 WindClaw 逆向

```
app.asar 是归档格式不能 string replace
→ 环境变量注入 (CLAWX_AIGW_BASE_URL) 是最安全的 runtime override
```

---

## 八、与 Amp 设计的差距

| Amp 设计 | 当前最佳实践 | 差距 |
|---|---|---|
| 9 套 prompt 对应不同模型 | 通用 prompt | **大** |
| 双通道输出 (commentary + final) | Claude Code 0 文本 | **大** |
| Subagent 纪律 | Codex sidecar | **中** |
| Verification 风险分级 | 全量测试 | **中** |
| TODO 工具级持久化 | OpenCode SQLite (80.4%) | **中** |
| Scaffold customization | 无 | **大** |

**最值得借鉴的 3 个设计**：
1. Prompt-Model 联合优化
2. 双通道输出（进展 + 结论）
3. Scope lock（防范围蔓延）

---

## 九、蒸馏产出清单

### 9.1 结构化知识

| 知识域 | 条目数 | 目录 |
|---|---|---|
| Trading 知识 | 46 条 | `trading_knowledge/` (7 个子分类) |
| Coding Session 索引 | 576 sessions | `coding_knowledge/scan_index.json` |
| Antigravity 统计 | 154 sessions | `antigravity_stats/` |
| OpenCode 元数据 | 247 sessions | `opencode/json/_summary.json` |

### 9.2 分析报告

| 报告 | 文件 | 行数 |
|---|---|---|
| Amp 逆向分析（总报告） | `00_agent_model_analysis_report.md` | 449 |
| Amp Deep Autonomous Prompt | `01_deep_autonomous_prompt.md` | 80 |
| Amp Deep Fallback Prompt | `02_deep_fallback_prompt.md` | 150 |
| Amp Pair Programming Prompt | `03_pair_programming_prompt.md` | 103 |
| Amp Subagent 架构 | `04_agent_architecture.md` | 147 |
| Amp Prompt 路由 & 模型 | `05_prompt_routing_and_models.md` | 182 |
| Amp 工程亮点 | `06_engineering_highlights.md` | 184 |
| Compaction/TODO/Handoff 对比 | `07_compaction_todo_handoff_comparison.md` | 141 |
| 国产 vs 前沿模型 | `08_domestic_vs_frontier_models.md` | 261 |
| **Coding Agent 深度分析** | `09_coding_agent_analysis_report.md` | **1,316** |
| Claude vs Gemini in Antigravity | `10_claude_vs_gemini_in_antigravity.md` | 197 |
| 国产模型 vs Codex | `11_domestic_vs_codex.md` | 233 |
| **全量蒸馏报告（本文）** | `12_full_session_distillation_report.md` | — |

### 9.3 分析脚本

| 脚本 | 用途 |
|---|---|
| `extract_coding_knowledge.py` | Coding session 扫描+分类 |
| `export_opencode.py` | OpenCode SQLite → JSON 导出 |
| `quant_analysis.py` | 量化统计分析 |
| `analyze_antigravity.py` | Antigravity 工具/模型分析 |
| `session_comparison.py` | Claude vs Gemini 对比 |
| `extract_sessions.py` | 特定 session 深度提取 |

---

## 十、总结

### 10.1 数据规模

- **2,842 sessions** 横跨 7 个 Agent 平台
- **~278,000 turns** 的人机交互记录
- **1.1GB** 原始 JSON 数据
- **4 个月** 的时间跨度 (2026-01 ~ 2026-05)

### 10.2 核心发现

1. **Codex 自主性最强**但长任务完成率仅 13%——范围蔓延是头号杀手
2. **Claude Code 工具密度最高**但深度模式不汇报推理——信息黑洞
3. **Antigravity 是编排器不是助手**——373 次跨 agent dispatch，71.4% 自纠错
4. **Gemini 说人话，Claude 有直觉**——理想编排 = Gemini 表达 + Claude 诊断
5. **国产模型适合交互短任务**——Kimi 是黑马，Qwen 最贵最低效
6. **GPT-5.5 适合 fire-and-forget**——但每个 session 都需要 auto-continue
7. **单一目标 + 短指令 + subagent = 完成**——跨所有模型通用
8. **0-5% steering ratio 是最优自主区间**——超过 10% 必停滞
9. **MCP 必须是 thin proxy**——fat adapter 是架构腐化
10. **Silent degradation 是最差失败模式**——必须 fail loudly

### 10.3 下一步

- [ ] Coding 知识结构化蒸馏（仿 trading_knowledge 格式）
- [ ] 长任务 prompt 模板（防范围蔓延 + 验证门 + 进展播报）
- [ ] 模型路由策略自动化（根据任务类型选模型）
- [ ] Amp 设计落地：双通道输出 + Scope lock + Prompt-Model 联合优化

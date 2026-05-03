# Tachi 图书馆架构设计

> 日期: 2026-05-03
> 状态: 设计中
> 参考: [Karpathy LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f), Superpowers brainstorming skill, OpenClaw eval/反思, [Google A2A](https://google.github.io/A2A/)

## 顶层定位：Agentic OS

Tachi 不只是一个 MCP server，它是一个 **Agentic OS** — Agent 的操作系统。

```
传统 OS                          Tachi (Agentic OS)
────────                        ──────────────────
文件系统                         Notes / Wiki / Skill
系统调用                         MCP (tachi_search / tachi_save / ...)
进程管理                         Dispatch (创建/调度/监控 Agent 进程)
动态链接器                       assemble_prompt (拼上下文给进程)
/proc + metrics                 Eval (性能监控)
GC / defrag                     Reflect (自我优化)
IPC                             A2A (进程间通信)
```

Agent 是进程，Tachi 是操作系统。操作系统不替进程思考，只提供基础设施。

## 五层架构总览

```
┌─ 知识层 ─────────────────────────────┐
│  Notes / Wiki / Skill                │
│  Obsidian 人类可读 + DB Agent 可搜    │
├─ 通信层 ─────────────────────────────┤
│  MCP (Agent↔Tool) + A2A (Agent↔Agent)│
├─ 算力层 ─────────────────────────────┤
│  Embedding / 前台 LLM / Foundry LLM  │
├─ 反馈层 ─────────────────────────────┤
│  Eval → 反思 → Agent Card → 路由优化  │
├─ 能力层 ─────────────────────────────┤
│  brainstorm → plan → implement →     │
│  review → debug → distill (5-6 skill)│
└──────────────────────────────────────┘
```

每层都有明确的协议/标准：
- **知识** = Obsidian markdown（人读）+ SQLite FTS/向量（Agent 搜）
- **通信** = MCP + A2A（开放标准，有生态）
- **算力** = 3 层 LLM（见 `LLM-双层架构重构计划.md`）
- **反馈** = eval_ledger + 反思 cron（OpenClaw 模式）
- **能力** = Hub Skill（核心 5-6 个 + 领域扩展，见 `Skill-全景清单.md`）

## 知识层：图书馆比喻

把 Tachi 比作图书馆，三层知识 + 两类 Worker + 统一检索。

```
外部采购                    馆内活动              知识层
───────                    ──────              ──────
Antigravity artifact ──┐
dispatch 结果         ──┤
tachi_web_search     ──┤   ┌──────────┐      ┌→ Wiki (百科全书)
论文搜索 (待建)      ──┼─→ │  Notes   │→ Worker ─┤
推特/KOL (待建)      ──┤   │  阅览桌  │      └→ Skill (工具书架)
                       │   └──────────┘
Brainstorm ────────────┘     ↑  内生产出
                             │
                        tachi_search (横跨三层检索)
```

## 三层知识

| 层 | 类比 | 内容 | 写入者 | 读取者 | 持久性 | 人类可读 |
|---|---|---|---|---|---|---|
| **Notes** | 阅览桌 | 原始素材、草稿、报告、brainstorm 输出 | 任何人/Agent | Worker + Agent | 中期 | ✅ Obsidian |
| **Wiki** | 百科全书 | 编译后的永久知识、架构决策、runbook | 后台 Worker | 所有 Agent | 永久 | ✅ Obsidian |
| **Skill** | 工具书架 | 可执行的 SOP、能力、工具链 | 前台 Worker | 所有 Agent | 永久 | 代码+文档 |

### Notes 层设计

文件系统为主，DB 索引为辅：

```
~/.tachi/notes/
├── antigravity/          ← Antigravity artifact 自动同步
│   ├── analysis/
│   ├── design/
│   └── task/
├── brainstorm/           ← skill:brainstorm 输出
├── dispatch/             ← dispatch 结果/轨迹
├── handoff/              ← Agent 交接备忘
├── inbox/                ← 随手记/临时素材
└── .index.json           ← 自动生成的索引
```

- **文件系统**：Obsidian 直接打开 `~/.tachi/notes/` 即可浏览
- **DB 索引**：每个 Note 在 memory DB 中有索引条目（category=note），`tachi_search` 可搜到
- **Git**：整个 notes 目录可 git 管理，天然版本控制
- **不污染 Wiki**：Notes 和 Wiki 检索空间独立

### Wiki 分区

```
/wiki/
├── runbook/        ← 操作手册（怎么做）
├── decision/       ← 架构决策 + 铁律（为什么）
├── analysis/       ← 深度分析报告（是什么）
└── reference/      ← 竞品对比、技术调研（参考）
```

### Skill（已有）

Hub 注册的可执行技能，通过 `hub_discover` / `run_skill` 调用。

## 两类 Worker

### 后台 Worker（蒸馏员）

扩展现有 `distill_trajectory` 框架：

- 定期扫描 Notes 目录
- 识别高价值内容（信息密度 + 可操作性评分）
- 提取核心知识 → 创建/更新 Wiki 页
- 维护交叉引用（Karpathy: "一次 ingest 触达 10-15 页"）
- 标记已蒸馏的 Note（frontmatter `distilled: true`）
- 不删除 Notes 原文，保留溯源

### 前台 Worker（分类员/馆员）

新增：

- 从 Wiki + Notes 中识别可复用模式
- 提炼为 Skill 注册到 Hub（`hub_register`）
- 例：V8 诊断系列 → 提炼出 `skill:scoring-engine-audit` SOP

## 完整工作流

### Brainstorm → Implement 闭环

```
用户提出想法
    ↓
┌─ Hub: skill:brainstorm ──────────────────┐
│  拉 Wiki/Notes/Search 做上下文            │
│  发散 → 收敛 → spec                      │
│  spec 写入 Notes/brainstorm/              │
└──────────────────────────────────────────┘
    ↓
┌─ Hub: skill:writing-plans ───────────────┐
│  spec → 拆解为可执行步骤（每步 2-5 分钟）  │
│  plan 写入 Notes/brainstorm/              │
│  执行选项：subagent-driven / inline        │
└──────────────────────────────────────────┘
    ↓
┌─ tachi_dispatch (确定性编排，无需路由 LLM)─┐
│  assemble_prompt:                        │
│    skill_match → 匹配 Superpowers skill  │
│    recall → 拉相关 Wiki/Notes 上下文      │
│    avoidance → 查历史踩坑                 │
│    拼装完整 prompt                        │
│  inject_tachi_mcp → delegate 可调        │
│    tachi_search / tachi_save 按需拉 Wiki  │
│  subprocess → claude/codex 执行           │
│  完成后自动 tachi_complete                │
└──────────────────────────────────────────┘
    ↓
┌─ 自动反馈 ───────────────────────────────┐
│  eval 记录（质量/耗时/避坑）              │
│  → 优化下次 prompt assembly               │
└──────────────────────────────────────────┘
    ↓
┌─ Worker (后台自动) ──────────────────────┐
│  蒸馏员: Notes → Wiki（永久知识）         │
│  分类员: 可复用模式 → 新 Skill            │
└──────────────────────────────────────────┘
```

### Dispatch 编排 = 确定性代码

编排不需要 LLM — 全是确定性操作：

```rust
fn assemble_prompt_v2(params, server) -> String {
    let skill = match_skill(&params.task);                    // 关键词匹配
    let context = recall_relevant(server, &params.task);     // SQL + 向量
    let avoidance = get_avoidance_notes(server, &skill);     // 查历史踩坑
    format!("{skill}\n\n{context}\n\n{avoidance}\n\n{params.task}")
}
```

Delegate Agent 通过 `--mcp-config` 挂载 Tachi MCP（`inject_tachi_mcp` 已实现），自己按需搜 Wiki。不需要把 Wiki 全塞进 prompt — Agent 自己有脑子。

### 信息采购管道

| 来源 | 实现方式 | 状态 |
|---|---|---|
| 内部记忆 | `tachi_search` | ✅ 已有 |
| 互联网 | `tachi_web_search` (Hub capability) | ✅ 已有 |
| Antigravity artifact | backfill 脚本 + 文件监控 | 🔨 待建 |
| dispatch 结果 | `tachi_complete` → Notes | 🔨 待建 |
| 论文搜索 | Hub capability (Semantic Scholar / arXiv) | 📋 远期 |
| 推特/社媒 | Hub capability (Twitter API) | 📋 远期 |
| Brainstorm | Hub `skill:brainstorm` → Notes | 🔨 待建 |

## 与 Karpathy LLM Wiki 的对比

| 维度 | Karpathy | Tachi 图书馆 |
|---|---|---|
| 三层 | Raw Sources → Wiki → Schema | Notes → Wiki → Skill |
| 操作 | Ingest / Query / Lint | Ingest + Brainstorm + Dispatch / Search / Lint |
| 索引 | index.md + log.md | FTS + 向量搜索 + DB 索引 |
| 人类可读 | Obsidian 浏览文件系统 | Notes = 文件系统（Obsidian）；Wiki = DB + browse |
| 执行能力 | 无（纯知识库） | Skill + Dispatch（可执行） |
| 协作模型 | 单人 + 单 LLM | 多 Agent 协作（主 Agent + delegate） |
| 知识复利 | Query 回填 Wiki | Brainstorm → Notes → Worker → Wiki/Skill |

Tachi 的核心优势：**不止是知识库，还是执行引擎**。从 brainstorm 到 implement 全闭环，知识和行动统一在同一个系统中。

## 通信层：MCP + A2A 双协议栈

### 现状

- **MCP** (Model Context Protocol) — Agent ↔ Tool，纵向调用。已有。
- Tachi 本身就是 MCP server，暴露 `tachi_search` / `tachi_save` / `tachi_dispatch` 等工具。

### A2A (Agent-to-Agent Protocol)

Google 开放标准，用于 Agent 间对等通信。补充 MCP 缺失的横向协作能力。

```
MCP:  Agent ──调用──→ Tool（上下级）
A2A:  Agent ←─协作─→ Agent（对等）
```

#### A2A 核心概念映射

| A2A 概念 | Tachi 对应 | 当前实现 | A2A 标准化后 |
|---|---|---|---|
| **Agent Card** | 模型能力画像 | 无 | eval 数据驱动的 JSON 自描述 |
| **Task** | dispatch 任务 | `tachi_dispatch` (subprocess) | A2A Task 生命周期管理 |
| **Streaming** | 实时进度 | 无（只有 Watchdog 兜底） | SSE 实时流 |
| **Push Notification** | 事件通知 | Kanban 轮询 | 事件驱动，Agent 完成 → push |

#### Ghost 能力的 A2A 复现

Ghost 系统已删除，但三个独有能力通过 A2A 标准化复活：

| Ghost 独有 | 问题 | A2A 替代 |
|---|---|---|
| Topic pub/sub + 游标持久化 | 自研协议，无生态 | A2A Streaming (SSE) + Push Notification |
| Whisper 点对点私密消息 | 自研加密通道 | A2A Task-based messaging |
| 自动反思规则提取 | 跟 Ghost 人格绑定 | A2A Agent Card + eval 动态更新 |

#### Agent Card 示例

```json
{
  "name": "codex-kimi-k2.6",
  "description": "Kimi K2.6 via Codex CLI",
  "capabilities": ["code-review", "architecture", "refactor"],
  "task_types_best": ["code-analysis", "deep-research"],
  "avg_quality_score": 8.5,
  "avg_duration_sec": 120,
  "profile": "operate",
  "eval_sample_count": 42,
  "last_updated": "2026-05-03"
}
```

dispatch 时查 Agent Card → 自动选最佳 Agent → eval 更新 Card → 越用越准。

## 反馈层：Eval + 反思系统

参考 OpenClaw 的 eval + 灵魂反思架构，面向 Tachi dispatch 的数据驱动路由优化。

### Eval 记录

每次 `tachi_dispatch` → `tachi_complete` 自动记录：

```jsonl
{"dispatch_id":"20260503T075037Z","model":"kimi-k2.6","task_type":"code-analysis","duration_sec":131,"quality_score":8,"status":"ok","agent":"codex"}
{"dispatch_id":"20260503T080000Z","model":"glm-5.1","task_type":"distill","duration_sec":45,"quality_score":9,"status":"ok","agent":"claude"}
```

字段：`dispatch_id`, `model`, `task_type`, `duration_sec`, `quality_score`(1-10), `status`(ok/failed/timeout), `agent`

### 反思 Cron

#### 每日轻量反思

- 触发：每天定时 / `tachi_complete` 后自动
- 输入：当天 eval 记录 + importance >= 0.5 的记忆
- 流程：
  1. 经历分级：routine / notable / pivotal
  2. 如果有 notable/pivotal → 写入 `notes/reflections/YYYY-MM-DD.md`
  3. 深度反思：学到了什么、与现有认知是否一致、盲点
  4. Actionable Items：具体变更建议 + 触发条件 + 置信度

#### 每周进化审视

- 输入源：
  - 本周所有反思文件
  - eval 记录（模型表现统计）
  - Agent Card（当前能力画像）
  - Wiki 中的架构决策（检查过时信念）
- 分析维度：
  - **路由优化**：eval 数据是否建议调整模型选择
  - **能力漂移**：某模型在某类任务上持续低分/高分
  - **过时信念**：Wiki 中已不成立的事实
  - **新增认知**：反思中反复出现但未固化的行为模式
- 输出：如有提案 → `notes/proposals/YYYY-MM-DD-weekly.md`

### Dispatch 路由优化闭环

```
dispatch 任务
    ↓
查 Agent Card（模型能力画像）
    ↓ 自动选 model + profile
A2A Task → delegate agent 执行
    ↓
tachi_complete → eval 记录
    ↓
反思 cron 分析 eval 趋势
    ↓
更新 Agent Card → 下次 dispatch 更准
```

### Eval 重心

在 Claude Code / Codex CLI 这类成熟工具里，模型选择不是问题（不像 OpenClaw 自研 harness 有截断/超时/格式问题）。eval 的真正价值是优化 **prompt assembly 质量**：

- 这个 task 做得好不好 → 下次注入更好的上下文
- 哪类 prompt 容易翻车 → 避坑指南
- 哪个 skill 匹配度高 → 自动 skill 选择
- 耗时/成本追踪 → 性价比优化

## 知识复利飞轮

```
dispatch → Agent 挂 Tachi MCP，按需搜 Wiki
    → Agent 干活，越搜越精准
    → tachi_complete → eval 记录
    → Worker 蒸馏 → Wiki 更丰富
    → 下次 dispatch 的 Agent 搜到更好的上下文
    → 循环，越转越快
```

Wiki 越丰富，Agent 越聪明。Agent 越聪明，产出越好。产出越好，蒸馏出的 Wiki 越有价值。

## 能力层：核心 Skill

详见 `Skill-全景清单.md`。

### Superpowers 14 Skill 流水线

对着 [obra/superpowers](https://github.com/obra/superpowers/tree/main/skills) 的模式流开发，14 个 skill 已全部在 Hub 注册（`skill:superpowers-*`）：

```
brainstorming                    ← 设计入口 (HARD GATE: 批准前禁写代码)
    ↓
writing-plans                    ← 拆解计划 (每步 2-5 分钟, TDD, 无占位符)
    ↓
┌─ subagent-driven-development   ← 逐 task 派 fresh agent (推荐)
│  dispatching-parallel-agents   ← 并行派多个 agent
└─ executing-plans               ← 当前 session 内联执行
    ↓
test-driven-development          ← TDD 铁律 (贯穿执行)
using-git-worktrees              ← 隔离开发环境
    ↓
requesting-code-review           ← 派 reviewer agent
receiving-code-review            ← 处理 review 反馈
    ↓
verification-before-completion   ← 完成前验证
finishing-a-development-branch   ← 收尾合并
    ↓
systematic-debugging             ← 出问题时调试
    ↓
writing-skills                   ← 元 skill: 造新 skill
using-superpowers                ← 元 skill: 怎么用这套体系
```

Superpowers = 能力层骨架。Tachi 在此基础上多：知识持久化、eval 反馈、MCP 上下文注入。

### 核心 Skill 算力归属

| 阶段 | Skill | 算力归属 |
|---|---|---|
| 设计 | `brainstorm` | 前台 LLM |
| 规划 | `writing-plans` | 前台 LLM |
| 执行 | `executing-plans` / `subagent-driven-dev` | Dispatch → delegate |
| 审查 | `review` / `verification-before-completion` | 前台 LLM / Codex |
| 调试 | `systematic-debugging` / `investigate` | 前台 LLM |
| 发布 | `ship` | delegate |

其余 100+ skill 按需加载，领域特定的从 Notes/Wiki 蒸馏产出。

## 实施优先级

### P0 — Notes 层基础（本周）
1. 创建 `~/.tachi/notes/` 目录结构
2. 28 个 Antigravity artifact backfill 到 `notes/antigravity/`
3. `tachi_save` 支持 `scope=note` 写入 Notes 目录 + DB 索引
4. `tachi_search` 支持搜索 Notes 层

### P1 — Worker MVP（下周）
5. 后台 Worker：扫描 Notes → 蒸馏到 Wiki（扩展 foundry worker）
6. Brainstorm skill 接入：Hub 注册 `skill:brainstorm`，输出写入 Notes

### P2 — 闭环（两周内）
7. `tachi_dispatch` 注入 `skill:implement-task` 给 delegate
8. 前台 Worker：识别可复用模式 → 注册新 Skill
9. Wiki 分区重构 + 交叉引用维护

### P3 — Eval + 反思（三周内）
10. `tachi_complete` 自动记录 eval（model, task_type, quality_score, duration）
11. Agent Card 数据结构 + 存储
12. dispatch 路由优化：查 Agent Card 自动选 model + profile
13. 每日轻量反思 cron → `notes/reflections/`
14. 每周进化审视 cron → `notes/proposals/`

### P4 — A2A 通信（远期）
15. A2A Agent Card 标准化（兼容 Google A2A spec）
16. A2A Task 生命周期替代 subprocess dispatch
17. SSE Streaming 实时进度（替代 Watchdog 轮询）
18. Push Notification 事件驱动（替代 Kanban 轮询）

### P5 — 采购扩展（远期）
19. 论文搜索 Hub capability
20. 推特/社媒监控
21. Antigravity artifact 实时监控（文件 watcher）

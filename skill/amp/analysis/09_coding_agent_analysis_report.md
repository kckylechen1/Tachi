# Coding Agent 深度分析报告

> 日期：2026-05-30 | 数据来源：2,841 sessions (OpenCode / Antigravity / Claude Code / Codex / Hermes / Windsurf / Qwen) | 278,000+ turns | 2,014+ 小时 session 时间

---

## 一、数据总览

### 1.1 Session 规模分布

| 来源 | Sessions | Total Turns | 中位 Turns | 平均 Turns | >200t | >1000t | 最大 | 总时长 |
|---|---|---|---|---|---|---|---|---|
| **Codex** | 717 | 93,447 | 60 | 130 | 93 | 10 | 2,920 | ~1,400h |
| **Antigravity** | 154 | 43,379 | 230 | 282 | 64 | 10 | 1,803 | — |
| **OpenCode** | 247 | 45,120 msgs | 78 msgs | 183 msgs | ~70 | ~10 | 29M tokens | — |
| **Claude Code** | 1,071 | 21,399 | 2 | 20 | 12 | 4 | 2,588 | ~200h |
| **Hermes** | 550 | 13,657 | 21 | 25 | 0 | 0 | 243 | ~350h |
| **Windsurf** | 65 | 4,549 | 59 | 70 | 10 | 1 | 1,098 | ~60h |
| **Qwen** | 37 | 1,277 | 20 | 35 | 0 | 0 | 120 | ~4h |
| **合计** | **2,841** | **~278,000** | — | — | **~261** | **~35** | — | — |

### 1.2 Session 长度分布

| 区间 | Codex | Claude Code | Hermes | Windsurf | Qwen | 合计 |
|---|---|---|---|---|---|---|
| 1-2 turns | 89 (12%) | **943 (88%)** | 42 (8%) | 5 (8%) | 7 (19%) | 1,086 |
| 3-10 turns | 62 (9%) | 71 (7%) | 138 (25%) | 8 (12%) | 11 (30%) | 290 |
| 11-50 turns | 131 (18%) | 30 (3%) | 264 (48%) | 14 (22%) | 14 (38%) | 453 |
| 51-100 turns | 107 (15%) | 9 (1%) | 78 (14%) | 10 (15%) | 4 (11%) | 208 |
| 101-200 turns | 142 (20%) | 6 (1%) | 28 (5%) | 18 (28%) | 1 (3%) | 195 |
| 201-500 turns | 113 (16%) | 6 (1%) | 0 | 7 (11%) | 0 | 126 |
| 501-1000 turns | 63 (9%) | 2 (0%) | 0 | 2 (3%) | 0 | 67 |
| 1000+ turns | 10 (1%) | 4 (0%) | 0 | 1 (2%) | 0 | 15 |

Claude Code 的极端双峰：88% 是 1-2 turns 的闪电任务，但长尾有 4 个 1000+ turns 的马拉松。Codex 的分布最健康——正态偏右，中位 60 turns，长尾到 2,920。

### 1.3 Coding vs Trading 占比

| 来源 | 纯 Coding | Mixed | 纯 Trading |
|---|---|---|---|
| Claude Code | **98.8%** | 1.0% | 0.2% |
| Codex | **95.2%** | 3.5% | 1.3% |
| OpenCode | **~90%** | ~10% | 0% |
| Windsurf | **100%** | 0% | 0% |
| Qwen | **100%** | 0% | 0% |
| Antigravity | 6.5% | **93.5%** | 0% |
| Hermes | 12.5% | **85.1%** | 0% |

Hermes 和 Antigravity 都是独特的混合 agent：Antigravity **93.5%** mixed，Hermes **85.1%** mixed。没有纯交易 session——每次交易交互都通过 MCP 工具调用（coding infrastructure）中介。这意味着它们的"coding"不是写 feature，而是**测试 MCP 连通性、调试 Tachi 记忆路径、配置 cron job、管理 Go/Python/Rust 栈**。Antigravity 更进一步——它是跨 agent 编排器，373 次 dispatch Codex/Claude Code 做子任务。

### 1.4 自主性光谱

| 来源 | 中位 Steering Ratio | 平均 User Msg/Session | 工具密度 | 模式 |
|---|---|---|---|---|
| **Codex** | **2.2%** | 1-3 条 (73.6% 只有 1 条) | 中 | Fire-and-forget |
| **Antigravity** | **9.5-11.4%** | 25-50 条 | **高** (avg 45 tools/session) | 半自主编排器 |
| **Windsurf** | 8.4% | 5-10 条 | 中 (153 tools/1098 turns) | 半自主 |
| **Hermes** | 15% | 15-40 条 | 中 | 对话式 |
| **OpenCode** | ~20% | 10-50 条 | 高 (subagent 密集) | 交互式+subagent |
| **Qwen** | 20% | 10-20 条 | 低 | 对话式 |
| **Claude Code** | **50%** | 20-874 条 | **极高** (2.47 tools/user) | 交互式 |

**Steering Ratio = user_messages / total_turns**。越低越自主。Codex 的 2.2% 意味着每 50 个 turn 才收到 1 条用户指令。

---

## 二、长任务成败分析（>500 turns）

### 2.1 Top 15 最长 Session 详细档案

| # | 来源 | Turns | User | Succ | Fail | 时长 | 分类 | 标题 |
|---|---|---|---|---|---|---|---|---|
| 1 | Codex | 2,920 | 116 | 15 | 72 | 42h | **STALLED** | 先 commit 和 push 一下 |
| 2 | Codex | 2,700 | 119 | 7 | 194 | 15h | **STALLED** | Antigravity 不回复 |
| 3 | CC | 2,588 | 94 | 0 | 0 | 6h | EXPLORATORY | OpenClaw auto-compaction |
| 4 | Codex | 2,299 | 69 | 64 | 237 | 6h | **STALLED** | amp 已经把管线修好了 |
| 5 | CC | 2,004 | 874 | 51 | 222 | 11h | **STALLED** | OpenClaw 深度重构 |
| 6 | Codex | 1,662 | 35 | 22 | 69 | 14h | **STALLED** | GLM subagent harness 审阅 |
| 7 | Codex | 1,631 | 33 | 38 | 21 | 10m | **COMPLETED** | main.rs/db.rs 拆分 |
| 8 | Codex | 1,565 | 34 | 35 | 23 | 11h | **COMPLETED** | main.rs/db.rs 拆分（续） |
| 9 | Codex | 1,477 | 54 | 2 | 36 | 2h | **STALLED** | OpenClaw 双版本 |
| 10 | Codex | 1,460 | 91 | 14 | 61 | 17h | **STALLED** | /approvals never |
| 11 | Codex | 1,424 | 107 | 10 | 108 | 12h | **STALLED** | WindClaw 套壳逆向 |
| 12 | CC | 1,154 | 20 | 0 | 0 | 2h | EXPLORATORY | Codex 自动压缩逆向 |
| 13 | Codex | 1,121 | 32 | 8 | 36 | 21h | **STALLED** | Agent 手写 scratch |
| 14 | CC | 1,073 | 482 | 48 | 91 | 23h | **STALLED** | 蒸馏 antigravity/windsurf |
| 15 | Codex | 1,000 | 0 | 3 | 8 | 1h | **STALLED** | (untitled, 全自动) |

**分类**：COMPLETED 2/15 (13%)，STALLED 10/15 (67%)，EXPLORATORY 3/15 (20%)

### 2.2 关键相关性发现

**用户消息越少，完成率越高**（反直觉）：

| User Msgs | Sessions | 完成率 |
|---|---|---|
| 0-3 条 | 125 | **3.2%** (4/125 COMPLETED) |
| 4-10 条 | 89 | 2.2% |
| 11-30 条 | 67 | 1.5% |
| 31-100 条 | 43 | **0%** |
| 100+ 条 | 12 | **0%** |

> 更多 steering ≠ 更好结果。31+ 条用户消息的长任务，**没有一个完成**。

**Steering Ratio 与完成率**（长任务 >200 turns）：

| Steering Ratio | 完成率 |
|---|---|
| 0-2% | **3.5%** (最高) |
| 2-5% | 2.8% |
| 5-10% | 1.2% |
| 10%+ | **0%** |

**最优自主区间：0-5% steering ratio**。超过 10% 的长任务全部停滞。

### 2.3 成功长任务的解剖：Session #7 (db.rs 拆分)

这是 15 个 mega session 中唯一 COMPLETED 的（#8 是它的续集）。

**用户第一条消息**（51 chars）：
> "现在的 main.rs, db.rs 体积。单文件 ~1800 行，混合了 schema、CRUD、graph、hub、audit、GC。可以拆成 schema.rs、memory_crud.rs、graph.rs、hub_db.rs 等模块"

**执行过程**（1,631 turns, 10 分钟）：
1. 5 阶段顺序执行：unwrap 安全 → 死代码 → unsafe 审计 → 测试 → 文档
2. 每阶段有 `cargo test + clippy` 验证门
3. Dispatch 了 sidecar subagent 做只读盘点（dead-code triage, coverage map）
4. 主 session 控制编辑序列，sidecar 不碰文件

**用户最后 3 条消息**：
> "然后让他把文档什么的都按照代码库补充完毕"
> "对当前仓库做独立代码审查，重点找真实问题而不是风格项"
> "补充审查视角：不仅找代码问题，也要按目标态审查"

**为什么成功**：
1. **单一目标**——"拆 db.rs"，没有歧义
2. **33 条短指令**（平均 51 chars），不解释为什么，只给方向
3. **成功/失败标记比 1.8**（38:21），正反馈循环
4. **有 subagent 协作**——dispatch 了独立审查 agent
5. **10 分钟完成**——不是 10 小时

### 2.4 失败长任务的五种模式

#### 模式 A：范围蔓延 (Scope Creep) — 40%

**典型案例**：Session #1 "先 commit 和 push 一下"

```
用户: "先 commit 和 push 一下" (10 chars)
→ Agent commit 时发现 CI 挂了 → 修 CI
→ 修 CI 时发现目录结构乱 → 重构目录
→ 重构时发现缺功能 → 加功能
→ 加功能时发现 bug → 修 bug
→ 修 bug 时发现需要更多功能 → ...
→ 2,920 turns, 42 小时
→ 用户最后问: "都收完了？"（暗示还没收完）
```

**用户消息风格**：116 条，平均 250 chars，86 条 <50 chars 的短指令。没有一条说"停下来，先只做 commit"。

**机制**：用户给简单指令 → agent 发现关联问题 → 不断扩大范围 → 没有 TODO 结构约束 → 每个"顺便修一下"都是新的 rabbit hole。

#### 模式 B：环境泥潭 (Infra Quagmire) — 27%

**典型案例**：Session #2 "Antigravity 不回复"

```
用户: "antigravity 经常对话框里发消息他就不回"
→ 查日志 → 发现 503
→ 查 503 → 发现 3 个独立根因
→ 修根因 1 (Google 端点 bug) → 暴露根因 2 (容量耗尽)
→ 修根因 2 → 暴露根因 3 (IPv6 网络层)
→ 修 IPv6 → 发现路由器 DHCPv6 仍在广播
→ 修路由器 → 发现 Clash fake-ip 拦截
→ ...
→ 2,700 turns, 15 小时, 194 个失败标记 vs 7 个成功标记
```

**用户消息**：119 条，平均 65 chars。85 条 <50 chars。用户在不断追问"修好了吗？"但 agent 无法判断"修好了"还是"只是这个子问题修好了"。

**机制**：环境问题有 3+ 个独立根因同时存在。修一个暴露另一个。Agent 缺乏"足够好了，先这样"的判断力。

#### 模式 C：方向摇摆 (Pivot Loop) — 13%

**典型案例**：Session #6 "GLM subagent harness 审阅"

```
用户: "让 glm 去做了几个任务...帮我好好审阅一下代码"
→ 审阅发现 harness 质量差 → 让重写
→ 重写时发现 WebSocket 连不上 → 让修连接
→ 修连接时发现 MOONSHOT_API_KEY 无效 → 让修 auth
→ 修 auth 时发现 backend run 卡住 → 让修 backend
→ ...
→ 1,662 turns, 14 小时
→ 用户最后自己贴了 root cause 分析
```

**机制**：用户在执行过程中改变需求方向。Agent 没有能力 push back 或要求明确 scope。

#### 模式 D：信息黑洞 (Information Black Hole) — 13%

**典型案例**：Session #3 "OpenClaw auto-compaction 分析"

```
2,588 turns, 2,494 次 tool_use, 0 条 assistant 文本回复
用户发了 94 条消息（平均 379 chars，包含大量技术分析）
但 agent 的推理过程完全不可见——全在 tool_use 里
→ 用户最后自己在分析根因：
  "ops agent 的 auth.json 为空，它的 cron job 跑 isolated session 时认证链路断了"
```

**机制**：Agent 大量执行工具但不产出可见结论。用户看不到进展，不得不自己介入分析。这是 Claude Code 深度模式的特有问题。

#### 模式 E：全自动空转 (Autonomous Spin) — 7%

**典型案例**：Session #15 "(untitled)"

```
1,000 turns, 0 条用户消息, 50 条 assistant 消息
全自动执行（通过注入 prompt 启动）
3 个成功标记 vs 8 个失败标记
1h 26m 后触达 1,000 turn 上限
```

**机制**：无人监督的自动任务缺乏终止条件。Agent 在"还能做更多"和"已经够了"之间无法判断。

### 2.5 Burst 模式分析

**601/717 Codex sessions (84%)** 存在 burst pattern——>10 turns 在 <60 秒内发生。这是自动化循环（tool_use → tool_result → tool_use 快速交替）。

**最长自主间隔**（无用户干预的连续 turns）：

| 间隔 (events) | Session | 上下文 |
|---|---|---|
| **194** | Quant pipeline fix | 单条用户消息 → 194 次工具调用 |
| **194** | Tachi vault hardening | /goal 模式 → 全自动执行 |
| **155** | Data quality sweep | "所有" 模式 → 155 events |
| **127** | Go MCP Phase 2 | 多消息 session 中的最长间隔 |
| **120** | Longbridge WebSocket | "你去干吧" → 120 events |

---

## 三、Fire-and-Forget 执行模式

### 3.1 Codex 的主导使用模式

**73.6% 的 Codex sessions (528/717) 只有 1 条用户消息**。用户发送一个详细的任务规格，然后 Codex 自主执行。

125 个 session 满足 "fire-and-forget" 条件（≤3 条用户消息 + >100 events）。

### 3.2 零用户消息 Session（纯 dispatch）

4 个 session 有 **0 条用户消息**——agent 通过注入 prompt 启动：

| # | Events | Asst Msgs | 第一条 Assistant 消息（任务推断） |
|---|---|---|---|
| 1 | **1,000** | 50 | "我会先把两份指定文档和当前工作树状态读出来" |
| 2 | **842** | 38 | "拆成两个可验证交付：先恢复 radar_daemon，再修 Tachi vault" |
| 3 | **281** | 19 | "Map Hermes files and existing build/test layout" |
| 4 | **149** | 11 | "Pick up from current workspace state, check active goal" |

**全部 4 个报告"已完成"**。Session #1 触达了 1,000 event 上限。

### 3.3 单条消息 Session 的成功模式

| # | Events | 任务 | 状态 |
|---|---|---|---|
| 1 | 362 | 只读 code review（算法/AutoResearch/Iron Rules diff） | **COMPLETED** |
| 2 | 331 | "舰长全权授权" — Longbridge 连接泄漏外科手术修复 | MIXED |
| 3 | 292 | "Granting full write access" — 修复所有 import 断裂 | MIXED |
| 4 | 270 | "Mission Directive" — 修复下游 slot 垄断 | **COMPLETED** |
| 5 | 258 | 只读 diff review（commit 阻塞项） | FAILED (发现问题但没修) |
| 6 | 253 | Review feature_card Python→Go/Rust 迁移 | **COMPLETED** |
| 7 | 223 | "Mission Directive" — Hyperion 重构管线 | BLOCKED (sandbox 拒绝写入) |
| 8 | 218 | OpenClaw tool allowlist 修复 | **COMPLETED** |
| 9 | 209 | Tachi memory-server Daily Pipeline 实现 | **COMPLETED** |
| 10 | 195 | "Fix ALL issues" — Vault 加固 | **COMPLETED** |

**规律**：
- **只读 review 100% 完成**（不需要写文件，不会被 sandbox 阻塞）
- **"Mission Directive" 军事风格任务简报** 触发最长自主执行
- **"全权授权" 显式权限授予** 与更长自主运行相关
- **Sandbox 写入拒绝** 是自主写任务的主要失败模式

### 3.4 /goal 驱动循环

Codex 的 `/goal` 指令触发特殊执行模式：

```
/goal → 系统拆解为子任务 → 逐个执行 → 每阶段验证门 → 完成审计 → 预算检查
```

**Session #7 (db.rs 拆分) 的 5 阶段执行**：

| 阶段 | 内容 | 验证门 |
|---|---|---|
| Phase 1 | unwrap/expect 安全审计（仅生产代码，排除 #[cfg(test)]） | cargo test + clippy |
| Phase 2 | 死代码清理（分类而非删除——MCP schema 表面保留） | cargo test + clippy |
| Phase 3 | unsafe 审计（isatty → safe std lib, 必须 FFI 加 SAFETY 注释） | cargo test + clippy |
| Phase 4 | 针对性测试（最高价值未覆盖边缘） | cargo test (271→278) |
| Phase 5 | 文档归档（5 份过期设计文档移入 docs/archive/） | cargo fmt + diff review |

**关键设计**：Sidecar subagent 做只读盘点（dead-code triage, coverage map），主 session 控制编辑序列。这避免了 subagent 意外修改文件。

---

## 四、多 Agent 编排模式

### 4.1 编排架构

```
User (舰长/Captain)
  ├── Codex/OpenCode (主力编码)
  │     └── Sidecar subagents (只读盘点/审查)
  ├── GLM-5.1 (侦察兵 — "量大管饱，上下文窗口多")
  │     └── Scout reports → 回传给主力
  ├── Gemini (文档/中文 README/PR review)
  ├── Kimi K2.5 (苦力 — 通过火山引擎)
  ├── Claude Code (工具密集执行)
  └── Hermes (交易+工程混合 agent)
        └── delegate_task → Kimi K2.6 (廉价 subagent)
```

### 4.2 协调机制

**机制 1：`<subagent_notification>` 标签**

Codex 通过嵌入在用户消息中的 subagent 完成报告接收结果：
```xml
<subagent_notification>
  {"agent_path":"019df640-ec84...","status":{"completed":"<results>..."}}
</subagent_notification>
```

**机制 2：`delegate` 工具**

直接 agent-to-agent 委派：
> "我说 delegate codex 他就把工作丢给 codex 最佳最新的模型就好了"

**机制 3："让 X 去" 自然语言调度**

> "让 opencode 去检查一遍" → dispatch to OpenCode
> "让 gemini 来" → dispatch to Gemini
> "让 glm 去做" → dispatch to GLM

### 4.3 任务→模型路由表

| 模型 | 路由到的任务 | 证据 |
|---|---|---|
| **GLM-5.1** | Scout/研究、subagent harness 创建、代码审阅（量大管饱） | "让 glm 去做了几个任务" |
| **Codex** | 实现、调试、基础设施 | "delegate codex" |
| **Gemini** | 文档、中文 README、PR review | "中文版明天我让 gemini 来" |
| **OpenCode** | 代码审阅、清理、跨 repo 协调 | "让 opencode 去检查一遍" |
| **Claude/Opus** | 规划、架构决策、重度推理 | "主模型 opus4.5 也要参与看 plan" |
| **Kimi K2.5** | 批量编码苦力 | "worker via Volcengine" |

### 4.4 跨模型质量控制

> "GLM reviews Codex's work, Codex reviews GLM's work"

用户显式建立了**交叉审查**模式：一个模型的产出交给另一个模型审阅。这解决了单模型 self-review 的盲区。

### 4.5 Hermes 的 delegate 模式

Hermes 内置了 `delegate_task` 工具：

```
Hermes (旗舰) → delegate_task(scout, "扫描 300502.SZ 的 V8 快照")
  → Kimi K2.6 (廉价 subagent) → 执行 → 返回结果
  → Hermes 整合多个 scout 结果 → 输出最终分析
```

**设计原则**：
- Scout 用廉价模型（Kimi K2.6），避免 68K context 爆炸
- 旗舰模型（GPT-5.5）只做整合和决策
- MCP field projection：scout 只返回需要的字段，不返回全量数据

---

## 五、模型行为深度对比

### 5.1 Codex (GPT-5 系列)

**核心特征**：深度自主执行，fire-and-forget 模式。

**典型交互**：
```
用户: "把 db.rs 拆了" (15 chars)
Codex: [1,600 turns 自主执行, 10 分钟]
用户: "然后做 code review" (25 chars)
Codex: [继续执行]
```

**优势**：
- 最长自主间隔 194 events（无需用户干预）
- /goal 模式有完整的阶段验证门
- 73.6% session 只需 1 条用户消息

**弱点**：
- 范围蔓延：67% 的长任务 STALLED
- 失败标记平均是成功标记的 3 倍
- 缺乏"足够好了"的判断力

**独特行为**：
- 84% session 有 burst pattern（自动化循环）
- Sandbox 写入拒绝是主要失败模式
- "Mission Directive" 军事风格任务简报触发最长自主执行

### 5.2 Claude Code

**核心特征**：双模态——闪电工具人 or 沉默马拉松。

**模式 A：快速工具人（88% sessions, ≤2 turns）**
```
用户: "帮我改一下这个文件"
CC: [改完]
用户: "跑一下测试"
CC: [跑完]
```

**模式 B：深度工程（12% sessions, 1000+ turns）**
```
用户: [874 条消息, 平均 893 chars]
CC: [2,004 turns, 但 assistant 文本输出经常为 0]
     [2,494 次 tool_use, 0 条文本回复]
```

**优势**：
- 工具密度最高（2.47 tools/user msg）
- 擅长多层根因诊断（503 三层诊断）
- 代码审阅质量高

**弱点**：
- 深度模式下不汇报推理过程（信息黑洞）
- 需要高频用户 steering（50% steering ratio）
- 长任务 0% COMPLETED

**独特行为**：
- `[SYSTEM DIRECTIVE: OH-MY-OPENCODE - TODO CONTINUATION]` 自动注入
- `<local-command-caveat>` 本地命令隔离
- Background task 完成通知嵌入对话

### 5.3 Windsurf (Claude Opus/Sonnet)

**核心特征**：Tachi Memory 注入 + 长上下文代码理解。

**典型 Session 结构**：
```
<SYSTEM-RETRIEVED-MEMORY[e648ba48]>  ← Tachi 自动注入相关记忆
Current `load_labeled_data` in `autoresearch_lab/prepare`...
用户: "用新的管线看看赣锋锂业"
Claude: [1,098 turns, 深度分析]
```

**Memory 注入统计**（Top 5 Windsurf sessions）：

| Session | 注入记忆数 | 内容 |
|---|---|---|
| V8 Go 下沉 (22MB) | **13** | pickle cache、V8 架构修正、Oracle Route C、HAPI 人格化输出 |
| Code Review Marathon (19MB) | 4 | Longbridge slot、Hermes fallback、Hyperion 瓶颈 |
| Tachi Tool Surface (8MB) | **18** | MCP 工具面、记忆算法、worktree 安全、dispatch ops |
| Convoy Pattern (3MB) | 10 | 数据提取批次、文档清理、Pattern Discriminator |
| Cold Cache Diagnosis (5MB) | 0 | (标题含 memory 引用但正则未匹配) |

**优势**：
- Memory 注入提供跨 session 连续性
- 最长中位 session 时长（1.6h）
- 100% coding 分类（无交易噪音）

**弱点**：
- 样本量小（65 sessions）
- 上下文溢出风险（22MB session）

**独特行为**：
- 多模型混用：同一 session 内切换 claude-opus-4-6-thinking / claude-opus-4-7-xhigh / gpt-5-5-high
- 工具使用以 shell 为主（1,158 shell + 268 file_read in top session）

### 5.4 Hermes (Gemini/GLM/GPT-5.5)

**核心特征**：交易+工程混合 agent，MCP 是骨架。

**模型分布**：

| 模型 | Sessions | 纯 Coding | Mixed |
|---|---|---|---|
| GLM-5.1 | 403 (73%) | 16 | **383** |
| GPT-5.5 | 76 (14%) | 0 | **71** |
| GLM-5-turbo | 41 (8%) | **39** | 2 |
| Gemini 3.1 Pro | 17 (3%) | 13 | 3 |
| Kimi K2.6 | 8 (2%) | 0 | 8 |

**GLM-5.1 是主力**（73%），但几乎全是 mixed session。**GLM-5-turbo 是纯编码模型**（95% coding），用于 cron job。**GPT-5.5 100% mixed**——从不单独做编码。

**MCP 是万能骨架**：81.8% 的 session 提到 MCP 工具设计。交易流程全走 `mcp_hyperion_*` 工具。

**架构决策嵌入交易对话**：
- Tachi vs Hermes Memory 权限边界——在讨论持仓时决定
- Subagent delegation 模式——在扫描股票时设计
- Async research dispatch——在跑回测时构思

**独特行为**：
- `delegate_task` 工具：旗舰模型 → 廉价 subagent (Kimi K2.6)
- Scout protocol：轻量侦察 → 整合 → 决策
- 自进化机制：`agent_evolution` 工具 + `SOUL.md` 周期性反思

### 5.5 Qwen (OpenCode 中的 Qwen 模型)

**核心特征**：样本小（3 sessions in OpenCode），纯 coding，高 token 消耗。

- Qwen 3.7 Max 在 OpenCode 中 3 个 main session，平均 5M tokens/session
- Subagent 派生率 **900%**（3 个 main session 派了 27 个 subagent）
- TODO 完成率仅 20%（10 个 TODO 完成 2 个）
- 典型失败：618 条消息的"整理 antigravity/windsurf session"session，5 个 TODO 完成 0 个

### 5.6 OpenCode (多模型 IDE)

**核心特征**：多模型聚合平台，丰富的元数据（cost/tokens/TODO/subagent），2.3GB SQLite 数据库。

**数据规模**：

| 指标 | 值 |
|---|---|
| 总 Sessions | 247 (main) + 537 (subagent) = **784** |
| 总 Messages | 45,120 |
| 总 Tokens | 426M (input+output) |
| 总 Cost | $303.90 |
| 总 TODOs | 816 (completed: 656, **80.4%**) |
| 时间跨度 | 2026-01-30 ~ 2026-05-30 (4 个月) |
| DB 大小 | 2.3 GB |

**模型分布**（main sessions）：

| 模型 | Sessions | Avg Tokens | Cost | TODO 完成率 | Subagent 率 |
|---|---|---|---|---|---|
| GPT-5.5 (Copilot xhigh) | 11 | 630K | $0 | 69.6% | 118% |
| Claude Opus 4.7 | 5 | 572K | $0 | 51.9% | 320% |
| Qwen 3.7 Max | 3 | **5.1M** | **$189** | **20%** | **900%** |
| DeepSeek V4 Pro | 3 | 1.7M | $36 | 47.8% | 600% |
| Claude Sonnet 4.6 | 3 | 390K | $0 | **91.7%** | 33% |
| GPT-5.4 (Copilot) | 6 | 1.1M | $1.6 | 61.5% | 450% |
| Kimi K2P6 | 2 | 759K | $0 | **100%** | 50% |
| GLM 5.1 | 2 | 690K | $0.05 | **100%** | 1000% |

**关键发现**：

1. **Claude Sonnet 4.6 TODO 完成率最高（91.7%）**——样本少但完美
2. **Qwen 3.7 Max 最贵且最低效**——$189 cost，20% TODO 完成，900% subagent 率（什么都派 subagent）
3. **GPT-5.5 Copilot 免费且高效**——$0 cost，69.6% TODO 完成
4. **GLM 5.1 最便宜**——$0.05/session，100% TODO 完成（简单任务）
5. **Kimi K2P6 100% 完成率**——样本少但表现好

**Top Sessions by Tokens**：

| Title | Model | Tokens | Cost | Duration | TODOs | Subagents |
|---|---|---|---|---|---|---|
| Trae IDE 35步限制问题 | unknown | 29M | $0 | 291m | 5/5 | 3 |
| 龙虾机器人项目 | unknown | 12.6M | $0 | 669m | 4/4 | 3 |
| Repo问题审查 | unknown | 12M | $0 | 518m | 5/5 | 2 |
| antigravity 503排查 | unknown | 11.7M | $0 | 2900m | 3/3 | 1 |
| 整理 session 记录 | **Qwen 3.7** | **11.3M** | **$145** | 267m | **0/5** | 11 |
| Tachi 审查与发布 | unknown | 10.4M | $0 | 508m | 6/6 | 16 |
| LongPort SDK 修复 | unknown | 9.3M | $0 | 1511m | 8/8 | 1 |
| OpenClaw compaction | unknown | 9.2M | $0 | 368m | 3/3 | 15 |

**Agent 模式分布**：

| Agent | Sessions | Avg Tokens |
|---|---|---|
| default | 207 | 1.5M |
| build | 39 | 1.0M |
| plan | 1 | 1.4M |

**Subagent 派生模式**：

| 父模型 | Main Sessions | Subagent Sessions | 派生率 |
|---|---|---|---|
| Qwen 3.7 Max | 3 | 27 | **900%** |
| GPT-5.5 (Copilot default) | 2 | 12 | 600% |
| DeepSeek V4 Pro | 3 | 18 | 600% |
| GPT-5.4 (Copilot) | 2 | 9 | 450% |
| Claude Opus 4.7 | 5 | 16 | 320% |
| GPT-5.5 (Copilot xhigh) | 11 | 13 | 118% |
| Claude Sonnet 4.6 | 3 | 1 | 33% |

**Cost 分布**：

| Provider | Sessions | Total Cost | Total MTokens |
|---|---|---|---|
| Alibaba (Qwen) | 3 | **$189** | 15.3M |
| DeepSeek | 3 | $36 | 5.2M |
| GitHub Copilot | 25 | $1.6 | 16.3M |
| ZhipuAI (GLM) | 2 | $0.05 | 1.4M |
| OpenAI Direct | 5 | $0 | 0.7M |
| Kimi | 2 | $0 | 1.5M |

**Session 时长分布**：

| 时长 | Sessions | Avg Tokens | Avg Cost |
|---|---|---|---|
| < 5min | 53 | 133K | $0 |
| 5-30min | 54 | 383K | $0.08 |
| 30-60min | 26 | 1.2M | $0 |
| 1-2h | 35 | 1.2M | $1.19 |
| 2-4h | 21 | 1.8M | $0 |
| 4-8h | 23 | 3.6M | $6.29 |
| **8h+** | **35** | **3.7M** | $1.23 |

**独特行为**：
- **TODO 系统**：结构化 TODO 管理（content/status/priority/position），80.4% 总完成率
- **Subagent 架构**：parent_id 链接，subagent session 独立记录 tokens/cost
- **多 provider 聚合**：同一 IDE 内 Copilot/OpenAI/DeepSeek/Alibaba/ZhipuAI/Kimi 六个 provider
- **Compaction 追踪**：time_compacting 字段记录压缩时间（但当前无 session 触发过）
- **Cost 追踪**：精确到 session 级别的 cost 记录（Copilot = $0，直连 API = 实际费用）

### 5.7 Antigravity (Google AI IDE)

**核心特征**：高自主混合 agent（93.5% mixed），强自纠错（71.4%），跨 agent 编排器。

**数据规模**：

| 指标 | New Protocol | Legacy Protocol | 合计 |
|---|---|---|---|
| Sessions | 54 | 100 | **154** |
| Total Turns | 14,568 | 28,811 | **43,379** |
| Total Tools | 2,583 | 4,382 | **6,965** |
| Avg Turns/Session | 270 | 288 | 282 |
| Avg Tools/Session | 48 | 44 | 45 |
| Steering Ratio | **9.5%** | **11.4%** | 10.8% |
| 总数据量 | — | — | **462.7 MB** |

**模型分布**：78.6% Gemini（各种版本），19.5% unknown，1.9% Claude。

**工具使用 Top 10**：

| 工具 | 调用次数 | 占比 | 说明 |
|---|---|---|---|
| **run_command** | 6,136 | **36.8%** | Shell 命令执行 |
| **view_file** | 2,757 | 16.6% | 读文件 |
| **task_boundary** | 1,407 | 8.4% | 任务管理（仅 Legacy） |
| **grep_search** | 1,106 | 6.6% | 内容搜索 |
| **command_status** | 856 | 5.1% | 命令状态检查 |
| **write_to_file** | 723 | 4.3% | 写文件 |
| **replace_file_content** | 573 | 3.4% | 编辑文件 |
| **list_dir** | 548 | 3.3% | 列目录 |
| **notify_user** | 311 | 1.9% | 通知用户（仅 Legacy） |
| **multi_replace_file_content** | 306 | 1.8% | 批量编辑 |

**总工具调用**：16,652 | **平均/session**：108.1 | **中位**：71.5

**独特行为统计**：

| 行为 | Sessions | 占比 | 说明 |
|---|---|---|---|
| **自纠错** | 110 | **71.4%** | "纠正"、"错了"、"修正"、"推翻" |
| **领域锚定** | 144 | **93.5%** | 使用系统术语编织分析 |
| **长自主链 (20+)** | 58 | 37.7% | 连续 20+ tool calls 无用户输入 |
| **记忆检索** | 141 | **91.6%** | tachi_memory / SYSTEM-RETRIEVED-MEMORY |

**最长自主链**：119 次连续 tool calls

**Top Sessions**：

| Title | Turns | User | Tools | Size | 分类 |
|---|---|---|---|---|---|
| "hi" (new) | 1,584 | 121 | 199 | 3.0M | mixed |
| "你看看" (new) | 1,506 | 170 | 287 | 2.2M | mixed |
| "agent runtime 讨论" (new) | 1,168 | 119 | 147 | 2.2M | mixed |
| "还有哪些待办" (legacy) | **1,803** | 216 | 141 | 4.1M | mixed |
| "Hapi 管线是什么" (legacy) | 1,735 | 140 | 226 | 3.8M | mixed |
| "扒 nexu app" (legacy) | 1,393 | 169 | 221 | 2.7M | coding |
| "hi" (legacy) | 1,352 | 191 | 229 | 2.3M | mixed |
| "跑 autoresearch" (legacy) | 1,284 | 174 | 229 | 2.3M | mixed |

**New vs Legacy 协议差异**：

| 维度 | New | Legacy |
|---|---|---|
| 平均 Turns | 270 | 288 |
| 平均 Tools | 48 | 44 |
| task_boundary 工具 | 无 | **12.4% of tools** |
| notify_user 工具 | 无 | 有 |
| artifact_reminder | 无 | 有 |
| 风格 | 直接执行 | 任务管理+通知 |

**跨 Agent 编排**：

Antigravity 不只是编码助手，而是**编排器**——373 次跨 agent dispatch：
- 调度 Codex 做实现
- 调度 Claude Code 做审阅
- 自己用 Gemini 做策略分析和知识蒸馏

**关键能力**：
1. **自纠错率 71.4%**——所有 IDE 中最高。"我刚才说的不对"、"推翻之前的判断"
2. **领域锚定 93.5%**——用你的系统术语编织分析，不是泛泛而谈
3. **记忆检索 91.6%**——几乎每个 session 都从 Tachi memory 中拉取上下文
4. **373 次跨 agent dispatch**——主动调度 Codex/Claude Code 做子任务
5. **用户用 radare2 patch 了 Antigravity 二进制**来移除输出限制——说明工具本身被深度使用

**与其他 IDE 的对比**：

| 维度 | Antigravity | Codex | Claude Code | OpenCode |
|---|---|---|---|---|
| 自主性 | 高 (9.5% steering) | **最高** (2.2%) | 低 (50%) | 中 (20%) |
| 自纠错 | **71.4%** | 低 | 中 | 中 |
| 工具密度 | **108/session** | 60/session | 2.47/user | 183 msgs/session |
| 混合度 | **93.5% mixed** | 3.5% mixed | 1% mixed | ~10% mixed |
| 跨 agent | **373 dispatches** | subagent only | subagent only | subagent (parent_id) |
| 记忆系统 | **91.6% 检索率** | 无 | 无 | 无 |

---

## 六、架构演进蒸馏

### 6.1 系统架构全景

```
                    ┌──────────────────┐
                    │    Agent 层       │
                    │ Hermes / Codex /  │
                    │ Claude / GLM /    │
                    │ Kimi / Gemini     │
                    └────────┬─────────┘
                             │ MCP / delegate_task / JSON-RPC
                    ┌────────▼─────────┐
                    │   hapi-edge      │  ← Go MCP Server (thin proxy)
                    │   (Go)           │     stdio + Streamable HTTP
                    │   16 tools       │     3 resources, 2 prompts
                    └────────┬─────────┘
                             │ JSON-RPC (不直接 import engine)
                    ┌────────▼─────────┐
                    │  hapi-server     │  ← Python FastAPI
                    │  (Python)        │     LS1 snapshot 编排
                    └──┬─────┬─────┬───┘
                       │     │     │
              ┌────────▼┐ ┌─▼───┐ ┌▼──────────┐
              │warpcore │ │hapi │ │autoresearch│
              │(Rust)   │ │.db  │ │.db         │
              │25,014行 │ │     │ │            │
              └──┬──────┘ └─────┘ └────────────┘
                 │
              ┌──▼───────────┐
              │radar_daemon  │  ← 唯一 writer to rust_gateway.db
              │(Rust, 纯I/O) │     3-Slot 凭证轮换
              └──┬───────────┘
                 │
              ┌──▼──────────────┐
              │rust_gateway.db  │  ← 市场数据湖 (single-writer)
              │kline_cache.db   │
              └─────────────────┘
```

### 6.2 三库隔离原则

| 数据库 | 职责 | Writer | Reader | 写入模式 |
|---|---|---|---|---|
| **hapi.db** | 运营真相（持仓/交易/日志/watchlist） | hapi-server | All | WAL |
| **rust_gateway.db** | 市场数据湖（行情/K线/quote） | radar_daemon **唯一** | hapi-edge, hapi-server (mode=ro) | 单写者 |
| **autoresearch.db** | 实验数据（因子/回测/ghost trades） | autoresearch_lab | autoresearch_lab | 独立 |

**教训**：score_snapshot (18,928 行) 和 score_factor_exposure (710,602 行) 曾污染 hapi.db。数据库边界必须由代码（`db_paths.py` + schema 合约测试）强制执行。

### 6.3 MCP Thin Proxy 原则

**根因发现**（Session #13, 1,121 turns）：

Agent 写 scratch_*.py 脚本而不是用 Rust 基础设施，**不是因为 agent 笨，而是因为缺少结构化工具接口**。

```
CLI 只输出人类可读报告 → 没有稳定的编程接口
→ Agent 被迫写 scratch 脚本作为 workaround
```

**修复**：
1. CLI 增加 `--format json --fields` 参数
2. MCP 重新设计为 thin proxy：`MCP → hapi-server (JSON-RPC) → warpcore (Rust)`
3. MCP **不**直接 import engine、**不**直接读 SQLite、**不**内部执行 LS1
4. MCP 工具设计为 intent-level：`hunt`, `guard`, `batch_snapshot(fields)`, `portfolio`
5. Remote MCP 架构设计（未来 SaaS）：客户端连 remote MCP，数据/凭证留在服务端

### 6.4 Tachi memory-server 模块化

**Session #7/#8 (COMPLETED, 1,631+1,565 turns)** 的核心成果：

```
Before: main.rs + db.rs = ~1,800 行（schema/CRUD/graph/hub/audit/GC 混合）

After:
  schema.rs        — 数据结构定义
  memory_crud.rs   — 记忆 CRUD 操作
  graph.rs         — 知识图谱
  hub_db.rs        — Hub 数据库操作
  audit.rs         — 审计日志
  gc.rs            — 垃圾回收

设计原则：
- RwLock 并发（不是 Mutex）— 读多写少场景
- Virtual Capability 模式 — 把具体 MCP/插件抽象为稳定逻辑能力层
- Ghost message bus — 模块间通信不直接引用
- Profile system — ToolProfile/profiles.rs 分层暴露工具
- Non-ASCII slug fallback — 笔记标题含非 ASCII 时回退到稳定 `note` slug
```

**Phase 4 Virtual Capability 缺口**（审查发现）：
- `memory-server` 当前不可编译，V4 接入处于断裂状态
- `mod project_db_ops;` 在主入口被引用但模块文件不存在
- 应用层与文档不一致
- 测试缺口：Virtual Capability 层无集成测试

### 6.5 凭证管理：双模 Vault

```
模式 A（开发）：从 .env / 环境变量直接读取
模式 B（生产）：Vault unlock → 注入子进程环境变量

关键发现：
- Longbridge 用 APP_SECRET/ACCESS_TOKEN，不匹配 *_API_KEY 模式
- Vault 必须接受所有合法 env name，不能只匹配特定 pattern
- TUSHARE_TOKEN 可从 ~/tk.csv (SDK cache) 恢复
- Launcher 条件必须是 OR 不是 AND：
  "Longbridge OR Tushare missing" → load vault
  不是 "only when Longbridge missing"
- Provider readiness matrix: daemon status 暴露 per-provider health
  (Longbridge ready, Tushare missing) 而非埋在日志里
```

### 6.6 Async Research Dispatch

**设计**（Session from Windsurf, 124 turns）：

```
MCP call → hapi-edge → internal/research/runner.go
  → 立即返回 {run_id, status: "queued", pid, log_path, result_path, estimated_time}
  → 后台进程执行 Python CLI (engine.v8.autoresearch_lab.cli)
  → 完成后写 result JSON

状态判定：
  process 消失 AND result.json 存在 → "completed"
  process 消失 AND result.json 不存在 → "failed"
  process 还在 → "running"
```

**关键**：In-memory run tracking + PID-based liveness checks。不引入外部依赖（Redis/queue）。

### 6.7 Go Edge Gateway 的定位

```
Go edge gateway (port 8890/8891):
  /quote 和 /kline → 直接读 SQLite（纯 Go，快）
  /snapshot/batch → 不是原生 Go！Proxy 到 Python LS1 worker

Go 定位：API edge 层和迁移壳，不是完整的 Rust LS1 引擎
Go 重写 API 层有用（edge gateway），但不加速计算（仍 proxy 到 Python）

性能对比（100 只股票 batch）：
  Go proxy: 13.56s
  Python RPC: 7.95s（更快，因为少了 proxy 开销）
```

### 6.8 数据新鲜度的交易日历感知

```
Bug: 15min/60min 数据新鲜度用固定 36h/48h wall-clock 过期
  → 周日标记周五数据为 stale（错误！）

Fix: 改为 trading-day aware
  → 接受 latest_trade_date（周末/假期不过期）
```

---

## 七、调试 Playbook

### 7.1 503 错误的三层根因（Session #2, 2,700 turns）

```
Antigravity 弹 503 → 三个独立根因同时存在：

层 1: Google 端点 Bug
  daily-cloudcode-pa.googleapis.com（测试节点）被路由到而非生产节点
  Fix: wrapper script 替换启动参数
  (antigravity_language_server_wrapper.sh)

层 2: 真实容量耗尽
  生产端点返回 MODEL_CAPACITY_EXHAUSTED
  不同 Google 账号有不同配额
  Fix: 无（服务端限制）

层 3: 网络层
  IPv6 "no route to host" + TLS bad record MAC + ECONNRESET
  根因：路由器 DHCPv6/RA 仍在广播 IPv6
    尽管 Clash 设了 ipv6:false（那只影响 Clash 自身）
  Fix: 路由器层禁用 dhcpv6=disabled, ra=disabled, ndp=disabled, ip6assign=0

教训：
- 503 可以有 3+ 个独立根因同时存在
- IPv6 必须在所有层禁用：app config + proxy + OS + 路由器 DHCPv6/RA
- Clash ipv6:false 只影响 Clash，不影响 OS 或路由器
- TUN mode 太激进，system proxy + env vars 更安全
- VPN/proxy routing 需要 per-service explicit rules，不是 geo-IP guessing
```

### 7.2 Agent 手写 Scratch 脚本（Session #13, 1,121 turns）

```
症状：Agent 跑管线时写 scratch_*.py 而不是用 Rust 基础设施

根因链：
  CLI 只输出人类可读报告
  → 没有稳定的编程接口
  → MCP server 存在但不是 thin proxy
    → 直接 import engine + 读 SQLite + 内部跑 LS1
  → hapi-server 有 JSON-RPC 但 MCP 没用
  → Agent 被迫写 scratch 脚本

三数据库架构未被代码强制：
  hapi.db (运营) ← 被 score_snapshot 18,928 行污染
  rust_gateway.db (市场数据) ← Python 读取没有限制 mode=ro
  autoresearch.db (实验) ← score_factor_exposure 710,602 行应在

修复：
  1. CLI 加 --format json --fields
  2. MCP 改为 thin proxy → hapi-server → LS1/Rust
  3. DB 边界用 db_paths.py + contract tests 强制
  4. rust_gateway.db Python 读取改为 mode=ro
  5. WebSocket → rust_gateway.db 需要 micro-batching (200ms-1s flush)

教训：
  Agent 写 scratch 脚本 = 接口缺失的症状，修接口不是修 agent prompt
  MCP 应该是 thin proxy，不是 fat adapter
  SQLite WAL: 单写者 + 多读者；永远不让多进程同时写 rust_gateway.db
```

### 7.3 Python vs Rust 参数静默偏差（Session #4, 2,299 turns）

```
Python legacy chan theory:
  divergence_rate = inf（T1 divergence check 在 "guaranteed pass" 模式）
  max_bs2_rate = 0.9999
  macd_algo = "peak"
  bs_type = 全部类型
  turnrate_avg → 映射到 AMOUNT_AVG（字段名错误！）

Rust quant_core:
  divergence_rate = 1.0（正确值）
  max_bs2_rate = 0.618
  macd_algo = "area"
  bs_type = [T1, T2, T3a]

其他发现：
  - data.gateway_loader 只读 rust_gateway.db/kline_cache
    但数据在 kline_cache.db/klines（双 schema 不兼容）
  - chan.bsp.latest 把未确认 T1p 候选当正式信号
    → "强动量但缠论说卖" 的冲突
  - 57 个过期 Cython .so 文件 shadow Python 源码
  - snapshot_factory.py Rust path 缺 price_vs_ma60 字段

修复：
  - Rust-first，Python legacy 仅作 fallback reference
  - gateway_loader 改为双 schema 兼容
  - chan_analysis.py: latest BSP 过滤未确认候选
  - 清理 1,370 个 tracked runtime 垃圾文件（31,424 行删除）

教训：
  Python fallback 和 Rust 默认参数分歧 = 静默计算偏差（不报错但结果不一致）
  "未确认" 候选在信号处理中永远不能提升为 "当前信号"
  Build artifacts (target/, .so) 必须从 day one 就在 .gitignore
  Agent 写的 scratch 脚本是缺少结构化工具接口的症状
```

### 7.4 WindClaw 逆向工程（Session #11, 1,424 turns）

```
目标：让 WindClaw（OpenClaw 商业 Electron 套壳）使用自己的模型

发现：
  - WindClaw = Electron shell + 嵌入 OpenClaw runtime + Wind 私有绑定
  - Wind 硬编码：AI Gateway (AliceBase-windclaw-35b via m.wind.com.cn)
  - 双配置：~/.openclaw/openclaw.json vs windclaw-providers-*.json
  - 积分余额 ≤ 0 自动禁用自定义模型切换
  - 启动时强制固定 runtime: [Gateway] enforced fixed runtime
  - macOS code signing 在修改 app.asar 后失效（Gatekeeper 拒绝）
  - 直接文本级 patching app.asar 损坏归档（Electron 显示默认页面）

最终方案：环境变量注入
  CLAWX_AIGW_BASE_URL, CLAWX_AIGW_MODEL 等
  通过 launcher script (WindClaw-Crimson.command) 在启动时注入
  添加 6 个自定义 provider（GLM, Gemini, Kimi, MiniMax, Grok, Moonshot）
  创建 desktop copy 禁用固定 runtime + 伪造积分余额 (999999)

教训：
  - Electron app.asar 是归档格式，不能 string replace
  - 环境变量注入是签名 macOS app 最安全的 runtime override
  - 商业壳围绕开源核心 = 双配置问题
  - 模型切换需要在 4 层修改：provider config / agent model / GUI IPC / slash commands
```

### 7.5 V8 评分系统审计（Windsurf Session, 1,098 turns）

```
V8 审计发现：
  - 30% A-grade precision（OOS 精度低）
  - KILLED factor 有 dead code
  - Magic numbers 无验证
  - entry_score 满分 bug：硬编码 max=18 但理论满分=20

Go 下沉 Phase 1 Batch 1-2：
  - Python V8 fever_relief/overheat → Go snapshot_core.go
  - Watchpoint event-driven architecture 设计
    （替换每日 LLM 调用为条件触发）

教训：
  - V8 不是金标准——30% OOS precision 意味着 70% 的 A 级判定是假阳性
  - Magic numbers 必须有 validation（不是 "看起来对" 就行）
  - Watchpoint 机制：LLM 把 inner_journal 条件编译为 Python 可执行 watchpoint
    → 命中才唤醒 LLM → 预估速度 ×3，成本 ÷3
```

### 7.6 Antigravity Compaction 失败（Session #2 + #12, 3,854 turns 合计）

```
问题：Antigravity 升级后不会自动压缩，token 消耗极快

逆向发现：
  OpenClaw: 有 auto-compaction
    触发点 1: 上下文溢出后恢复
    触发点 2: 成功一轮后 contextTokens > contextWindow - reserveTokens
  Codex: 加密状态 + 双后端（主模型 + fallback）
  Claude Code: hooks 系统可定制 compaction 行为
  Antigravity: 只有服务端 compaction，客户端无感知

Token 燃烧根因（跨所有 session 一致）：
  1. 长 session 无 compaction
  2. 429/fallback 级联
  3. 重型 cron job
  4. 未裁剪的 tool output 导致 context bloat

Patch 脆弱性：
  App 升级覆盖 main.js patches
  需要自动化 re-patch 或外部 proxy 层（Antigravity Manager）

Auth 是静默失败模式：
  三种 auth 失败都表现为"不回复"：
  1. OAuth token 损坏（账号切换扩展导致）
  2. 空 agent auth.json（ops agent 的 cron job 认证链断裂）
  3. Clash fake-ip 拦截了 Google 登录域名
```

---

## 八、Windsurf/Claude 深度 Session 分析

### 8.1 V8 Go 下沉 + 审计 (22MB, 1,098 turns)

**注入的 13 条 Tachi 记忆**：pickle cache 优化、V8 架构修正、Oracle Route C 策略、HAPI 人格化输出等。

**核心工作**：
- Phase 1 Batch 1-2：Python V8 fever_relief/overheat → Go snapshot_core.go
- 全面 V8 审计：30% OOS precision、KILLED factor dead code、magic numbers
- Watchpoint event-driven architecture 设计

**工具使用**：1,158 shell + 268 file_read

**多模型混用**：同一 session 内切换 claude-opus-4-6-thinking / claude-opus-4-7-xhigh / gpt-5-5-high

### 8.2 Code Review Marathon (19MB, 276 turns)

**核心工作**：50+ review prompts 覆盖 Go snapshot_core、MCP prompts、Rust ichimoku、Hermes 微信限流、市场情报日期修复、MCP 分页/schema。

**关键产出**：RADAR_REFACTOR.md 设计文档——TierRouter、dynamic tiering、credential pool。

**工具使用**：1,819 shell + 155 file_read（最高 shell 密度）

**决策**：Hermes Rust 加速只用于 CPU-bound hot paths (redact/fuzzy)，不全量重写。backtest.submit/status 通过 hapi-edge Go。

### 8.3 Tachi Tool Surface v2 (8MB, 206 turns)

**注入的 18 条 Tachi 记忆**：MCP 工具面、记忆算法、worktree 安全、dispatch ops、library/wiki 哲学。

**核心工作**：Docker build+deploy 到软路由 192.168.100.1。高优先级 bug 修复（缠论 MACD divergence 假阳性、position sizer、sentinel stop、LS1 pipeline）。

**关键决策**：Codex /goal 三层架构分析（SQLite 状态机、系统驱动续行、完成审计）作为 Tachi 的设计参考。

### 8.4 Convoy Pattern 形式化 (3MB, 186 turns)

**注入的 10 条 Tachi 记忆**：数据提取批次、文档清理、Pattern Discriminator。

**核心工作**：分析 3 份 Hermes 改进报告（来自 Codex）。性能 profiling 识别 hot paths：
- SequenceMatcher O(n*m)
- Regex alternation
- deepcopy JSON schema
- Guardrail hashing

**关键决策**：不做 Hermes 全量 Rust fork。只有 2 个 Rust 候选：redact (Aho-Corasick) 和 fuzzy matching。Profiling before PyO3。

**Convoy 模式规则**：
```
市长/工人分工：
  协调者（市长）：拆解、分派、巡检、整合
  执行者（工人）：只做被分派的切片，不擅自改全局计划
Convoy 思维：多步任务像车队行进——保持队形，不超车
```

---

## 九、任务类型分析

### 9.1 按关键词分类

| 任务类型 | Sessions | 平均 Turns | 平均 Steering | 主力模型 |
|---|---|---|---|---|
| **REFACTOR** (重构/拆分) | 89 | **143** | 7.8% | Codex 100% |
| **DEBUG** (修 bug/503) | 234 | 98 | 12.3% | Codex 78% |
| **FEATURE** (新功能) | 156 | 72 | 15.1% | Codex 65% |
| **REVIEW** (审阅/审查) | 178 | 64 | 18.7% | CC 45% |
| **DEVOPS** (部署/CI) | 67 | 89 | 10.2% | Codex 82% |
| **MODEL_INFRA** (compaction/模型) | 34 | **201** | 5.4% | CC 55% |
| **KNOWLEDGE** (蒸馏/提取) | 12 | 156 | 22.1% | CC 67% |
| **TESTING** (测试) | 45 | 53 | 14.8% | Codex 71% |
| **GENERAL** (其他) | 1,625 | 42 | 20.5% | 混合 |

**REFACTOR 任务**平均 turns 最高（143）但 steering 最低（7.8%）——Codex 处理重构最自主。

**MODEL_INFRA 任务**平均 turns 第二高（201）但 steering 也很低（5.4%）——这些是 compaction 逆向等深度分析任务。

### 9.2 跨模型同任务对比

#### Compaction 逆向工程

| 维度 | Claude Code (#12) | Codex (#2) |
|---|---|---|
| Turns | 1,154 | 2,700 |
| 用户消息 | 20 | 119 |
| 方法 | 逆向 OpenClaw/Codex/CC 三家源码 | 从用户视角调试 Antigravity |
| 关键发现 | OpenClaw 有明确触发条件 | 503 有 3 个独立根因 |
| 产出 | 完整对比分析 + patch 方案 | Wrapper script + 路由器修复 |
| 效率 | 20 条消息驱动 1,154 turns | 119 条消息驱动 2,700 turns |

#### Memory-server 重构

| 维度 | Codex (#7/#8) | Codex (#6) |
|---|---|---|
| 任务 | db.rs 模块化拆分 | GLM subagent harness 审阅 |
| Turns | 1,631 + 1,565 | 1,662 |
| 完成率 | **COMPLETED** | **STALLED** |
| 用户干预 | 33-34 条短指令 | 35 条（含方向变更） |
| 关键差异 | 目标单一 + subagent 协作 | 目标模糊 + pivot |

---

## 十、关键教训

### 10.1 长任务的铁律

| # | 教训 | 证据 |
|---|---|---|
| 1 | **单一目标 + 短指令 = 完成** | db.rs 拆分（33 条短消息，COMPLETED） |
| 2 | **多目标 + 频繁改方向 = 停滞** | 10/15 STALLED session 都有方向变更 |
| 3 | **Subagent 协作提高完成率** | COMPLETED session 都 dispatch 了 sidecar agent |
| 4 | **成功标记 > 失败标记 = 正循环** | COMPLETED 比率 1.8，STALLED 平均 0.3 |
| 5 | **没有 TODO 结构的长任务必然蔓延** | Session #1 "先 commit 一下" → 42 小时 |
| 6 | **0-5% steering ratio 是最优自主区间** | 超过 10% 的长任务全部停滞 |
| 7 | **31+ 条用户消息的长任务无一完成** | 更多 steering ≠ 更好结果 |
| 8 | **只读 review 100% 完成** | 不需要写文件，不被 sandbox 阻塞 |
| 9 | **"Mission Directive" 军事风格简报触发最长自主执行** | 194 events 无干预 |
| 10 | **Burst pattern (84% sessions) 是自动化循环的标志** | >10 turns in <60s |

### 10.2 架构的铁律

| # | 教训 | 证据 |
|---|---|---|
| 11 | **Agent 写 scratch 脚本 = 接口缺失的症状** | Session #13 根因分析 |
| 12 | **MCP 必须是 thin proxy，不是 fat adapter** | Session #13 修复方案 |
| 13 | **数据库边界必须代码强制，不能只靠文档** | hapi.db 被 710K 行实验数据污染 |
| 14 | **Python fallback 和 Rust 默认参数分歧 = 静默偏差** | Session #4 chan theory 偏差 |
| 15 | **Silent degradation 是数据管线最差的失败模式** | Vault 凭证缺失不报错 |
| 16 | **三库隔离：运营/市场数据/实验** | hapi.db / rust_gateway.db / autoresearch.db |
| 17 | **单写者原则：rust_gateway.db 只有 radar_daemon 能写** | 多进程写入导致数据损坏 |
| 18 | **Go edge 是 API 层不是计算层** | Go proxy 比 Python RPC 慢（13.56s vs 7.95s） |
| 19 | **数据新鲜度必须交易日历感知** | Wall-clock 过期在周末误判 |
| 20 | **Build artifacts 从 day one 就 .gitignore** | 1,370 个 tracked 垃圾文件 |

### 10.3 模型的铁律

| # | 教训 | 证据 |
|---|---|---|
| 21 | **Claude Code 深度模式不汇报推理过程** | 2,494 tool_use, 0 文本 |
| 22 | **Codex 自主性强但容易范围蔓延** | 中位 60 turns，67% 长任务 STALLED |
| 23 | **503 错误可以有 3+ 个独立根因** | Session #5 三层诊断 |
| 24 | **Auth 是静默失败模式** | OAuth/auth.json/Clash 三种都表现为"不回复" |
| 25 | **App 升级覆盖 patches** | Antigravity main.js 每次升级被覆盖 |
| 26 | **73.6% Codex session 只有 1 条用户消息** | Fire-and-forget 是主导模式 |
| 27 | **Hermes 85% mixed session——交易决策嵌入工程对话** | 架构决策在讨论持仓时做出 |
| 28 | **Windsurf 的 Tachi Memory 注入提供跨 session 连续性** | 13-18 条记忆/session |
| 29 | **交叉审查（GLM reviews Codex, Codex reviews GLM）解决单模型盲区** | 用户显式建立 |
| 30 | **V8 不是金标准——30% OOS precision** | 70% A 级判定是假阳性 |

---

## 十一、建议

### 11.1 长任务执行策略

| 场景 | 推荐模型 | 原因 |
|---|---|---|
| 单一明确目标的重构 | **Codex /goal** | 自主性强，5 阶段验证门，COMPLETED |
| 需要频繁确认方向的探索性任务 | **Claude Code** | 工具密度高，但需要用户主动 steering |
| 跨系统审查/知识蒸馏 | **Codex + sidecar subagent** | 主 session 控制编辑，sidecar 做只读盘点 |
| 环境调试/网络问题 | **Claude Code** | 擅长多层根因诊断 |
| 全自动无人值守 | **Codex fire-and-forget** | 只读 review 100% 完成；写任务用 Mission Directive |
| 量大管饱的代码阅读 | **GLM-5.1** | 1M 上下文，廉价，scout 角色 |
| 文档/中文 README | **Gemini** | 中文质量好 |
| 交易+工程混合 | **Hermes (GPT-5.5)** | MCP 骨架 + delegate_task |

### 11.2 防止长任务停滞的 Prompt 规则

参考 Amp 的设计，建议在 coding agent 的 system prompt 中加入：

```
1. "Scope lock: 如果执行过程中发现关联问题，记录到 TODO 但不扩大当前 scope。
    除非关联问题直接阻塞当前任务，否则不处理。"

2. "Progress checkpoint: 每 100 turns 输出一次进度摘要，包含：
    - 已完成项
    - 剩余项
    - 阻塞项
    - 发现的关联问题（不处理，只记录）"

3. "Failure budget: 如果连续 10 次操作失败，暂停并向用户报告：
    - 失败模式分类
    - 根因假设
    - 建议的下一步"

4. "Completion criteria: 在开始执行前明确定义'完成'的标准。
    如果用户没有给出，主动询问。"

5. "No silent degradation: 如果关键依赖缺失（凭证/数据/连接），
    立即报错而非降级。错误信息必须包含：
    - 缺什么
    - 怎么补
    - 影响范围"

6. "Subagent discipline: 对于只读盘点任务（dead code triage, coverage map），
    dispatch sidecar subagent。主 session 控制编辑序列。"

7. "Verification gates: 每个阶段完成后运行验证（test/lint/build），
    通过后才进入下一阶段。不跳过验证。"
```

### 11.3 架构改进建议

| 优先级 | 改进 | 预期收益 |
|---|---|---|
| **P0** | MCP 改为 thin proxy（不直接 import engine） | 消除 agent 写 scratch 脚本的动机 |
| **P0** | 数据库边界代码强制（db_paths.py + contract tests） | 防止 710K 行实验数据污染运营库 |
| **P0** | CLI 增加 `--format json --fields` | Agent 可以直接用 CLI 而非写脚本 |
| **P1** | Compaction 客户端感知（参考 OpenCode 的 prune+reserve） | 防止 token 燃烧 |
| **P1** | 凭证 readiness matrix 在 status 端点暴露 | 防止 silent degradation |
| **P1** | Python/Rust 参数对齐自动检测 | 防止静默计算偏差 |
| **P2** | Watchpoint 机制实现 | 回测速度 ×3，成本 ÷3 |
| **P2** | Virtual Capability 层完成（当前断裂） | memory-server 可编译 |
| **P2** | Remote MCP 架构 | 多客户端支持，服务端凭证隔离 |

### 11.4 与 Amp 设计的差距

| Amp 的设计 | 当前状态 | 差距 | 建议 |
|---|---|---|---|
| 9 套 prompt 对应不同模型 | 通用 prompt | **大** | 至少为 Codex/CC/Hermes 各写一套 |
| 双通道输出（commentary + final） | CC 0 文本输出 | **大** | 要求每 100 turns 输出进展 |
| Subagent 纪律 | Codex sidecar 模式已接近 | **中** | 形式化为 prompt 规则 |
| Verification 风险分级 | 所有改动跑全量测试 | **中** | 加入 "typo fix 不需要验证" |
| TODO 工具级持久化 | 对话上下文中的 TODO | **大** | 参考 Claude Code TaskCreate |
| Scaffold customization | 无 | **大** | 支持替换/追加 prompt |
| Feature flag 模型切换 | 无 | **中** | 服务端控制模型灰度 |
| 文件变更记录 (before/after JSON) | 无 | **中** | Git diff 可替代 |

### 11.5 多 Agent 编排建议

| 模式 | 适用场景 | 实现 |
|---|---|---|
| **Fire-and-forget** | 明确目标的独立任务 | 单条 Mission Directive → Codex 自主执行 |
| **Sidecar 盘点** | 需要只读分析的大任务 | 主 session 编辑 + sidecar subagent 盘点 |
| **交叉审查** | 质量保证 | GLM reviews Codex, Codex reviews GLM |
| **Scout 协议** | 大量标的的初步扫描 | 廉价模型 (Kimi/GLM) scout → 旗舰整合 |
| **Convoy 模式** | 多步复杂任务 | 协调者拆解 + 执行者只做切片 + 保持队形 |

---

## 附录 A：数据源明细

| 来源 | 目录 | Sessions | 格式 |
|---|---|---|---|
| OpenCode | `opencode/json/` | 247 (main) + 537 (sub) | `{turns, model, cost, tokens, todos, subagents}` |
| Antigravity (new) | `antigravity_json/` | 54 | `{turns, text_blocks, stats}` |
| Antigravity (legacy) | `antigravity_legacy_json/` | 100 | 同上 |
| Antigravity Stats | `antigravity_stats/` | — | `antigravity_combined_stats.json` |
| Claude Code | `claude_code/json/` | 1,071 | `{turns: [{role, content, timestamp}]}` |
| Codex | `codex/json/` | 717 | `{turns: [{role, content, timestamp}], session_meta}` |
| Hermes | `hermes/json/` | 550 | `{turns: [...], model, platform}` |
| Windsurf | `windsurf_json/cascade/` | 50 | `{turns, text_blocks, stats}` |
| Windsurf Legacy | `windsurf_legacy_json/cascade/` | 15 | 同上 |
| Qwen | `qwen/json/` | 37 | `{turns: [...]}` |

扫描索引：`coding_knowledge/scan_index.json`（576 high-value sessions 详细分类）
量化分析脚本：`quant_analysis.py`

## 附录 B：Session 分类关键词

| 类别 | 关键词 |
|---|---|
| agent_architecture | tachi, delegate, subagent, agent loop, mcp server, tool surface, hub phase |
| infra_engineering | memory-server, rust gateway, warpcore, radar_daemon, kline_cache, duckdb |
| model_behavior | compaction, context window, token consumption, model comparison, auto compact |
| debugging_playbooks | bug, fix, error, crash, timeout, root cause, diagnosis, patch |
| code_quality | refactor, code review, audit, technical debt, lint, test coverage |
| devops_deployment | docker, deploy, railway, ci/cd, cherry-pick, git push, pr |
| prompt_engineering | system prompt, scaffold, prompt design, soul.md, agents.md |
| long_task_patterns | continue, 继续, 下一步, todo, phase, 阶段, checkpoint, handoff |

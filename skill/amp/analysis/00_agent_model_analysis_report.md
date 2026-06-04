# Coding Agent 模型对比分析报告

> 日期：2026-05-30 | 数据来源：opencode SQLite DB (768 sessions, 44,628 messages) + Antigravity JSON logs + Amp 二进制逆向

---

## 一、OpenCode 模型使用统计（近 5 天）

### Build Session 核心指标

| 模型 | Sessions | 平均时长 | 最大时长 | 平均 Tokens (K) | 平均 TODO | TODO 完成率 | 费用 |
|---|---|---|---|---|---|---|---|
| GPT 5.5 | 16 | 154 min | 1007 min | 627 | 5.3 | **75.5%** | $0 (Copilot) |
| Claude Sonnet 4.6 | 3 | 81 min | 104 min | 390 | 8.0 | **91.7%** | $0 (Copilot) |
| Claude 4.7 | 5 | 174 min | 472 min | 572 | 5.4 | 39.7% | $0 (Copilot) |
| Claude 4.8 | 1 | 74 min | 74 min | 81 | 6.0 | 53.1% | $0 (Copilot) |
| DeepSeek V4 Pro | 3 | 380 min | 993 min | 1,724 | 7.7 | 47.8% | $35.73 |
| Qwen 3.7 Max | 2 | 182 min | 267 min | 7,382 | 3.5 | 64.9% | $184.59 |
| GLM 5.1 | 2 | 252 min | 443 min | 689 | 3.0 | 83.8% | $0.05 |

### Subagent 使用模式

| 模型 | Subagent 派生率 | 特征 |
|---|---|---|
| Qwen 3.7 Max | **93.9%** | 几乎什么都派 subagent，但主 session 0/5 TODO 完成 |
| Claude 4.8 | 88.9% | 高派生率，但样本少（1 build session） |
| GLM 5.1 | 81.8% | 中等 |
| Claude 4.7 | 64.3% | 选择性派生 |
| GPT 5.5 | 54.3% | 最均衡，派了就用 |
| DeepSeek V4 Pro | 50% | 保守派生 |

---

## 二、TODO 丢失的根因

### OpenCode 的 Compaction 机制

- **触发**：context 占到模型窗口 95%
- **方法**：把整段对话发给 summarizer agent 生成纯文本摘要，截断消息列表
- **TODO 处理**：完全没有结构化保护，靠 LLM 摘要时"碰巧"提到

### Claude Code 的 Compaction 机制

- TaskCreate/TaskUpdate/TaskList 是**工具级持久化**，独立于对话上下文
- Compaction 只压缩对话，任务列表天然不受影响

### 为什么 GPT 5.5 不丢 TODO

| 模型 | 上下文窗口 | 最大 build session tokens | 是否触发 compaction |
|---|---|---|---|
| GPT 5.5 | ~1M | 1.53M | 极少 |
| Qwen 3.7 Max | ~1M | **11.3M** | 极少 |
| DeepSeek V4 Pro | ~1M | 3.68M | 极少 |
| Claude 4.8 | 200K | 80K | **频繁** |

**结论**：200K 窗口是 Claude 在 opencode 丢 TODO 的根本原因。1M 窗口模型几乎不触发 compaction，问题被掩盖。

### 建议

在 opencode 用 Claude 时，prompt 里要求 "把 TODO 列表写在每条回复开头"，增加摘要包含 TODO 的概率。或大任务优先用 1M 窗口模型。

---

## 三、国产模型 vs 海外模型：行为差距

### 3.1 规划 vs 执行

| 模型 | 100% 完成的 session 占比 | 典型失败模式 |
|---|---|---|
| GPT 5.5 | **81%** (13/16) | 极少半途而废 |
| Claude Sonnet 4.6 | **100%** (3/3) | 样本少但完美 |
| DeepSeek V4 Pro | 33% (1/3) | "HyperTachi三层架构落地" 11 TODO / 0 完成 / 383 消息 |
| Qwen 3.7 Max | 50% (1/2) | "整理 antigravity session" 5 TODO / 0 完成 / 618 消息 |

**关键发现**：国产模型遇到复杂任务时倾向疯狂规划/分析/拆解，但不主动推进执行。实际是用户主动叫停（怕"干岔劈了"），不是模型不能干。

### 3.2 信息密度

| 模型 | 每条消息输出 tokens | 每完成一个 TODO 需要几条消息 |
|---|---|---|
| Claude Sonnet 4.6 | 347 | 20 |
| GPT 5.5 | 219 | 26 |
| DeepSeek V4 Pro | 283 | 35 |
| **Qwen 3.7 Max** | **457** | **124** |
| **GLM 5.1** | 169 | **140** |

### 3.3 Explore 产出比（读多少代码 vs 提炼多少结论）

| 模型 | 任务 | 输入 tokens | 输出 tokens | 产出比 |
|---|---|---|---|---|
| **Qwen** | Review HyperTachi upstream bugs | **493K** | 6.9K | **1.4%** |
| **Qwen** | Analyze memory-core capabilities | **232K** | 1.9K | **0.8%** |
| GPT 5.5 | Audit HyperTachi fixes | 132K | 6.7K | **5.0%** |
| GPT 5.5 | Map HyperTachi core | 176K | 7.3K | **4.2%** |

Qwen 读 49 万 token 代码提炼 6900 token 结论；GPT 读 13 万 token 提炼出几乎等量的结论。

### 3.4 用户实际使用策略

> "是我没让他们干。我怕他们干岔劈了。看他们说的话很多都不在点子上，只能用来跑跑 review、需要长上下文的 explore"

国产模型的实际定位：**粗扫工具**（利用 1M 上下文 + 低价大量阅读代码），而非精确执行器。

---

## 四、Antigravity 对话质量对比

### Gemini 3.1 Pro（Antigravity 新版）

**案例：用户说"今天亏惨咯"**

1. 调 `tachi_memory` 搜 `sticky_high_conviction`、`trading纪律手操`
2. 拉实际持仓数据，逐只股票拆解
3. 用项目术语出诊断表——"冲动首仓"、"越跌越接"、"杠杆放大焦虑"
4. 搞混了兆易创新和风华高科数据 → 被质疑后拆了底层原因："注意力机制对高信息密度 JSON 的吸附效应"
5. 讲"鱼尾必跌"时引用 `intraday_volume_structure` 里 `escape_signal: true` —— 来自 Rust 代码的实际算法结论

### Claude 4.6（Antigravity Legacy）

**案例：用户贴了工业富联会议纪要**

1. 立刻跑 V8 引擎扫描这只股票的技术面
2. 调 `pm.get_position_diff()` 拉实际持仓 → **发现自己犯了错**（建议买入已持有 16000 股的票）→ 自己发现并修正
3. 重新推演后给出具体操作建议：回踩 51.5-52.0 加仓，砍亿纬锂能抽血
4. 又犯一个错（基于 20 日均值判寒武纪为"弱鸡"）→ 被质疑后定位认知错误根源："底层算法被 20 天死板均线回撤糊住了眼睛"

**案例：缠论 Rust 覆盖度讨论**

> "这不是要修的 bug，是有意的工程取舍。`seg.rs` 第29行 `#[pyo3(signature = (seg_algo="chan", left_method="peak"))]`，当时 Claude Code 用 `BiLike` trait 让 `CBi` 和 `CSeg` 共享同一套 14 个方法的接口。95% 场景够用。"

知道具体到哪一行代码、当初为什么这么设计、设计者的意图。

### 差距总结

| 维度 | Claude 4.6 | Gemini 3.1 Pro | 国产模型 |
|---|---|---|---|
| 读代码精度 | 知道设计意图 | 读得准 | 只看到表面事实 |
| 行动性 | 自己发现问题自己修 | 被纠正后能深度反思 | 等你指令 |
| 自驱力 | 从 hi 到 1352 turns | 从 hi 到 678 turns | 需要明确 prompt |
| 错误诊断 | "底层算法被 X 糊住了眼睛" | "注意力机制吸附到了 X" | "您说得对" |
| 领域锚定 | 用你的系统术语编织分析 | 能调你的数据出结论 | 写一篇关于你代码的文档 |

---

## 五、Amp 的 System Prompt 分析

### 5.1 核心设计原则

来源：`~/.amp/bin/amp` 二进制逆向提取，三个模式（Autonomous / Extended / Pair Programming）

**自驱力**：
> "Carry the work through **implementation and verification** rather than stopping at a proposal. Unless the user is brainstorming, assume they want you to **solve the problem**, not describe a proposed solution."

**容错自愈**：
> "If an approach fails, **diagnose why before switching tactics**. Don't retry blindly, but don't abandon after a single failure either."

**最小改动**：
> "The best change is often the **smallest correct change**. Prefer existing patterns, frameworks, and local helper APIs over inventing a new abstraction."

**读代码纪律**：
> "Read enough code to **avoid guessing, then stop**. Each read should answer a specific uncertainty. Once clear, move to the edit."

**验证风险分级**：
> "Verification scales with risk: typo fix needs none, localized change needs targeted check, shared/cross-module changes need broader coverage."

**反 AI slop**：
> "No default system fonts (Inter, Roboto), no purple gradient bias, no decorative animation without purpose."

### 5.2 多 Agent 协作架构

Amp 有三种 subagent，各自有明确的使用场景和 prompt 策略：

#### Oracle（高级工程顾问）

- **定位**：Senior engineering advisor with GPT-5.5 reasoning model
- **用于**：Code reviews, architecture decisions, performance analysis, complex debugging, planning Task runs
- **不用于**：Simple file searches, bulk code execution
- **Prompt 要点**："Prompt it with a precise problem description and attach necessary files. Ask for concrete outcomes and request trade-off analysis."

#### Task Tool（执行苦力）

- **定位**：Fire-and-forget executor for heavy, multi-file implementations. "Productive junior engineer who can't ask follow-ups"
- **用于**：Feature scaffolding, cross-layer refactors, mass migrations, boilerplate generation
- **不用于**：Exploratory work, architectural decisions, debugging analysis
- **Prompt 要点**："Give detailed instructions, enumerate deliverables, step-by-step procedures, constraints, and relevant context."

#### Codebase Search Agent（代码探索器）

- **定位**：Smart code explorer that locates logic based on conceptual descriptions across languages/layers
- **用于**：Mapping features, tracking capabilities, finding side-effects by concept
- **不用于**：Code changes, design advice, simple exact text searches
- **Prompt 要点**："Prompt with the real-world behavior you're tracking. Give hints with keywords, file types, directories."

#### 并行执行规则

> "Default to **parallel** for all independent work: reads, searches, diagnostics, writes and **subagents**."

| 可并行 | 必须串行 |
|---|---|
| Oracle 的不同关注点（架构审查 / 性能分析 / 竞态调查） | Plan → Code（规划完成后才能编辑） |
| 不同路径的 Codebase Search | 同一文件的多个 Task（写冲突） |
| 写目标不重叠的多个 Task | 链式变换（B 依赖 A 的产物） |

#### 选择决策树

```
"我需要一个高级工程师跟我一起想" → Oracle
"我需要找到匹配某个概念的代码" → Codebase Search Agent
"我知道要做什么，需要大规模多步执行" → Task Tool
```

#### Subagent 使用纪律

> "Do not spawn a subagent for work you can complete directly in a single response (e.g., editing one file, running one search)."
>
> "Each subagent loses your context, so include everything it needs in the prompt: the plan, relevant file paths, coding conventions, and how to verify its work."
>
> "Avoid duplicating work that subagents are already doing."

### 5.3 TODO 工具设计

> "You plan with a todo list. Track your progress and steps. A good todo list breaks the task into meaningful, logically ordered steps that are easy to verify."
>
> "MARK todos as completed as soon as you are done. **Do not batch up multiple tasks before marking them as completed.**"

---

## 六、实操建议

### 模型选择

| 场景 | 推荐模型 | 原因 |
|---|---|---|
| 从头到尾干完的任务 | GPT 5.5 | 81% session 100% 完成，Copilot 免费 |
| Rust/Go 精确实现 | Claude 4.7 | 代码质量最高 |
| 搜索/探索/调研 | Qwen 3.7 Max | 93.9% subagent 派生率，1M 上下文 |
| 规划/分析/审计 | DeepSeek V4 Pro | 规划很细（11 TODO），但需推着执行 |
| 便宜跑杂活 | GLM 5.1 | $0.02/session |

### System Prompt 优化

如果能在 opencode 里自定义 agent system prompt，建议参考 Amp 的设计：

1. **明确"默认执行"**："Unless explicitly asked for a plan, implement the solution directly"
2. **限制 subagent 使用**："Do not spawn subagents for tasks you can complete in a single response"
3. **要求自我诊断**："If an approach fails, diagnose why before switching tactics"
4. **规范 TODO 管理**："Mark todos as completed immediately after finishing each one"

### 1M 上下文能否抹平差距

| 能解决 | 不能解决 |
|---|---|
| Compaction 导致的 TODO 丢失 | 代码质量差距 |
| 长任务上下文断裂 | 任务规划精度 |
| — | 领域锚定能力（把知识挂钩到具体场景） |
| — | 自我纠正的诊断价值 |
| — | 从"读到信息"到"给出行动指令"的跳跃 |

---

## 七、Amp 二进制架构深度分析

### 7.1 本地二进制是什么

**Bun 编译的 JavaScript 单体应用**，67MB Mach-O arm64：

- 运行时：Bun 的 JavaScriptCore (JSC) 引擎 + 完整 Node.js 兼容层
- 打包方式：所有 JS/TS 源码被编译进二进制，通过 `Bake::BakeLoadInitialServerCode` 引导启动
- 依赖：内置 Gemini SDK、Zod schema 校验、完整的 git 操作库、MCP client、HTTP cache
- 版本检查：通过 `static.ampcode.com/cli/cli-version.txt` 检测更新

### 7.2 本地 vs 服务端的分工

| 功能 | 执行位置 | 说明 |
|---|---|---|
| System Prompt 生成 | **本地** | 9 个 prompt 模板 + 模型/模式路由逻辑全在二进制里 |
| Tool 定义和注册 | **本地** | 12 个内置工具 + MCP 工具动态注册 |
| Tool 执行（bash/edit/file） | **本地** | 文件系统操作、shell 执行全在本地 |
| 对话历史存储 | **服务端** | Thread ID 格式 `T-{UUIDv7}`，消息全部上传 ampcode.com |
| 模型推理 | **服务端代理** | 本地不调模型 API，所有 inference 请求经服务端中转 |
| MCP 管理 | **本地** | `amp mcp add/remove/approve` 操作本地配置 |
| 权限控制 | **本地** | `dangerouslyAllowAll` 或按工具白名单 |
| 文件变更记录 | **本地** | `~/.amp/file-changes/{thread_id}/` 存 JSON diff |

**关键洞察**：Amp 是 **thin client + server-side inference** 架构。本地只管 prompt 组装、tool 执行、MCP；所有模型调用和对话持久化在服务端。这解释了为什么本地看不到对话历史。

### 7.3 Agent Loop 工作流

```
用户输入
  ↓
本地组装 system prompt（根据 model + agentMode 选模板）
  ↓
发送到 ampcode.com 服务端（带 prompt + 工具列表 + 上下文）
  ↓
服务端调模型 API（GPT-5.5 / Claude / Gemini / Kimi）
  ↓
流式返回 response + tool_calls
  ↓
本地执行 tool（bash / edit / file / MCP / oracle / task / search）
  ↓
把 tool_result 发回服务端
  ↓
重复直到模型输出结束（无 tool_call）
  ↓
对话历史全部保存在服务端
```

### 7.4 System Prompt 路由（`NFR` 函数）

```
agentMode === "aggman"    → _FR()  // Aggman 模式（快速简单任务）
agentMode === "rush"      → qFR()  // Rush 模式（快速执行）
agentMode === "deep"      → sFR()  // Deep Autonomous（GPT-5.5）
agentMode === "deep" + FF → nFR()  // Deep Fallback（GPT-5.4）
provider === "openai"     → yFR()  // GPT 模式
model === "gpt-5-codex"   → kFR()  // Codex 模式
provider === "xai"        → SFR()  // xAI 模式
model === "kimi-k2"       → mFR()  // Kimi 模式
provider === "vertexai"   → lFR()  // Gemini 模式
default                   → oFR()  // Pair Programming（通用）
```

**发现**：Amp 有 **9 套不同的 system prompt**，根据 agentMode 和模型动态切换。每个 prompt 针对特定模型的特性优化过。

### 7.5 工具体系

**12 个内置工具**：

| 工具 | 类型 | 说明 |
|---|---|---|
| `read_file` | 文件 | 读文件内容 |
| `write_file` | 文件 | 创建/覆写文件 |
| `edit_file` | 文件 | 精确编辑文件 |
| `create_file` | 文件 | 创建新文件 |
| `delete_file` | 文件 | 删除文件 |
| `list_directory` | 导航 | 列目录 |
| `bash` / `shell_command` | 执行 | 运行 shell 命令 |
| `todo_read` | 管理 | 读取 TODO 列表 |
| `todo_write` | 管理 | 写入/更新 TODO |
| `oracle` | 子 Agent | GPT-5.5 高级工程顾问 |
| `task_tool` | 子 Agent | fire-and-forget 执行器 |
| `codebase_search` | 子 Agent | 概念化代码搜索 |
| + MCP 工具 | 扩展 | 动态注册的外部工具 |

**工具过滤逻辑**：根据 agentMode 动态启用/禁用工具：
- `enableTask` 控制 `task_tool` 可用性
- `enableOracle` 控制 `oracle` 可用性
- `enableDiagnostics` 控制诊断工具
- `enableChart` 控制图表工具

### 7.6 文件变更记录格式

`~/.amp/file-changes/{thread_id}/` 下每个文件是 JSON：

```json
{
  "id": "uuid",
  "uri": "file:///path/to/file",
  "before": "原始内容...",
  "after": "修改后内容..."
}
```

直接存 before/after 全文 diff，不是 patch 格式。这让用户可以回溯每次文件修改。

### 7.7 Amp 值得学习的设计

#### 1. 多模式 Agent 架构

Amp 不只是一个 agent，而是一组针对不同场景的 agent：

- **Deep（自主模式）**：长任务，自动推进，最小化用户交互
- **Rush（快速模式）**：快速简单任务
- **Aggman**：Slack 集成的协作模式
- **Pair Programming**：结对编程，每步确认

每种模式有独立的 system prompt 和工具集。这个设计让同一个 CLI 在不同场景下自动切换行为模式。

#### 2. Prompt-Model 联合优化

不是一套 prompt 跑所有模型，而是 **9 套 prompt 对应不同模型**：

```
GPT-5.5 → deep prompt（强调自主推理和最小改动）
Gemini → gemini prompt（强调结构化输出和精确性）
Kimi → kimi prompt（针对长上下文优化）
通用 → pair programming prompt（强调用户协作）
```

这意味着 Amp 团队为每个模型做了 prompt tuning，而 opencode/Claude Code 用的是通用 prompt。

#### 3. 子 Agent 纪律

```
Oracle:  "高级工程师跟你一起想" → 只用于 review/architecture/debug
Task:    "我知道要干啥，帮我跑"  → 只用于大规模执行
Search:  "找到匹配概念的代码"   → 只用于代码探索

规则：
- 不要为单条回复就能完成的工作派子 agent
- 每个 subagent 丢失你的上下文，prompt 里要包含所有必要信息
- 不要重复子 agent 正在做的工作
- 并行执行独立的子 agent
```

#### 4. Scaffold Customization

```json
{
  "systemPrompt": {
    "type": "replaceAll" | "replaceBase",
    "value": "自定义 prompt..."
  },
  "enableToolSpecs": [{"name": "oracle"}],
  "disableTools": ["bash"]
}
```

用户可以通过 scaffold 文件完全替换或追加 system prompt，同时精确控制启用哪些工具。这比 opencode 的 `oh-my-openagent.jsonc` 灵活得多。

#### 5. Feature Flag 驱动的模型升级

```javascript
// Deep 模式下动态选择 GPT-5.5 还是 5.4
if (agentMode === "deep") {
  let canUseGPT55 = ATR(serverStatus);  // 服务端 feature flag
  return canUseGPT55 ? "deep" : "deep-gpt5.4";
}
```

模型选择不是写死的，而是通过服务端 feature flag 动态控制。这让 Amp 能灰度发布新模型，遇到问题秒级回滚。

#### 6. MCP 集成的工程细节

- **OAuth 支持**：`amp mcp oauth login/logout`，支持 OAuth 认证的 MCP 服务器
- **自动发现**：从 MCP Registry 拉取可用服务器列表
- **权限隔离**：每个 MCP 服务器需要单独 approve（`amp mcp approve`）
- **延迟加载**：MCP 工具标记 `deferred: true`，按需激活

### 7.8 Amp 的局限性

| 方面 | 问题 |
|---|---|
| 对话隐私 | 所有对话上传 ampcode.com，无法本地部署 |
| Prompt 不可见 | 用户看不到实际使用的 prompt（需逆向二进制） |
| 二进制体积 | 67MB 单体，每次更新全量下载 |
| 自定义受限 | 只能通过 scaffold 文件修改，不能像 opencode 那样完全控制 |
| 本地无历史 | 无法离线回溯对话，只能看文件 diff |

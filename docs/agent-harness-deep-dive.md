# Agent Harness 深度分析：TODO · Subagent · Compaction · Handoff

**Status:** Research Complete
**Date:** 2026-06-03
**Sources:**
- `~/.amp/bin/amp` 二进制逆向（67MB Bun SEA）
- Claude Code v2.1.150 内部机制分析
- OpenCode v0.44.1 逆向
- Codex CLI v0.135.0 分析
- 2,841 sessions / 278,000+ turns / 2,014+ 小时实证数据

---

## 1. 核心结论（TL;DR）

### 1.1 CLI Dispatch 的致命限制

所有非交互式 CLI 模式（`claude -p`、`codex exec`、`gemini -p` 等）都是 **Worker-only**：

| 功能 | 交互式 TUI | CLI `-p` / `exec` |
|------|-----------|-------------------|
| Persistent TODO | ✅ TaskCreate 工具 | ❌ 进程退出即消失 |
| Subagent spawn | ✅ `/spawn` 命令 | ❌ 不支持 |
| Multi-turn iterate | ✅ 自动循环 | ❌ single-shot |
| Session memory | ✅ JSONL 累积 | ❌ 每轮独立 |
| Handoff | ✅ memory 系统 | ❌ 无原生支持 |

**这意味着：Tachi 必须自己做 Orchestrator。** Agent 只能做无状态执行，状态（TODO、进度、上下文）必须由 Tachi 维护。

### 1.2 TODO 必须工具级持久化

```
TODO 存在对话上下文里 → compaction 压缩对话 → TODO 丢失 ❌
TODO 存在独立工具/数据结构 → compaction 只压对话 → TODO 保留 ✅
```

| Agent | TODO 存储 | Compaction 影响 |
|-------|----------|----------------|
| **Claude Code** | TaskCreate/TaskUpdate 工具 | ✅ 不受影响 |
| **Amp** | todo_read/todo_write 工具 | ✅ 不受影响 |
| **OpenCode** | SQLite `todo` 表 | ❌ 大概率丢失（摘要无保护） |
| **Codex CLI** | 无 | N/A |

### 1.3 长任务成功率极低

>500 turns 的 mega session：
- **COMPLETED: 13%** (2/15)
- **STALLED: 67%** (10/15)
- **EXPLORATORY: 20%** (3/15)

**最优自主区间：Steering Ratio 0-5%**（用户消息 / 总 turn）。超过 10% 的长任务全部停滞。

---

## 2. 四大 Agent Harness 机制对比

### 2.1 Claude Code（Anthropic）

#### Session 存储
```
~/.claude/projects/{project}/conversation.jsonl   # 完整对话历史
~/.claude/settings.json                           # 全局设置
~/.claude/.mcp.json                               # MCP 配置
```

#### TODO 系统：工具级持久化

```typescript
// 不是 prompt 里的文本，是独立工具调用
TaskCreate: { content: string, status: "in_progress" | "done" }
TaskUpdate: { id: string, status: "done" }
TaskList:   {}  // 随时查看
```

**关键特性：**
- Task 工具独立于对话上下文
- Compaction 只压缩对话文本，Task 数据不受影响
- Prompt 规则："Mark completed immediately"（不 batch）

#### Subagent：通用 Agent 工具

```typescript
Agent: {
  prompt: string,           // 子任务描述
  tools: string[],          // 子 agent 可用工具
  cwd: string               // 工作目录
}
```

- Subagent 是 **通用型** — 不是专用角色
- 同步阻塞，等 subagent 完成
- 结果返回给 parent session

#### Compaction

- **触发：** 上下文接近窗口限制时自动触发
- **方法：** 将完整对话发送给 summarizer agent，生成结构化摘要
- **恢复：** `claude --continue` 或 `--resume`
- **TODO 保护：** ✅ 不受影响（工具级）

#### Handoff

- **无原生 handoff 机制**
- 通过 `--continue` 恢复 session + memory 系统实现跨 session 追踪

---

### 2.2 Amp（Amp Code）

#### Session 存储

```
服务端：ampcode.com（全部对话历史在云端）
本地：~/.amp/file-changes/{thread_id}/   # 仅文件 diff
```

Thread ID 格式：`T-{UUIDv7}`

#### TODO 系统：工具级 + Prompt 规则

```typescript
todo_read:  {}  // 读取当前 TODO
todo_write: { items: TodoItem[] }  // 写入 TODO
```

**Prompt 规则：**
> "MARK todos as completed as soon as you are done. **Do not batch up.**"

#### Subagent：三种专用角色

| Subagent | 定位 | 用途 | 不用于 |
|----------|------|------|--------|
| **Oracle** | 高级工程顾问 | Code review, architecture, debugging, planning | Simple search |
| **Task Tool** | 执行苦力 | Feature scaffolding, mass migration, boilerplate | Exploratory work |
| **Codebase Search** | 代码探索器 | Conceptual search across languages/layers | Code changes |

**工作流：**
```
Oracle (plan) → Codebase Search (validate scope) → Task Tool (execute)
```

**并行规则：**
```
可并行：
- Oracle 的不同关注点（架构 / 性能 / 竞态）
- 不同路径的 Codebase Search
- 写目标不重叠的多个 Task
- Reads / Searches / Diagnostics

必须串行：
- Plan → Code
- 同一文件的多个 Task
- 链式变换（B 依赖 A 的产物）
```

#### Compaction

- **触发：** 服务端管理（`latestCompactionCutIndex` 等函数）
- **方法：** 服务端执行，本地不感知
- **恢复后 Prompt 指令：**
  > "If the conversation was compacted, continue from the summary; don't restart."
  > "Before finalizing after an interrupt or context compaction, verify your answer addresses the newest request."
- **TODO 保护：** ✅ 不受影响

#### Handoff：Thread 工作流

```typescript
continue_thread:  { thread_id: string }     // 继续现有 thread
read-thread:      { thread_id: string }     // 用 subagent 阅读其他 thread
archive_thread:   { thread_id: string }     // 归档 thread
unarchive_thread: { thread_id: string }     // 恢复 thread
report_back:      { thread_id: string, summary: string }  // Callback
```

**Aggman 模式**有完整的 thread 工作流：
1. 发现 threads
2. 阅读 thread 内容
3. 创建/回复 thread
4. 合并/审查
5. Callback 通知主 thread

---

### 2.3 OpenCode

#### Session 存储

```
~/.local/share/opencode/opencode.db   # SQLite 数据库
```

#### TODO 系统：SQLite 但无 Compaction 保护

```sql
-- SQLite `todo` 表
CREATE TABLE todo (...);
```

**问题：**
- TODO 存在 SQLite 里
- Compaction 时 summary prompt 只说 "focus on what we did/doing/will do next"
- **没有明确要求包含 TODO 列表**
- 恢复后 TODO 信息大概率丢失

**为什么 GPT-5.5 不丢 TODO：**
- 1M 上下文几乎不触发 compaction
- 问题被窗口大小掩盖了

#### Subagent：agent 工具（read-only）

```typescript
agent: {
  prompt: string,
  tools: ["Glob", "Grep", "LS", "View"],  // 只读工具
}
```

- Subagent 是 **read-only** — 不能编辑文件
- 同步阻塞
- `parent_id` 机制指向主 session

#### Compaction

- **触发：** 上下文占 95% 窗口时触发
- **方法：** summarizer agent 生成纯文本摘要，截断消息列表
- **TODO 保护：** ❌ 大概率丢失

#### Handoff

- `parent_id` 机制
- Subagent 结果需要手动查看

---

### 2.4 Codex CLI（OpenAI）

#### Session 存储

**无持久化。** `codex exec` 是 stateless CLI。

#### TODO 系统

**无。** Codex CLI 没有 TODO 工具。

#### Subagent

**无。** Codex CLI 不支持 subagent。

#### Compaction

**无 autocompaction。** 上下文超限时直接截断旧消息。

#### Handoff

**不支持。**

---

### 2.5 对比矩阵

| | Claude Code | OpenCode | Amp | Codex CLI |
|---|---|---|---|---|
| **Compaction** | 自动，TODO 不受影响 | 自动，TODO 丢失 | 服务端，TODO 不受影响 | 无，直接截断 |
| **TODO 存储** | TaskCreate 工具 | SQLite 但无保护 | todo_read/write 工具 | 无 |
| **TODO Prompt 规则** | "Mark completed immediately" | 无明确规则 | "Do not batch up" | 无 |
| **Session** | 本地 JSONL | 本地 SQLite | 服务端 | 无 |
| **Subagent** | Agent 工具（通用） | agent 工具（只读） | 3 种专用 agent | 无 |
| **Subagent 并行** | 不支持 | 不支持 | 精确控制 | 无 |
| **Handoff** | memory 系统 | parent_id | Thread + callback | 无 |
| **Handoff 跨 session** | ✅ `--continue` | ✅ `--resume` | ✅ Thread ID | ❌ |
| **恢复后指令** | 无特殊 | 无特殊 | "Don't restart" | 无 |

---

## 3. Amp Prompt 路由 & 模型选择

### 3.1 9 套 Prompt 对应不同模型+模式

```javascript
// NFR 函数（Prompt 路由）
agentMode === "aggman"    → _FR()   // Slack 集成、项目管理
agentMode === "rush"      → qFR()   // 快速执行
agentMode === "deep"      → sFR()   // GPT-5.5 Deep Autonomous
agentMode === "deep" + FF → nFR()   // GPT-5.4 Deep Fallback
provider === "openai"     → yFR()   // GPT 通用
model === "gpt-5-codex"   → kFR()   // Codex 专用
provider === "xai"        → SFR()   // xAI
model === "kimi-k2"       → mFR()   // Kimi
provider === "vertexai"   → lFR()   // Gemini（可选 oracle/diagnostics）
default                   → oFR()   // Pair Programming
```

### 3.2 Feature Flag 模型升级

```javascript
if (agentMode === "deep") {
  let canUseGPT55 = ATR(serverStatus);  // 查服务端 feature flag
  return canUseGPT55 ? "deep" : "deep-gpt5.4";
}
```

- 灰度发布：先内部用户 → 逐步扩大
- 秒级回滚：关 flag 立刻切回旧模型
- 用户无感知，CLI 不需要更新

### 3.3 双通道输出

```
commentary channel → 实时进展（1-2 句话）
final channel      → 最终结果（精炼）
```

规则：
- commentary 只在**改变用户理解**时发送（发现、决策、阻碍、计划）
- 不播报例行操作（搜文件、读代码）
- 相关进展合并成一条

### 3.4 Scaffold Customization

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

---

## 4. 长任务成败分析（>500 turns）

### 4.1 数据总览

| 指标 | 数值 |
|------|------|
| >500 turns sessions | 15 |
| COMPLETED | 2 (13%) |
| STALLED | 10 (67%) |
| EXPLORATORY | 3 (20%) |

### 4.2 失败五种模式

#### 模式 A：范围蔓延 (Scope Creep) — 40%

**案例：** "先 commit 和 push 一下" → 2,920 turns, 42h

```
commit → CI 挂了 → 修 CI → 目录结构乱 → 重构 → 缺功能 → 加功能 → bug → ...
```

**根因：** 没有 TODO 结构约束，每个"顺便修一下"都是新的 rabbit hole。

#### 模式 B：环境泥潭 (Infra Quagmire) — 27%

**案例：** "Antigravity 不回复" → 2,700 turns, 15h

```
查日志 → 503 → 3 个根因 → 修 1 → 暴露 2 → 修 2 → 暴露 3 → ...
```

**根因：** 多独立根因同时存在，修一个暴露另一个。Agent 缺乏"足够好了"的判断力。

#### 模式 C：方向摇摆 (Pivot Loop) — 13%

**案例：** "审阅 GLM harness" → 1,662 turns, 14h

```
审阅 → 重写 → WebSocket 连不上 → 修连接 → API KEY 无效 → 修 auth → ...
```

**根因：** 用户在执行中改变需求方向，agent 无法 push back。

#### 模式 D：信息黑洞 (Information Black Hole) — 13%

**案例：** "OpenClaw auto-compaction 分析" → 2,588 turns, 0 assistant 文本

```
2,494 次 tool_use，0 条 assistant 文本回复
用户发了 94 条技术分析 → agent 全在 tool_use 里 → 用户看不到进展
```

**根因：** Agent 大量执行工具但不产出可见结论。

#### 模式 E：全自动空转 (Autonomous Spin) — 7%

**案例：** 1,000 turns, 0 用户消息, 50 assistant 消息

**根因：** 通过注入 prompt 启动全自动执行，但缺少外部 feedback 校正方向。

### 4.3 成功解剖：Session #7 (db.rs 拆分)

**1,631 turns, 10 分钟, COMPLETED**

**为什么成功：**
1. **单一目标** — "拆 db.rs"，无歧义
2. **33 条短指令**（平均 51 chars），只给方向不解释
3. **成功/失败比 1.8**（38:21），正反馈循环
4. **有 subagent 协作** — 独立审查 agent
5. **10 分钟完成** — 不是 10 小时

### 4.4 关键相关性

**Steering Ratio 与完成率：**

| Steering Ratio | 完成率 |
|---|---|
| 0-2% | **3.5%** (最高) |
| 2-5% | 2.8% |
| 5-10% | 1.2% |
| 10%+ | **0%** |

> **更多 steering ≠ 更好结果。** 31+ 条用户消息的长任务，没有一个完成。

---

## 5. 对 Tachi 的启示

### 5.1 Tachi 必须实现的 Harness 功能

因为所有 CLI agent 都是无状态的，Tachi 必须自己提供：

| 功能 | 实现方式 | 状态 |
|------|---------|------|
| **Persistent TODO** | Kanban card (`/kanban/`) + `tachi_task` | ✅ 已有 |
| **Subagent dispatch** | `tachi_dispatch()` + `tachi_complete()` | ✅ 已有 |
| **Session context** | `tachi_memory briefing` + `tachi_recall` | ✅ 已有 |
| **Handoff** | `handoff_ops.rs` + `tachi_handoff` | ✅ 已有 |
| **Compaction 保护** | Memory 工具独立于对话 | ✅ 已有 |
| **状态机流转** | #150 Epic: issue → doc → skill → dispatch → wiki | 🔨 规划中 |

### 5.2 必须避免的陷阱

#### 陷阱 1：范围蔓延

**对策：**
- Issue 创建时强制写 `## Spec` section（明确 scope）
- `tachi_shell` stage = `plan` 时必须输出验收标准
- Agent 输出超过 N turns 无进展 → 自动暂停，要求用户确认

#### 陷阱 2：环境泥潭

**对策：**
- `tachi_dispatch` 的 `max_turns` 限制（默认 100）
- 环境问题识别：如果连续 3 个 turn 都是 infra/debug 类工具调用 → 标记为"环境泥潭"
- 自动建议："是否缩小范围到单一根因？"

#### 陷阱 3：方向摇摆

**对策：**
- `tachi_shell` 的 `plan` stage 输出必须用户确认后才能进入 `dispatch`
- Kanban card 的 `title` 变更需要显式用户确认
- Agent 检测到需求变更时："检测到需求变更，是否创建新 issue？"

#### 陷阱 4：信息黑洞

**对策：**
- `tachi_dispatch` 实时 stream 到 kanban card（Multica 的 `Messages` channel 模式）
- 每 N turns 必须产出可见结论（summary），否则暂停
- `tachi_complete` 的 `notes` 字段强制要求产出摘要

#### 陷阱 5：全自动空转

**对策：**
- 0 用户消息的全自动任务必须有 `max_turns` 硬限制
- 每 N turns 必须 checkpoint，等待用户确认或自动继续
- Steering ratio 监控：超过阈值自动降速

### 5.3 借鉴 Amp 的具体设计

| Amp 设计 | Tachi 借鉴 |
|----------|-----------|
| 3 种专用 subagent（Oracle/Task/Search） | Agent Router 根据 task 类型自动路由到不同 agent |
| 双通道输出（commentary + final） | `tachi_dispatch` stream 实时状态 + `tachi_complete` 最终摘要 |
| Feature flag 模型路由 | `TACHI_BACKEND_*_TIER` env 变量 |
| 并行执行精确控制 | Kanban card 的 `disjoint_writes` 标记 |
| Thread callback（report_back） | `tachi_complete` 后自动更新 kanban + 通知 parent task |

### 5.4 借鉴 Claude Code 的具体设计

| Claude Code 设计 | Tachi 借鉴 |
|-----------------|-----------|
| TaskCreate/TaskUpdate 工具 | Kanban `post_card`/`update_card` |
| `--continue` session 恢复 | `tachi_memory briefing` 跨 session 恢复 |
| Agent 工具 spawn subagent | `tachi_dispatch` + `tachi_complete` |
| Compaction 不影响 TODO | Memory 工具独立于对话（已实现） |

---

## 6. 附录：原始数据来源

### 6.1 Amp 二进制逆向

```
~/.amp/bin/amp                              # 67MB Bun SEA binary
~/.amp/file-changes/{thread_id}/            # File diff storage
```

提取的 prompt 函数：`sFR`, `nFR`, `oFR`, `qFR`, `_FR`, `yFR`, `kFR`, `SFR`, `lFR`, `mFR`

### 6.2 Claude Code 分析

```
/opt/homebrew/Caskroom/claude-code/2.1.150/claude   # Bun SEA binary
~/.claude/projects/{project}/conversation.jsonl      # Session 存储
```

### 6.3 OpenCode 分析

```
~/.local/share/opencode/opencode.db   # SQLite 数据库
~/.config/opencode/config.yaml        # 配置
```

### 6.4 Codex CLI 分析

```
~/.npm-global/bin/codex               # Node.js ESM
~/.codex/config.toml                  # 配置
~/.codex/agents/*.toml                # 21 个预定义 agent
```

### 6.5 实证数据来源

| 来源 | Sessions | Turns | 时长 |
|------|---------|-------|------|
| Codex | 717 | 93,447 | ~1,400h |
| Claude Code | 1,071 | 21,399 | ~200h |
| OpenCode | 247 | 45,120 | — |
| Hermes | 550 | 13,657 | ~350h |
| Windsurf | 65 | 4,549 | ~60h |
| Qwen | 37 | 1,277 | ~4h |
| Antigravity | 154 | 43,379 | — |
| **合计** | **2,841** | **~278,000** | **~2,014h** |

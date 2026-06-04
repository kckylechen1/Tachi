# Compaction / TODO / Handoff：四大 Coding Agent 对比

> 日期：2026-05-30
> 对比对象：Claude Code、OpenCode、Amp、Codex CLI

---

## 一、Context Compaction 机制对比

### Claude Code

- **触发**：对话上下文接近模型窗口限制时自动触发
- **方法**：将完整对话发送给 summarizer agent，生成结构化摘要
- **TODO 处理**：**工具级持久化**。TaskCreate/TaskUpdate/TaskList 是独立于对话的工具，compaction 只压缩对话文本，任务列表天然不受影响
- **恢复后行为**：从摘要继续，任务列表完整保留
- **优势**：TODO 不可能丢，因为根本不在对话里

### OpenCode

- **触发**：context 占到模型窗口的 **95%** 时触发
- **方法**：把整段对话发给 summarizer agent 生成纯文本摘要，截断消息列表
- **TODO 处理**：**完全没有结构化保护**。TODO 存在 SQLite 的 `todo` 表里，但摘要 prompt 只说 "focus on what we did/doing/will do next"，靠 LLM 摘要时"碰巧"提到
- **摘要 prompt**：没有要求包含 TODO 列表
- **恢复后行为**：从纯文本摘要继续，TODO 信息大概率丢失
- **为什么 GPT 5.5 不丢 TODO**：1M 上下文几乎不触发 compaction，问题被窗口大小掩盖了

### Amp

- **触发**：二进制中有 `latestCompactionCutIndex`、`latestCompactionIndex`、`addCompactionRecord` 等函数，说明有服务端管理的 compaction
- **方法**：compaction 在**服务端**执行（对话历史全部在 ampcode.com），本地不感知
- **TODO 处理**：`todo_read` / `todo_write` 是独立工具，类似 Claude Code 的 TaskCreate
- **恢复后行为**：prompt 中明确写了规则：
  > "If the conversation was compacted, continue from the summary; don't restart."
  > "Before finalizing after an interrupt or context compaction, verify your answer addresses the newest request, not an older one still in flight."
- **优势**：
  1. 服务端控制 compaction，可以更智能地选择摘要策略
  2. prompt 里明确告诉模型 compaction 后该怎么做
  3. TODO 工具独立于对话

### Codex CLI（OpenAI）

- **架构**：codex 是无状态 CLI，每轮对话发完整上下文
- **Compaction**：**无 autocompaction**。codex 不压缩上下文，而是在上下文超限时直接截断旧消息
- **TODO 处理**：codex 没有 TODO 工具，完全靠对话上下文
- **劣势**：长任务上下文会溢出，没有恢复机制
- **在 Amp 中的实现**：Amp 的 `gpt-5-codex` prompt 是 codex 模式，400K 窗口 + 128K 输出，不触发 compaction 是因为窗口够大

---

## 二、TODO 管理机制对比

| | Claude Code | OpenCode | Amp | Codex CLI |
|---|---|---|---|---|
| **TODO 存储** | TaskCreate/TaskUpdate 工具，内存级持久化 | SQLite `todo` 表 | todo_read/todo_write 工具 | 无 |
| **Compaction 影响** | 不受影响（工具级） | 大概率丢失（纯文本摘要） | 不受影响（工具级） | N/A |
| **Prompt 规则** | "Mark completed immediately" | 无明确规则 | "MARK todos as completed as soon as you are done. **Do not batch up.**" | 无 |
| **任务列表可见性** | TaskList 随时查看 | 依赖 session 记录 | todo_read 随时查看 | 无 |

---

## 三、Handoff / Session 恢复对比

### Claude Code

- **Session 存储**：本地 JSONL 文件（`~/.claude/projects/.../conversation.jsonl`）
- **恢复**：通过 `--continue` 或 `--resume` flag 恢复上次会话
- **Context**：恢复时加载完整对话历史（如果没被 compaction）
- **Handoff**：无原生 handoff 机制。但可以通过 TaskCreate + memory 系统实现跨 session 的任务追踪

### OpenCode

- **Session 存储**：SQLite 数据库（`~/.local/share/opencode/opencode.db`）
- **恢复**：`opencode --resume` 恢复 session
- **Context**：从 SQLite 重新加载消息历史
- **Handoff**：`parent_id` 机制——subagent session 有 parent_id 指向主 session，但结果需要手动查看
- **Subagent**：`agent` 工具创建子 session，read-only（Glob, Grep, LS, View），同步阻塞

### Amp

- **Session 存储**：**全部在服务端**（`ampcode.com`），本地无对话历史
- **本地痕迹**：只有 `~/.amp/file-changes/{thread_id}/` 存文件 diff
- **恢复**：通过 thread ID 恢复（`T-{UUIDv7}` 格式）
- **Handoff**：
  - Aggman 模式有完整的 thread 工作流：发现 threads → 阅读 → 创建/回复 → 合并/审查
  - `continue_thread` 工具：继续现有 thread
  - `read-thread` 工具：用 subagent 阅读其他 thread 的内容
  - Callback 机制：thread 完成后调 `report_back` 通知主 thread
  - Thread 状态管理：`archive_thread` / `unarchive_thread`
- **Subagent**：三种专用 subagent（Oracle/Task/Codebase Search），每种有明确的 prompt 策略

### Codex CLI

- **Session 存储**：无持久化（无状态）
- **恢复**：不支持
- **Handoff**：不支持

---

## 四、Compaction 的核心教训

### 什么会导致 TODO 丢失

```
TODO 存在对话上下文里 → compaction 压缩对话 → TODO 信息在摘要中丢失
```

### 什么不会导致 TODO 丢失

```
TODO 存在独立工具/数据结构里 → compaction 只压缩对话文本 → TODO 天然不受影响
```

### 1M 上下文能解决问题吗？

**能掩盖**：
- 1M 窗口的模型（GPT-5.5, Qwen, DeepSeek）几乎不触发 compaction
- 问题从"TODO 丢失"变成"永远不会触发 compaction"

**不能解决**：
- 如果真的触发 compaction（超长任务），问题依然存在
- 根本解决方案是工具级持久化（Claude Code / Amp 的做法）

### 最佳实践

1. **TODO 必须工具级持久化**：不依赖对话上下文，不依赖 LLM 摘要
2. **Compaction 后的 prompt 指令**：明确告诉模型 "continue from the summary; don't restart"（Amp 的做法）
3. **摘要要结构化**：不只是"focus on what we did"，而是明确要求包含任务进度（OpenCode 缺失的）
4. **Handoff 需要 callback**：subagent 完成后需要显式回报，不能假设用户能看到 subagent 输出

---

## 五、架构对比总结

| | Claude Code | OpenCode | Amp | Codex CLI |
|---|---|---|---|---|
| **Compaction** | 自动，TODO 不受影响 | 自动，TODO 丢失 | 服务端，TODO 不受影响 | 无，直接截断 |
| **TODO** | 工具级持久化 | SQLite 但无 compaction 保护 | 工具级持久化 | 无 |
| **Session** | 本地 JSONL | 本地 SQLite | 服务端 | 无 |
| **Subagent** | Agent 工具（通用） | agent 工具（read-only） | 3 种专用 agent | 无 |
| **Handoff** | memory 系统 | parent_id | thread 工作流 + callback | 无 |
| **Handoff 跨 session** | 支持（memory 持久化） | 支持（SQLite） | 支持（服务端 thread） | 不支持 |

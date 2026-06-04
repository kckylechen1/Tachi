# Amp Subagent 架构定义

> 来源：`~/.amp/bin/amp` 二进制逆向提取

---

## 三个子 Agent

### Task Tool（执行苦力）

**定位**：Fire-and-forget executor for heavy, multi-file implementations. Think of it as a **productive junior engineer who can't ask follow-ups** once started.

- **用于**：Feature scaffolding, cross-layer refactors, mass migrations, boilerplate generation
- **不用于**：Exploratory work, architectural decisions, debugging analysis
- **Prompt 要点**：
  - Detailed instructions on the goal
  - Enumerate the deliverables
  - Step-by-step procedures and ways to validate results
  - Constraints (e.g. coding style)
  - Include relevant context snippets or examples

### Oracle（高级工程顾问）

**定位**：Senior engineering advisor with **GPT-5.5 reasoning model** for reviews, architecture, deep debugging, and planning.

- **用于**：Code reviews, architecture decisions, performance analysis, complex debugging, planning Task Tool runs
- **不用于**：Simple file searches, bulk code execution
- **Prompt 要点**：
  - Precise problem description
  - Attach necessary files or code
  - Ask for concrete outcomes
  - Request trade-off analysis
  - Use the reasoning power it has

### Codebase Search Agent（代码探索器）

**定位**：Smart code explorer that locates logic based on **conceptual descriptions** across languages/layers.

- **用于**：Mapping features, tracking capabilities, finding side-effects by concept
- **不用于**：Code changes, design advice, simple exact text searches
- **Prompt 要点**：
  - The real-world behavior you are tracking
  - Hints with keywords, file types, or directories
  - Specify a desired output format

---

## 选择决策树

```
"我需要一个高级工程师跟我一起想" → Oracle
"我需要找到匹配某个概念的代码" → Codebase Search Agent
"我知道要做什么，需要大规模多步执行" → Task Tool
```

---

## 最佳实践

### 推荐工作流

```
Oracle (plan) → Codebase Search (validate scope) → Task Tool (execute)
```

### 作用域约束

- Always constrain directories, file patterns, acceptance criteria
- Prompts: **Many small, explicit requests > one giant ambiguous one**

---

## 并行执行规则

| 可并行 | 必须串行 |
|---|---|
| Oracle 的不同关注点（架构审查 / 性能分析 / 竞态调查） | Plan → Code（规划完成后才能编辑） |
| 不同路径的 Codebase Search | 同一文件的多个 Task（写冲突） |
| 写目标不重叠的多个 Task | 链式变换（B 依赖 A 的产物） |
| Reads / Searches / Diagnostics（独立调用） | — |

### 并行示例

**Good**：
```
Oracle(plan-API), finder("validation flow"), finder("timeout handling"),
Task(add-UI), Task(add-logs) → disjoint paths → parallel
```

**Bad**：
```
Task(edit-auth.js), Task(edit-auth.js) → same file → conflict
```

---

## Subagent 使用纪律

> **Do not spawn a subagent for work you can complete directly in a single response**
> (e.g., editing one file, running one search, refactoring a function you can already see).

> **Each subagent loses your context**, so include everything it needs in the prompt:
> the plan, relevant file paths, coding conventions, and how to verify its work.

> **Avoid duplicating work that subagents are already doing.** When a subagent finishes,
> summarize its result for the user since the user cannot see subagent output directly.

> Spawn multiple Task subagents in the same turn when fanning out across genuinely
> independent items — for example, making parallel changes to frontend, backend, and API
> layers after you have already planned the changes.

---

## TODO 工具设计

### 规则

> You plan with a todo list. Track your progress and steps. A good todo list breaks the task
> into meaningful, logically ordered steps that are easy to verify.

> MARK todos as completed as soon as you are done. **Do not batch up multiple tasks
> before marking them as completed.**

---

## 12 个内置工具

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

工具根据 agentMode 动态启用/禁用：
- `enableTask` → `task_tool` 可用性
- `enableOracle` → `oracle` 可用性
- `enableDiagnostics` → 诊断工具
- `enableChart` → 图表工具

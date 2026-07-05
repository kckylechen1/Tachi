---
title: "Tachi Dispatch Goal System Design"
summary: "Design report mapping Codex goal system to Tachi dispatch for better tracking."
category: "engineering/architecture"
organize: true
---
# Tachi Dispatch Goal 系统设计报告

**报告日期**: 2026-05-06
**基于**: OpenAI Codex CLI v0.128.0 源码分析 + Tachi 现有架构
**目标**: 将 Codex 的 `/goal` 系统映射到 Tachi 的 dispatch 流程，提升异步 agent 的目标追踪和完成审计能力

---

## 执行摘要

本报告分析了 OpenAI Codex CLI 的 `ThreadGoal` 系统，并将其映射到 Tachi 的 dispatch 架构。核心发现：**Codex 的 goal 不是简单的 prompt 文本，而是一等状态对象 + 自动审计机制 + 预算控制的组合系统**。

通过引入 `DispatchGoal` 数据结构、continuation template 注入、以及双层审计验证（prompt + API），Tachi 可以在保持异步架构优势的同时，实现 Codex 级别的目标追踪精度。

**关键创新**：
- Turn-based budget（替代 token-based，更简单可靠）
- Skill degradation chain（预算耗尽时自动切换工作模式，而非停止）
- API 层 audit validation（不信任模型自检，系统强制验证）

---

## 1. 背景分析

### 1.1 问题陈述

当前 Tachi dispatch 存在以下问题：
1. **目标模糊**: `task` 参数是纯文本，agent 对"完成"的理解不一致
2. **无审计机制**: 子 agent 可能声称完成，但实际未验证所有需求
3. **预算失控**: `max_turns` 只是硬限制，耗尽时无优雅降级
4. **状态不透明**: dispatch 执行中，主 agent 不知道子 agent 的"目标完成度"

### 1.2 Codex 的解决方案

Codex 通过 `/goal` 命令引入了一个完整的目标管理系统：

```
User: /goal improve benchmark coverage --tokens 50K
Codex: Goal set. Pursuing: improve benchmark coverage (Budget: 50K tokens)

[...执行中...]

Codex: Goal budget limited. Summarizing progress...
```

**核心组件**：
- `ThreadGoal` 状态对象（SQLite 持久化）
- `continuation.md` 模板（每个 turn 自动注入）
- `budget_limit.md` 模板（预算耗尽时切换）
- 自动完成审计（prompt 层强制）

---

## 2. Codex Goal 系统深度分析

### 2.1 数据结构

```rust
// codex-rs/state/src/model/thread_goal.rs
pub struct ThreadGoal {
    pub thread_id: ThreadId,
    pub goal_id: String,
    pub objective: String,           // "improve benchmark coverage"
    pub status: ThreadGoalStatus,    // Active | Paused | BudgetLimited | Complete
    pub token_budget: Option<i64>,   // e.g., 100000 tokens
    pub tokens_used: i64,            // runtime tracking
    pub time_used_seconds: i64,      // runtime tracking
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

**设计要点**：
- `objective` 被标记为 **untrusted data**（用户提供的，不可覆盖系统指令）
- `token_budget` 是可选的，无预算时视为 unlimited
- 状态机简单清晰：Active → BudgetLimited/Complete

### 2.2 Prompt 模板系统

Codex 使用两个核心模板：

**continuation.md**（每个 turn 开始时注入）：
```markdown
Continue working toward the active thread goal.

<untrusted_objective>
{{ objective }}
</untrusted_objective>

Budget:
- Time spent: {{ time_used_seconds }} seconds
- Tokens used: {{ tokens_used }}

Avoid repeating work already done...

Before deciding completion, perform a completion audit:
- Restate objective as concrete deliverables
- Build prompt-to-artifact checklist
- Verify every requirement has concrete evidence
- Do not accept proxy signals as completion
- Treat uncertainty as NOT achieved
```

**budget_limit.md**（预算耗尽时注入）：
```markdown
The active thread goal has reached its token budget.

Do not start new substantive work. Wrap up this turn:
- Summarize useful progress
- Identify remaining work or blockers
- Leave a clear next step
```

**关键洞察**：
- 模板用 `{{ variable }}` 语法，运行时渲染
- `untrusted_objective` XML tag 明确标记用户数据的边界
- 审计指令是**强制性的**（"Before deciding completion, perform..."）

### 2.3 状态机

```
Active ──/goal pause──► Paused ──/goal resume──► Active
  │
  │ token_budget exhausted
  ▼
BudgetLimited
  │
  │ goal achieved + audit passed
  ▼
Complete
```

**状态转换触发**：
- Active → Paused: 用户输入 `/goal pause`
- Paused → Active: 用户输入 `/goal resume`
- Active → BudgetLimited: `tokens_used >= token_budget`
- Active → Complete: 模型调用 `update_goal(status="complete")`

### 2.4 持久化

Codex 将 goal 状态存储在 SQLite `thread_goals` 表中：

```sql
CREATE TABLE thread_goals (
    thread_id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL,
    objective TEXT NOT NULL,
    status TEXT NOT NULL,  -- 'active' | 'paused' | 'budget_limited' | 'complete'
    token_budget INTEGER,
    tokens_used INTEGER NOT NULL DEFAULT 0,
    time_used_seconds INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
```

**设计选择**：
- 按 `thread_id` 分区（一个 thread 一个 goal）
- 时间戳用 epoch milliseconds（方便计算和排序）
- 无外键约束（Codex 是客户端应用，不保证 thread 表存在）

---

## 3. Tachi 映射方案

### 3.1 架构映射总览

```
┌─────────────────────────────────────────────┐
│ Codex Goal System                            │
├─────────────────────────────────────────────┤
│ ThreadGoal { objective, status,              │
│             token_budget, tokens_used }      │
│         │                                    │
│         ▼                                    │
│ continuation.md (per-turn injection)         │
│         │                                    │
│         ▼                                    │
│ completion audit (prompt-enforced)           │
│         │                                    │
│         ▼                                    │
│ budget_limit.md (budget exhausted)           │
└─────────────────────────────────────────────┘
              │
              │ maps to
              ▼
┌─────────────────────────────────────────────┐
│ Tachi Dispatch Goal                          │
├─────────────────────────────────────────────┤
│ DispatchGoal { task, status, budget,         │
│              audit_required }                │
│         │                                    │
│         ▼                                    │
│ Skill Continuation Template                  │
│ (per-dispatch injection)                     │
│         │                                    │
│         ▼                                    │
│ Skill Audit Gate (prompt + API validation)   │
│         │                                    │
│         ▼                                    │
│ Skill Degradation Chain                      │
│ (budget exhausted → switch skill)            │
└─────────────────────────────────────────────┘
```

### 3.2 数据结构映射

| Codex ThreadGoal | Tachi DispatchGoal | 说明 |
|------------------|-------------------|------|
| `objective` | `task` | 已存在，强化为可审计目标 |
| `status` | `status` | 映射到 kanban a2a_state |
| `token_budget` | `turn_budget` | **关键变更**: turn-based 替代 token-based |
| `tokens_used` | `turns_used` | 通过 trajectory.jsonl 追踪 |
| `time_used_seconds` | `elapsed_seconds` | 从 DispatchResult 计算 |
| `goal_id` | `dispatch_id` | 已存在 |
| — | `audit_required` | **新增**: 是否强制审计 |
| — | `degradation_chain` | **新增**: skill 降级链 |
| — | `audit_checklist` | **新增**: 审计检查项 |

**为什么用 turn-based budget？**
- Token 计数需要 tokenizer 集成，增加复杂度
- Turn 是更高层的语义单位，与 `max_turns` 参数天然对齐
- Codex 用 token 是因为它是交互式 REPL；Tachi 是异步 dispatch，turn 更合适

### 3.3 Prompt 模板映射

Tachi 版 continuation template：

```markdown
## Active Goal Context

You are working toward: {{ task }}

Progress tracking:
- Elapsed time: {{ elapsed_minutes }} minutes
- Turns used: {{ turns_used }} / {{ turn_budget }}
- Stage: {{ stage }}
- Skills active: {{ skills }}

Avoid repeating work already completed. Choose the next concrete action.

## Completion Audit Gate

Before calling `tachi_complete`, you MUST perform a completion audit:
1. Restate the task as concrete deliverables or success criteria
2. Build a checklist mapping every requirement to concrete evidence
3. Inspect files, command output, test results for each item
4. Do not accept proxy signals ("tests pass") as completion by themselves
5. Identify any missing, incomplete, or unverified requirement
6. Treat uncertainty as NOT achieved; continue working

Only call `tachi_complete` when the audit confirms full achievement.
```

**与 Codex 的差异**：
- Codex 用 `untrusted_objective` XML tag；Tachi 用 Markdown section（更简单）
- Codex 注入每个 turn；Tachi 注入每个 dispatch（异步架构差异）
- Tachi 明确提及 `tachi_complete` 调用点

### 3.4 状态机映射

**Tachi Kanban 状态**：

```
TASK_STATE_WORKING ──pause──► TASK_STATE_PENDING ──resume──► TASK_STATE_WORKING
      │
      │ turn_budget exhausted
      ▼
TASK_STATE_INPUT_REQUIRED
      │
      │ audit passed + tachi_complete
      ▼
TASK_STATE_COMPLETED
```

**新增状态**：
- `TASK_STATE_INPUT_REQUIRED`: 预算耗尽，等待 operator 决策（对应 Codex 的 BudgetLimited）
- 现有 `TASK_STATE_FAILED` 对应失败场景

### 3.5 Skill 绑定映射

Codex 的 goal 隐式绑定到当前 session 的 skill。Tachi 可以显式声明：

```json
{
  "skill": "skill:superpowers-writing-plans",
  "goal_types": ["plan", "design"],
  "produces_audit": true,
  "continuation_template": "templates/continuation.md",
  "budget_degradation": {
    "at_50_percent": "skill:superpowers-verification",
    "at_80_percent": "skill:superpowers-handoff"
  }
}
```

**设计意图**：
- `goal_types`: 声明这个 skill 适合什么类型的目标
- `produces_audit`: 是否自动注入完成审计指令
- `budget_degradation`: 预算耗尽时的 skill 降级链

---

## 4. 原型实现

### 4.1 核心数据结构

```rust
// docs/codex-goal/prototype.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchGoal {
    pub task: String,
    pub status: GoalStatus,
    pub turn_budget: Option<u32>,
    pub turns_used: u32,
    pub elapsed_seconds: u64,
    pub audit_required: bool,
    pub degradation_chain: Vec<String>,
    pub audit_checklist: Vec<AuditItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    Active,
    Paused,
    BudgetLimited,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditItem {
    pub requirement: String,
    pub evidence_path: Option<String>,
    pub verified: bool,
    pub notes: String,
}
```

### 4.2 Prompt 渲染函数

```rust
pub fn render_continuation_template(
    goal: &DispatchGoal,
    stage: &str,
    skills: &[String],
) -> String {
    let template = include_str!("templates/continuation.md");
    template
        .replace("{{task}}", &goal.task)
        .replace("{{elapsed_minutes}}", &(goal.elapsed_seconds / 60).to_string())
        .replace("{{turns_used}}", &goal.turns_used.to_string())
        .replace("{{turn_budget}}", &goal.turn_budget.map(|b| b.to_string()).unwrap_or_else(|| "unlimited".to_string()))
        .replace("{{stage}}", stage)
        .replace("{{skills}}", &skills.join(", "))
}
```

### 4.3 预算追踪

```rust
pub fn update_goal_progress(
    goal: &mut DispatchGoal,
    turns_consumed: u32,
    elapsed_seconds: u64,
) -> GoalStatus {
    goal.turns_used += turns_consumed;
    goal.elapsed_seconds = elapsed_seconds;

    if let Some(budget) = goal.turn_budget {
        if goal.turns_used >= budget {
            goal.status = GoalStatus::BudgetLimited;
            return GoalStatus::BudgetLimited;
        }
    }
    goal.status
}
```

### 4.4 Skill 降级

```rust
pub fn resolve_degradation_skill(
    goal: &DispatchGoal,
    original_skills: &[String],
) -> Vec<String> {
    let Some(budget) = goal.turn_budget else {
        return original_skills.to_vec();
    };

    let usage_ratio = goal.turns_used as f32 / budget as f32;

    if usage_ratio >= 0.8 && goal.degradation_chain.len() >= 3 {
        vec![goal.degradation_chain[2].clone()] // handoff
    } else if usage_ratio >= 0.5 && goal.degradation_chain.len() >= 2 {
        vec![goal.degradation_chain[1].clone()] // verification
    } else {
        original_skills.to_vec()                // original
    }
}
```

### 4.5 审计验证

```rust
pub fn validate_completion_audit(
    goal: &DispatchGoal,
    outcome: &str,
    notes: Option<&str>,
) -> Result<(), String> {
    if !goal.audit_required {
        return Ok(());
    }

    if outcome == "success" {
        let has_checklist = goal.audit_checklist.iter().any(|item| item.verified);
        if !has_checklist {
            return Err(
                "Completion audit failed: no verified checklist items. \
                 Please inspect the actual state and verify each requirement."
                    .to_string(),
            );
        }
    }
    Ok(())
}
```

---

## 5. 集成点

### 5.1 dispatch.rs 集成

在 `handle_tachi_dispatch()` 中增加：

1. **初始化 goal**（从参数或默认构造）
2. **注入 continuation template**（在 `assemble_prompt()` 中）
3. **记录 trajectory**（`{"event": "goal_progress_updated", ...}`）
4. **Watchdog 追踪**（子进程完成后更新 goal 进度）
5. **BudgetLimited 处理**（设置 kanban 为 TASK_STATE_INPUT_REQUIRED）

### 5.2 prompt.rs 集成

在 `assemble_prompt()` 中增加 Step 4.5：

```rust
if let Some(g) = goal {
    match g.status {
        GoalStatus::Active => {
            parts.push(render_continuation_template(g, stage, &skills));
        }
        GoalStatus::BudgetLimited => {
            parts.push(render_budget_limit_template(g, stage));
        }
        _ => {}
    }
}
```

### 5.3 kanban_helpers.rs 集成

增强 `init_kanban_task()`：

```rust
if let Some(g) = goal {
    metadata["goal_task"] = json!(g.task);
    metadata["goal_audit_required"] = json!(g.audit_required);
    metadata["goal_turn_budget"] = json!(g.turn_budget);
    metadata["goal_turns_used"] = json!(g.turns_used);
}
```

### 5.4 complete_ops.rs 集成

在 `handle_tachi_complete()` 中增加：

```rust
if goal.audit_required && outcome == "success" {
    validate_completion_audit(&goal, &outcome, notes.as_deref())?;
}
```

---

## 6. 实施计划

### Phase 1: 数据结构 + 模板（2 天）

- [ ] 创建 `DispatchGoal` 和 `GoalStatus`
- [ ] 扩展 `TachiDispatchParams` 加入 `goal` 字段
- [ ] 创建 `templates/continuation.md` 和 `templates/budget_limit.md`
- [ ] 实现 `render_continuation_template()` 和 `render_budget_limit_template()`

### Phase 2: Prompt 集成（1 天）

- [ ] 修改 `assemble_prompt()` 注入 goal continuation
- [ ] 支持 `stage` 参数联动（plan/execute/auto）
- [ ] 测试不同 goal status 的 prompt 输出

### Phase 3: Budget 追踪（2 天）

- [ ] 在 `trajectory.jsonl` 中记录 turn usage
- [ ] Watchdog 中实现 `update_goal_progress()`
- [ ] 实现 skill degradation chain
- [ ] 测试 budget 耗尽场景

### Phase 4: Audit Gate（2 天）

- [ ] 在 `tachi_complete` 中加入 audit validation
- [ ] 设计 audit checklist 数据结构
- [ ] 实现 checklist 在 trajectory 中的记录
- [ ] 测试 audit 失败/通过场景

### Phase 5: Kanban 集成（1 天）

- [ ] 增强 kanban 初始化（goal metadata）
- [ ] 状态映射（GoalStatus → a2a_state）
- [ ] `tachi_board` 查询支持 goal 过滤

**总计**: 8 个工作日

---

## 7. 风险评估

### 7.1 技术风险

| 风险 | 概率 | 影响 | 缓解措施 |
|------|------|------|----------|
| Turn 计数不准确 | 中 | 高 | 通过 `max_turns` 参数由子进程自身限制，Tachi 只追踪实际消耗的 turn |
| Audit validation 误报 | 低 | 高 | 先设为 warn 模式，不 block dispatch；积累数据后改为 enforce |
| Prompt 过长 | 中 | 中 | continuation template 控制在 200 tokens 以内；可选注入（audit_required=false 时跳过） |
| Skill degradation 失效 | 低 | 中 | degradation skill 必须注册在 Hub 中；fallback 到默认 skill |

### 7.2 兼容性风险

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| 向后兼容 | 中 | `goal` 参数为 Option，不传时行为不变 |
| Codex agent 支持 | 低 | Codex exec 模式不读取外部 config；Tachi 通过 prompt injection 实现等效功能 |
| Custom agent 支持 | 低 | custom agent 不受 goal 系统影响（除非显式配置） |

---

## 8. 对比总结

### Tachi 相对 Codex 的增强

| 维度 | Codex | Tachi (映射后) |
|------|-------|----------------|
| **Budget 单位** | Token count（需 tokenizer） | Turn count（更简单） |
| **Budget 耗尽行为** | 被动停止（告诉模型别干了） | 主动 Skill 降级（切换工作模式） |
| **Audit 保障** | Prompt only（信任模型自检） | Prompt + API validation（不信任模型） |
| **失败状态** | 无显式 Failed | Failed + BudgetLimited 区分 |
| **交互方式** | Slash command（交互式） | Declarative parameter（自动化友好） |
| **持久化** | SQLite（集中式） | Kanban + Trajectory（分布式） |
| **目标复用** | 单 session | 跨 dispatch 复用（goal 存于 kanban） |

### Codex 相对 Tachi 的优势

| 维度 | Codex | Tachi |
|------|-------|-------|
| **实时反馈** | 每 turn 更新 goal 状态 | 仅 dispatch 开始/结束更新 |
| **用户控制** | `/goal pause/resume/clear` | 需通过 kanban API 操作 |
| **Token 精度** | 精确到 token | 粗粒度 turn |
| **UI 集成** | TUI 显示 goal 进度 | 无原生 UI（依赖 tachi_board） |

---

## 9. 参考文件

### Codex 源码

- `codex-rs/core/src/goals.rs` — Goal 运行时状态管理（1639 行）
- `codex-rs/core/templates/goals/continuation.md` — Continuation prompt 模板
- `codex-rs/core/templates/goals/budget_limit.md` — Budget limit prompt 模板
- `codex-rs/state/src/model/thread_goal.rs` — ThreadGoal 数据模型
- `codex-rs/app-server-protocol/src/protocol/v2/thread.rs` — ThreadGoal 协议定义

### Tachi 源码

- `crates/memory-server/src/dispatch_ops/dispatch.rs` — Dispatch 主流程
- `crates/memory-server/src/dispatch_ops/prompt.rs` — Prompt 组装
- `crates/memory-server/src/dispatch_ops/kanban_helpers.rs` — Kanban 状态管理
- `crates/memory-server/src/dispatch_ops/subprocess.rs` — 子进程管理
- `crates/memory-server-params/src/facade.rs` — TachiDispatchParams 定义

### 本报告相关文件

- `docs/codex-goal-to-tachi-mapping.md` — 映射方案总览
- `docs/codex-goal-code-mapping.md` — 代码片段对照
- `docs/codex-goal/templates/continuation.md` — Tachi continuation 模板
- `docs/codex-goal/templates/budget_limit.md` — Tachi budget limit 模板
- `docs/codex-goal/prototype.rs` — 原型数据结构 + 工具函数（文档原型，非编译单元）
- `docs/codex-goal/integration-example.rs` — 集成示例代码（文档示例，非接线实现）

---

## 10. 附录：Codex Goal 系统关键代码片段

### A.1 ThreadGoal 定义

```rust
// codex-rs/state/src/model/thread_goal.rs:52
pub struct ThreadGoal {
    pub thread_id: ThreadId,
    pub goal_id: String,
    pub objective: String,
    pub status: ThreadGoalStatus,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### A.2 continuation.md 模板

```markdown
// codex-rs/core/templates/goals/continuation.md
Continue working toward the active thread goal.

<untrusted_objective>
{{ objective }}
</untrusted_objective>

Budget:
- Time spent pursuing goal: {{ time_used_seconds }} seconds
- Tokens used: {{ tokens_used }}
- Token budget: {{ token_budget }}
- Tokens remaining: {{ remaining_tokens }}

Avoid repeating work that is already done...

Before deciding completion, perform a completion audit:
- Restate the objective as concrete deliverables
- Build a prompt-to-artifact checklist
- Verify every requirement has concrete evidence
- Do not accept proxy signals as completion
- Treat uncertainty as not achieved
```

### A.3 Prompt 渲染

```rust
// codex-rs/core/src/goals.rs:1396
fn continuation_prompt(goal: &ThreadGoal) -> String {
    let objective = escape_xml_text(&goal.objective);
    CONTINUATION_PROMPT_TEMPLATE.render([
        ("objective", objective.as_str()),
        ("tokens_used", &goal.tokens_used.to_string()),
        ("time_used_seconds", &goal.time_used_seconds.to_string()),
        ("token_budget", &token_budget),
        ("remaining_tokens", &remaining_tokens),
    ])
}
```

---

*报告完成。如需进一步细化某个 Phase 的实施细节，或需要开始编码实现，请告知。*

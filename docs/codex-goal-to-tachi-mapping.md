# Codex Goal → Tachi 映射方案

## 核心洞察

Codex 的 `/goal` 不是简单的 prompt 注入，而是一个**一等状态对象** + **自动审计机制** + **预算控制**的组合系统。它把"目标完成度"从 optional 变成 mandatory。

## 架构映射总览

```
┌─────────────────────────────────────────────────────────────────┐
│                     Codex Goal System                            │
├─────────────────────────────────────────────────────────────────┤
│  ThreadGoal { objective, status, token_budget, tokens_used }    │
│         │                                                        │
│         ▼                                                        │
│  continuation.md (每个 turn 自动注入)                            │
│         │                                                        │
│         ▼                                                        │
│  completion audit (强制完成审计)                                 │
│         │                                                        │
│         ▼                                                        │
│  budget_limit.md (预算耗尽时切换)                                │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ 映射到
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Tachi Dispatch Goal                          │
├─────────────────────────────────────────────────────────────────┤
│  DispatchGoal { task, status, budget, audit_required }          │
│         │                                                        │
│         ▼                                                        │
│  Skill Continuation Template (每次 dispatch 注入)                │
│         │                                                        │
│         ▼                                                        │
│  Skill Audit Gate (tachi_complete 前强制检查)                    │
│         │                                                        │
│         ▼                                                        │
│  Skill Degradation (预算耗尽时切换 skill)                        │
└─────────────────────────────────────────────────────────────────┘
```

## 详细映射

### 1. 数据结构映射

| Codex | Tachi | 说明 |
|-------|-------|------|
| `ThreadGoal.objective` | `DispatchGoal.task` | 已存在，需强化为**可审计目标** |
| `ThreadGoal.status` | `kanban a2a_state` | 已存在：`TASK_STATE_WORKING/COMPLETED/FAILED` |
| `ThreadGoal.token_budget` | `DispatchGoal.turn_budget` | **新增**：max_turns 的语义扩展 |
| `ThreadGoal.tokens_used` | `trajectory.jsonl` 记录 | **新增**：每步 tool call 的 token 追踪 |
| `ThreadGoal.time_used_seconds` | `DispatchResult.duration_ms` | 已存在 |
| `ThreadGoal.goal_id` | `dispatch_id` | 已存在 |

### 2. Prompt 模板映射

| Codex | Tachi | 注入时机 |
|-------|-------|----------|
| `continuation.md` | `skill continuation template` | 每次 `assemble_prompt()` 时 |
| `budget_limit.md` | `skill degradation template` | budget 耗尽时切换 skill |

**Codex continuation.md 核心内容：**
```markdown
Continue working toward the active thread goal.

<untrusted_objective>
{{ objective }}
</untrusted_objective>

Budget:
- Time spent: {{ time_used_seconds }}s
- Tokens used: {{ tokens_used }}

Avoid repeating work already done...

Before deciding completion, perform a completion audit:
- Restate objective as concrete deliverables
- Build prompt-to-artifact checklist
- Verify every requirement has concrete evidence
```

**Tachi 对应版本：**
```markdown
## Active Goal Context

You are working toward: {{ task }}

Progress tracking:
- Elapsed time: {{ elapsed_minutes }} minutes
- Turns used: {{ turns_used }} / {{ turn_budget }}
- Stage: {{ stage }}

Avoid repeating work already completed. Choose the next concrete action.

## Completion Audit Gate

Before calling `tachi_complete`, you MUST perform a completion audit:
1. Restate the task as concrete deliverables or success criteria
2. Build a checklist mapping every requirement to concrete evidence
3. Inspect files, command output, test results for each item
4. Do not accept proxy signals ("tests pass") as completion by themselves
5. Identify any missing, incomplete, or unverified requirement
6. Treat uncertainty as NOT achieved; continue working

Only call `tachi_complete` when the audit confirms the objective is fully achieved.
```

### 3. 状态机映射

**Codex：**
```
Active ──pause──► Paused ──resume──► Active
  │                                  │
  │ budget exhausted                 │
  ▼                                  │
BudgetLimited ◄─────────────────────┘
  │
  │ goal achieved
  ▼
Complete
```

**Tachi Kanban：**
```
TASK_STATE_WORKING ──pause──► TASK_STATE_PENDING ──resume──► TASK_STATE_WORKING
      │                                                           │
      │ budget exhausted (input needed)                             │
      ▼                                                           │
TASK_STATE_INPUT_REQUIRED ◄───────────────────────────────────────┘
      │
      │ audit passed
      ▼
TASK_STATE_COMPLETED
```

### 4. Skill 绑定映射

Codex 的 goal 是隐式绑定到 skill 的（goal 本身包含指令）。Tachi 可以显式声明 skill 与 goal 类型的关系：

```json
{
  "skill": "skill:superpowers-writing-plans",
  "goal_types": ["plan", "design"],
  "produces_audit": true,
  "continuation_template": "## Plan Continuation\n...",
  "budget_degradation": {
    "at_50_percent": "skill:superpowers-verification",
    "at_80_percent": "skill:superpowers-handoff"
  }
}
```

### 5. 预算控制映射

| Codex | Tachi | 触发条件 |
|-------|-------|----------|
| `token_budget` 耗尽 | `turn_budget` 耗尽 | `max_turns` 达到阈值 |
| 切换 `budget_limit.md` | 切换 degradation skill | 自动降低工作粒度 |
| 停止新工作 | 进入 `TASK_STATE_INPUT_REQUIRED` | 等待 operator 决策 |

## 实施计划

### Phase 1: Goal 数据结构（1 天）
- [ ] 创建 `DispatchGoal` 结构体
- [ ] 扩展 `TachiDispatchParams` 加入 `goal` 字段
- [ ] 在 `trajectory.jsonl` 中记录 token/turn 使用情况

### Phase 2: Continuation Template（1 天）
- [ ] 创建 `templates/dispatch/continuation.md`
- [ ] 在 `assemble_prompt()` 中注入 goal 上下文
- [ ] 实现模板变量渲染（`{{ task }}`, `{{ turns_used }}` 等）

### Phase 3: Audit Gate（1 天）
- [ ] 在 `tachi_complete` 处理中加入 audit 验证
- [ ] 创建 `skill:superpowers-completion-audit`
- [ ] 在 kanban 中记录 audit 结果

### Phase 4: Budget Control（1 天）
- [ ] 在 Watchdog 中追踪 turn 使用量
- [ ] 实现 skill degradation（预算耗尽时自动切换）
- [ ] 测试 budget_limit 场景

## 关键设计决策

### Q: 为什么不用 Codex 的 `/goal` 命令？
A: Codex 的 goal 是交互式 TUI 功能（用户输入 `/goal improve tests`）。Tachi 的 dispatch 是异步的，没有交互会话。所以 Tachi 的 goal 是**声明式的**（dispatch 参数），而非**命令式的**（slash command）。

### Q: Completion Audit 放在哪里？
A: 两个位置：
1. **Prompt 层**：在 `assemble_prompt()` 中注入 audit 指令，让子 agent 自检
2. **Validation 层**：在 `tachi_complete` 处理时，由 Tachi 主进程验证子 agent 是否提供了足够的 evidence

### Q: 预算耗尽时怎么办？
A: Codex 的做法是停止新工作 + 总结进度。Tachi 可以：
1. 将状态设为 `TASK_STATE_INPUT_REQUIRED`
2. 在 `result.md` 中生成进度报告
3. 等待 operator 决策：续费预算、接受部分完成、或取消任务

## 参考文件

- Codex: `codex-rs/core/src/goals.rs` — Goal 运行时状态管理
- Codex: `codex-rs/core/templates/goals/continuation.md` — Continuation prompt 模板
- Codex: `codex-rs/core/templates/goals/budget_limit.md` — Budget limit prompt 模板
- Tachi: `crates/memory-server/src/dispatch_ops/prompt.rs` — Prompt 组装
- Tachi: `crates/memory-server/src/dispatch_ops/kanban_helpers.rs` — Kanban 状态管理
- Tachi: `crates/memory-server/src/dispatch_ops/dispatch.rs` — Dispatch 主流程
- Prototype: `docs/codex-goal/prototype.rs` — 原型数据结构与工具函数
- Example: `docs/codex-goal/integration-example.rs` — 集成示例代码
- Templates: `docs/codex-goal/templates/` — continuation / budget-limit 模板

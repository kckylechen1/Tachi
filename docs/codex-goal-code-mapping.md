# Codex Goal ↔ Tachi 代码对照

## 1. 数据结构定义

### Codex: ThreadGoal

```rust
// codex-rs/state/src/model/thread_goal.rs
#[derive(Debug, Clone, PartialEq, Eq)]
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

pub enum ThreadGoalStatus {
    Active,
    Paused,
    BudgetLimited,
    Complete,
}
```

### Tachi: DispatchGoal

```rust
// Prototype: docs/codex-goal/prototype.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchGoal {
    pub task: String,                  // ← objective
    pub status: GoalStatus,            // ← ThreadGoalStatus
    pub turn_budget: Option<u32>,      // ← token_budget (turn-based)
    pub turns_used: u32,               // ← tokens_used (turn-based)
    pub elapsed_seconds: u64,          // ← time_used_seconds
    pub audit_required: bool,          // NEW: configurable audit
    pub degradation_chain: Vec<String>, // NEW: skill fallback chain
    pub audit_checklist: Vec<AuditItem>, // NEW: completion checklist
}

pub enum GoalStatus {
    Active,         // ← Active
    Paused,         // ← Paused
    BudgetLimited,  // ← BudgetLimited
    Complete,       // ← Complete
    Failed,         // NEW: terminal failure state
}
```

**关键差异**：
- Codex 用 `token_budget`（需要 tokenizer）；Tachi 用 `turn_budget`（更简单，无需 tokenizer 集成）
- Tachi 增加了 `audit_required` 和 `degradation_chain`，把 Codex 的隐式行为变成显式配置

---

## 2. Prompt 模板注入

### Codex: continuation.md

```rust
// codex-rs/core/src/goals.rs
static CONTINUATION_PROMPT_TEMPLATE: LazyLock<Template> =
    LazyLock::new(|| {
        Template::parse(include_str!("../templates/goals/continuation.md"))
    });

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

**调用时机**：每个 turn 开始时，作为 developer message 注入。

### Tachi: templates/continuation.md

```rust
// Prototype: docs/codex-goal/prototype.rs
pub fn render_continuation_template(goal: &DispatchGoal, stage: &str, skills: &[String]) -> String {
    let template = include_str!("templates/continuation.md");
    template
        .replace("{{task}}", &goal.task)
        .replace("{{elapsed_minutes}}", &((goal.elapsed_seconds / 60).to_string()))
        .replace("{{turns_used}}", &goal.turns_used.to_string())
        .replace("{{turn_budget}}", &goal.turn_budget.map(|b| b.to_string()).unwrap_or_else(|| "unlimited".to_string()))
        .replace("{{stage}}", stage)
        .replace("{{skills}}", &skills.join(", "))
}
```

**调用时机**：`assemble_prompt()` 中，dispatch 前一次性注入（Tachi 的 dispatch 是异步子进程，不是交互式 turn）。

**关键差异**：
- Codex：每个 turn 都注入（交互式 session）
- Tachi：dispatch 时一次性注入（异步子进程），但通过 `tachi_complete` 的 audit validation 实现等效约束

---

## 3. 预算控制

### Codex: Token Budget Tracking

```rust
// codex-rs/core/src/goals.rs
pub(crate) enum GoalRuntimeEvent {
    TurnStarted { token_usage: TokenUsage, ... },
    TurnFinished { turn_completed: bool, ... },
    // ...
}

// On each turn finish:
// 1. Update tokens_used
// 2. Check if tokens_used >= token_budget
// 3. If yes, inject budget_limit_prompt() and set status = BudgetLimited
```

### Tachi: Turn Budget Tracking

```rust
// Prototype: docs/codex-goal/prototype.rs
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

**调用时机**：Watchdog 在子进程完成后调用。

**关键差异**：
- Codex：实时追踪（每 turn 结束检查）
- Tachi：事后追踪（子进程结束后检查，通过 `max_turns` 参数由子进程自身限制）

---

## 4. Skill Degradation（预算耗尽时）

### Codex: Implicit Stop

```markdown
// codex-rs/core/templates/goals/budget_limit.md
The active thread goal has reached its token budget.

Do not start new substantive work for this goal.
Wrap up this turn soon: summarize useful progress, identify remaining work.
```

Codex 的做法是：**停止新工作，总结进度**。

### Tachi: Explicit Skill Chain

```rust
// Prototype: docs/codex-goal/prototype.rs
pub fn resolve_degradation_skill(
    goal: &DispatchGoal,
    original_skills: &[String],
) -> Vec<String> {
    let usage_ratio = goal.turns_used as f32 / goal.turn_budget.unwrap_or(1) as f32;

    if usage_ratio >= 0.8 {
        vec![goal.degradation_chain[2].clone()] // skill:superpowers-handoff
    } else if usage_ratio >= 0.5 {
        vec![goal.degradation_chain[1].clone()] // skill:superpowers-verification
    } else {
        original_skills.to_vec()                // skill:superpowers-executing-plans
    }
}
```

Tachi 的做法是：**切换 skill**，让子 agent 以不同的模式继续工作（从"执行"降级到"验证"再降级到"交接"）。

**关键差异**：
- Codex：被动停止（告诉模型别干了）
- Tachi：主动降级（切换 skill，改变工作模式）

---

## 5. 完成审计

### Codex: Prompt-based Audit

```markdown
// codex-rs/core/templates/goals/continuation.md (excerpt)
Before deciding that the goal is achieved, perform a completion audit:
- Restate the objective as concrete deliverables or success criteria.
- Build a prompt-to-artifact checklist...
- Do not accept proxy signals as completion by themselves.
- Treat uncertainty as not achieved; do more verification or continue the work.
```

Codex 的审计是**纯 prompt 驱动**的——告诉模型要审计，模型自己决定做不做。

### Tachi: Prompt + API Validation

```rust
// Prototype: docs/codex-goal/prototype.rs
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

Tachi 的审计是**双层保障**：
1. **Prompt 层**：`continuation.md` 中注入审计指令（同 Codex）
2. **API 层**：`tachi_complete` 处理时验证 checklist（Codex 没有这层）

**关键差异**：
- Codex：信任模型自检（prompt only）
- Tachi：不信任模型，API 层强制验证

---

## 6. 状态持久化

### Codex: SQLite

```rust
// codex-rs/state/src/model/thread_goal.rs
pub(crate) struct ThreadGoalRow {
    pub thread_id: String,
    pub goal_id: String,
    pub objective: String,
    pub status: String,         // "active" | "paused" | "budget_limited" | "complete"
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
```

存储在 SQLite `thread_goals` 表中。

### Tachi: Kanban + Trajectory

```rust
// Tachi: kanban row (memory DB)
{
    "path": "/kanban/tasks/{dispatch_id}",
    "metadata": {
        "a2a_state": "TASK_STATE_WORKING",      // ← status
        "goal_task": "improve benchmark",        // ← objective
        "goal_audit_required": true,
        "goal_turn_budget": 10,
        "goal_turns_used": 5,
    }
}

// Tachi: trajectory.jsonl (append-only events)
{"event": "dispatch_started", "goal_status": "active", "turn_budget": 10}
{"event": "turn_completed", "turns_used": 5, "turn_budget": 10}
{"event": "goal_budget_limited", "turns_used": 10, "turn_budget": 10}
{"event": "audit_submitted", "checklist_items": 5, "verified_items": 4}
{"event": "subprocess_finished", "goal_status": "complete"}
```

**关键差异**：
- Codex：集中式 SQLite（适合单进程）
- Tachi：分布式（Kanban 存最新状态，Trajectory 存历史事件）

---

## 7. 用户交互

### Codex: Slash Command

```
User: /goal improve benchmark coverage
Codex: Goal set. Pursuing: improve benchmark coverage

User: /goal pause
Codex: Goal paused.

User: /goal resume
Codex: Goal resumed.
```

### Tachi: Declarative Parameters

```json
// tachi_shell dispatch call
{
    "agent": "claude",
    "task": "improve benchmark coverage",
    "goal": {
        "audit_required": true,
        "turn_budget": 10,
        "degradation_chain": [
            "skill:superpowers-executing-plans",
            "skill:superpowers-verification",
            "skill:superpowers-handoff"
        ]
    },
    "stage": "execute"
}
```

**关键差异**：
- Codex：交互式（用户在 TUI 中输入 `/goal`）
- Tachi：声明式（用户在 dispatch 参数中指定 goal）

---

## 总结：Tachi 相对 Codex 的增强

| 维度 | Codex | Tachi (映射后) |
|------|-------|----------------|
| **Budget 单位** | Token count | Turn count (更简单) |
| **Budget 耗尽** | 停止工作 | Skill degradation (更智能) |
| **Audit  enforcement** | Prompt only | Prompt + API validation (更强) |
| **状态持久化** | SQLite | Kanban + Trajectory (更适合分布式) |
| **用户交互** | Slash command | Declarative parameter (更适合自动化) |
| **完成定义** | 模型自检 | 模型自检 + 系统验证 (更可靠) |
| **失败处理** | 无显式 Failed 状态 | Failed + BudgetLimited 区分 (更精细) |

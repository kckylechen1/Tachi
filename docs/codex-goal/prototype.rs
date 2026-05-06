// codex_goal_prototype.rs
// Prototype showing how Codex's ThreadGoal maps to Tachi's DispatchGoal system
// This file demonstrates the integration points; it is NOT meant to be compiled directly.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════════════
// PART 1: Data Structure Mapping
// Codex ThreadGoal → Tachi DispatchGoal
// ═══════════════════════════════════════════════════════════════════════════════

/// Codex's ThreadGoal (from codex-rs/state/src/model/thread_goal.rs)
///
/// ```rust
/// pub struct ThreadGoal {
///     pub thread_id: ThreadId,
///     pub goal_id: String,
///     pub objective: String,           // "improve benchmark coverage"
///     pub status: ThreadGoalStatus,    // Active | Paused | BudgetLimited | Complete
///     pub token_budget: Option<i64>,   // e.g., 100000 tokens
///     pub tokens_used: i64,            // runtime tracking
///     pub time_used_seconds: i64,      // runtime tracking
///     pub created_at: DateTime<Utc>,
///     pub updated_at: DateTime<Utc>,
/// }
/// ```

/// Tachi's equivalent: DispatchGoal
/// Maps Codex's imperative state into Tachi's declarative dispatch parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchGoal {
    /// Maps to: ThreadGoal.objective
    /// The user-provided task description, treated as untrusted data
    pub task: String,

    /// Maps to: ThreadGoal.status
    /// Tachi uses kanban a2a_state for persistence; this is the runtime view
    pub status: GoalStatus,

    /// Maps to: ThreadGoal.token_budget
    /// Tachi uses "turn budget" instead of token budget (easier to track at dispatch level)
    pub turn_budget: Option<u32>,

    /// Maps to: ThreadGoal.tokens_used
    /// Tracked via trajectory.jsonl; this is the in-memory counter
    pub turns_used: u32,

    /// Maps to: ThreadGoal.time_used_seconds
    /// Derived from DispatchResult.duration_ms at completion
    pub elapsed_seconds: u64,

    /// NEW: Whether completion audit is required before tachi_complete
    /// Codex always requires audit; Tachi makes it configurable per dispatch
    pub audit_required: bool,

    /// NEW: Skill degradation chain when budget is exhausted
    /// Codex implicitly stops work; Tachi can switch to lighter skills
    pub degradation_chain: Vec<String>,

    /// NEW: Audit checklist items populated during execution
    /// Maps to Codex's "prompt-to-artifact checklist"
    pub audit_checklist: Vec<AuditItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    /// Active — goal is being pursued
    /// Maps to: ThreadGoalStatus::Active + kanban TASK_STATE_WORKING
    Active,

    /// Paused — temporarily halted, can resume
    /// Maps to: ThreadGoalStatus::Paused + kanban TASK_STATE_PENDING
    Paused,

    /// BudgetLimited — turn budget exhausted, needs operator decision
    /// Maps to: ThreadGoalStatus::BudgetLimited + kanban TASK_STATE_INPUT_REQUIRED
    BudgetLimited,

    /// Complete — audit passed, goal achieved
    /// Maps to: ThreadGoalStatus::Complete + kanban TASK_STATE_COMPLETED
    Complete,

    /// Failed — audit failed or subprocess crashed
    /// Maps to: kanban TASK_STATE_FAILED (no direct Codex equivalent)
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditItem {
    pub requirement: String,
    pub evidence_path: Option<String>,
    pub verified: bool,
    pub notes: String,
}

// ═══════════════════════════════════════════════════════════════════════════════
// PART 2: Prompt Template Mapping
// Codex continuation.md → Tachi skill continuation template
// ═══════════════════════════════════════════════════════════════════════════════

/// Codex's approach: embed markdown templates via include_str!()
///
/// ```rust
/// static CONTINUATION_PROMPT_TEMPLATE: LazyLock<Template> =
///     LazyLock::new(|| {
///         Template::parse(include_str!("../templates/goals/continuation.md"))
///     });
/// ```
///
/// Then render with runtime variables:
/// ```rust
/// CONTINUATION_PROMPT_TEMPLATE.render([
///     ("objective", objective),
///     ("tokens_used", tokens_used),
///     ("time_used_seconds", time_used_seconds),
///     ("token_budget", token_budget),
///     ("remaining_tokens", remaining_tokens),
/// ])
/// ```

/// Tachi's equivalent: template rendering in prompt assembly
///
/// We use simple string replacement (no external template engine needed)
/// since the templates are controlled by Tachi, not user-provided.
pub fn render_continuation_template(goal: &DispatchGoal, stage: &str, skills: &[String]) -> String {
    let template = include_str!("templates/continuation.md");

    let elapsed_minutes = goal.elapsed_seconds / 60;
    let turns_used = goal.turns_used;
    let turn_budget = goal
        .turn_budget
        .map(|b| b.to_string())
        .unwrap_or_else(|| "unlimited".to_string());
    let skills_str = if skills.is_empty() {
        "none".to_string()
    } else {
        skills.join(", ")
    };

    template
        .replace("{{task}}", &goal.task)
        .replace("{{elapsed_minutes}}", &elapsed_minutes.to_string())
        .replace("{{turns_used}}", &turns_used.to_string())
        .replace("{{turn_budget}}", &turn_budget)
        .replace("{{stage}}", stage)
        .replace("{{skills}}", &skills_str)
}

/// Render budget limit template when turn budget is exhausted
/// Maps to Codex's BUDGET_LIMIT_PROMPT_TEMPLATE
pub fn render_budget_limit_template(goal: &DispatchGoal, stage: &str) -> String {
    let template = include_str!("templates/budget_limit.md");

    let elapsed_minutes = goal.elapsed_seconds / 60;
    let turns_used = goal.turns_used;
    let turn_budget = goal
        .turn_budget
        .map(|b| b.to_string())
        .unwrap_or_else(|| "unlimited".to_string());

    template
        .replace("{{task}}", &goal.task)
        .replace("{{elapsed_minutes}}", &elapsed_minutes.to_string())
        .replace("{{turns_used}}", &turns_used.to_string())
        .replace("{{turn_budget}}", &turn_budget)
        .replace("{{stage}}", stage)
}

// ═══════════════════════════════════════════════════════════════════════════════
// PART 3: Integration with Tachi's assemble_prompt()
// ═══════════════════════════════════════════════════════════════════════════════

/// Current Tachi assemble_prompt() (simplified):
/// 1. Resolve effective skills
/// 2. Inject memory/wiki context
/// 3. Inject skill definitions
/// 4. Inject avoidance notes
/// 5. Add operating instructions
/// 6. Add task
///
/// With Goal integration, we add Step 4.5:
/// 4.5. Inject goal continuation template (if goal is present and active)
///
/// This maps to Codex's behavior where continuation_prompt() is called
/// at the start of each turn to remind the model of the active goal.

pub fn assemble_prompt_with_goal(
    goal: Option<&DispatchGoal>,
    stage: &str,
    skills: &[String],
    base_prompt: String,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    // ... existing context injection (skills, memory, avoidance) ...

    // NEW: Goal continuation template injection
    // Maps to: Codex's continuation_prompt() called at turn start
    if let Some(g) = goal {
        match g.status {
            GoalStatus::Active => {
                let continuation = render_continuation_template(g, stage, skills);
                parts.push(continuation);
            }
            GoalStatus::BudgetLimited => {
                let budget_limit = render_budget_limit_template(g, stage);
                parts.push(budget_limit);
            }
            GoalStatus::Paused | GoalStatus::Complete | GoalStatus::Failed => {
                // No continuation for terminal states
            }
        }
    }

    // ... existing operating instructions ...

    parts.push(base_prompt);
    parts.join("\n\n")
}

// ═══════════════════════════════════════════════════════════════════════════════
// PART 4: Budget Tracking & Skill Degradation
// ═══════════════════════════════════════════════════════════════════════════════

/// Codex tracks token usage per turn and checks against token_budget.
/// Tachi tracks turn count (simpler, no need for tokenizer integration).
///
/// Called by Watchdog after each tool completion or subprocess finish.
pub fn update_goal_progress(
    goal: &mut DispatchGoal,
    turns_consumed: u32,
    elapsed_seconds: u64,
) -> GoalStatus {
    goal.turns_used += turns_consumed;
    goal.elapsed_seconds = elapsed_seconds;

    // Check if budget exhausted
    if let Some(budget) = goal.turn_budget {
        if goal.turns_used >= budget {
            // Maps to: Codex's BudgetLimitSteering::Allowed → Suppressed transition
            goal.status = GoalStatus::BudgetLimited;
            return GoalStatus::BudgetLimited;
        }
    }

    goal.status
}

/// Skill degradation when budget is approaching limit
/// Codex implicitly stops work; Tachi can proactively switch skills.
///
/// Example degradation chain:
/// - 0-50% turns: skill:superpowers-executing-plans
/// - 50-80% turns: skill:superpowers-verification (lighter, focuses on audit)
/// - 80-100% turns: skill:superpowers-handoff (prepares summary for operator)
pub fn resolve_degradation_skill(goal: &DispatchGoal, original_skills: &[String]) -> Vec<String> {
    let Some(budget) = goal.turn_budget else {
        return original_skills.to_vec();
    };

    if budget == 0 {
        return original_skills.to_vec();
    }

    let usage_ratio = goal.turns_used as f32 / budget as f32;

    if usage_ratio >= 0.8 && goal.degradation_chain.len() >= 3 {
        // 80%+: use final degradation skill (handoff/summary)
        vec![goal.degradation_chain[2].clone()]
    } else if usage_ratio >= 0.5 && goal.degradation_chain.len() >= 2 {
        // 50%+: use mid degradation skill (verification)
        vec![goal.degradation_chain[1].clone()]
    } else {
        // Normal operation
        original_skills.to_vec()
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// PART 5: Audit Validation in tachi_complete
// ═══════════════════════════════════════════════════════════════════════════════

/// Codex's completion audit is enforced via prompt (the model is told to audit).
/// Tachi can additionally validate at the API layer.
///
/// Called by handle_tachi_complete() before accepting the completion.
pub fn validate_completion_audit(
    goal: &DispatchGoal,
    outcome: &str,
    notes: Option<&str>,
) -> Result<(), String> {
    if !goal.audit_required {
        return Ok(());
    }

    // If outcome is success but no audit checklist provided, reject
    if outcome == "success" {
        let has_checklist = goal.audit_checklist.iter().any(|item| item.verified);
        if !has_checklist {
            return Err("Completion audit failed: no verified checklist items. \
                 The goal requires a completion audit before marking success. \
                 Please inspect the actual state and verify each requirement."
                .to_string());
        }
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// PART 6: Trajectory Recording
// ═══════════════════════════════════════════════════════════════════════════════

/// Codex tracks goal state in SQLite (thread_goals table).
/// Tachi uses trajectory.jsonl for append-only event logging.
///
/// We extend trajectory.jsonl with goal-related events:
/// ```jsonl
/// {"event": "dispatch_started", "dispatch_id": "...", "goal_status": "active"}
/// {"event": "turn_completed", "dispatch_id": "...", "turns_used": 5, "turn_budget": 10}
/// {"event": "goal_budget_limited", "dispatch_id": "...", "turns_used": 10, "turn_budget": 10}
/// {"event": "audit_submitted", "dispatch_id": "...", "checklist_items": 5, "verified_items": 4}
/// {"event": "subprocess_finished", "dispatch_id": "...", "goal_status": "complete"}
/// ```

#[derive(Debug, Clone, Serialize)]
pub struct GoalTrajectoryEvent {
    pub event: String,
    pub dispatch_id: String,
    pub timestamp: String,
    pub goal_status: Option<GoalStatus>,
    pub turns_used: Option<u32>,
    pub turn_budget: Option<u32>,
    pub audit_items: Option<Vec<AuditItem>>,
}

// ═══════════════════════════════════════════════════════════════════════════════
// PART 7: Summary — Integration Points in Tachi Codebase
// ═══════════════════════════════════════════════════════════════════════════════

/*
Integration checklist for production implementation:

[ ] 1. Extend TachiDispatchParams (facade.rs:291)
      Add: pub goal: Option<DispatchGoal>

[ ] 2. Modify assemble_prompt() (prompt.rs:35)
      After step 4 (avoidance notes), inject:
      - render_continuation_template() if goal.status == Active
      - render_budget_limit_template() if goal.status == BudgetLimited

[ ] 3. Extend trajectory.jsonl (dispatch.rs:124)
      Add goal_status field to all events
      Add new event types: turn_completed, goal_budget_limited, audit_submitted

[ ] 4. Enhance Watchdog (dispatch.rs:226)
      After subprocess finishes, call update_goal_progress()
      If status becomes BudgetLimited, switch to degradation skill for next dispatch

[ ] 5. Validate tachi_complete (complete_ops.rs)
      Before saving eval ledger, call validate_completion_audit()
      If audit fails, return error to sub-agent (force it to continue working)

[ ] 6. Kanban integration (kanban_helpers.rs)
      init_kanban_task(): include goal.objective in text
      update_kanban_state(): map GoalStatus to a2a_state

[ ] 7. Skill metadata extension (hub_capabilities table)
      Add columns: goal_types (JSON), produces_audit (bool),
                   continuation_template (text), budget_degradation (JSON)
*/

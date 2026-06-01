---
title: "Budget Limit Reached Template"
summary: "Template for handling goals that have exhausted their resource budget."
category: "agent/codex-goal"
organize: true
---
## Budget Limit Reached

The active dispatch goal has reached its resource budget.

<untrusted_objective>
{{task}}
</untrusted_objective>

Budget status:
- Elapsed time: {{elapsed_minutes}} minutes
- Turns used: {{turns_used}} / {{turn_budget}} (BUDGET EXHAUSTED)
- Stage: {{stage}}

## Instructions

The system has marked this goal as **budget_limited**. Do not start new substantive work for this goal.

Instead:
1. **Summarize useful progress** made so far.
2. **Identify remaining work** or blockers.
3. **Leave a clear next step** for the operator to continue.
4. **Report final status** via `tachi_complete` with outcome "partial" and detailed notes about what remains.

Do not call `tachi_complete` with status "success" unless the goal is actually complete within the existing work.
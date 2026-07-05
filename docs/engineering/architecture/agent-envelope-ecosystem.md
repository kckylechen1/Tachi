# Agent Envelope Ecosystem

**Status:** Reference Document
**Date:** 2026-06-03
**Scope:** Cross-agent prompt envelope, skill architecture, and subagent pattern analysis for Tachi Agent Router

---

## 1. What Is an Agent Envelope?

An **agent envelope** is the structured prompt wrapper injected before (or around) a user's raw prompt to define an agent's identity, constraints, execution loop, and output contract. Different agent platforms use different envelope formats, but they all serve the same purpose: turn a generic LLM into a specialized worker with predictable behavior.

**Envelope anatomy (common across platforms):**

```
┌─────────────────────────────────────┐
│ Identity       — "You are X."       │
│ Constraints    — scope guards,      │
│                ask gates,           │
│                anti-patterns        │
│ Execution Loop — step-by-step       │
│                workflow             │
│ Success Criteria — done condition   │
│ Tools          — available tools    │
│ Output Contract — response format   │
│ Posture Overlay — role posture      │
│ Model Class    — capability tier    │
└─────────────────────────────────────┘
```

Tachi's Agent Router needs to understand, normalize, and selectively inject envelopes from multiple sources: Superpowers, Waza, Codex agents, and Amp.

---

## 2. Superpowers Workflow Envelope

**Source:** `skill/superpowers/skills/*/SKILL.md`
**Maintainer:** obra
**License:** MIT
**Pattern:** Workflow skeleton (5-stage lifecycle)

### 2.1 Envelope Format

Superpowers skills are Markdown files with YAML frontmatter:

```yaml
---
name: brainstorming
description: "You MUST use this before any creative work..."
---
```

Each skill defines a **complete workflow** with hard gates, checklists, process flows (DOT digraphs), and terminal states. Skills chain together:

```
brainstorming → writing-plans → executing-plans → requesting-code-review → finishing-a-development-branch
```

### 2.2 Skill-by-Skill Envelope Breakdown

#### `brainstorming` — Design Before You Build

| Element | Content |
|---------|---------|
| Identity | "Help turn ideas into fully formed designs and specs through natural collaborative dialogue." |
| Hard Gate | `<HARD-GATE>` — Do NOT invoke any implementation skill until design is approved |
| Checklist | 9 steps: explore context → visual companion → questions → 2-3 approaches → present design → write spec → self-review → user review → invoke writing-plans |
| Terminal State | Invoke `writing-plans` ONLY |
| Anti-Pattern | "This is too simple to need a design" |

Key constraint: **Every project goes through this process.** No exceptions.

#### `writing-plans` — Bite-Sized Implementation Plans

| Element | Content |
|---------|---------|
| Identity | "Write comprehensive implementation plans assuming the engineer has zero context." |
| Output | `docs/superpowers/plans/YYYY-MM-DD-<feature>.md` |
| Task Granularity | Each step = one action (2-5 minutes) |
| Required Header | Goal / Architecture / Tech Stack |
| DRY / YAGNI / TDD | Enforced in plan quality |

Key constraint: Plans must be decomposed into independently testable subsystems.

#### `executing-plans` — Execute With Review Checkpoints

| Element | Content |
|---------|---------|
| Identity | "Load plan, review critically, execute all tasks, report when complete." |
| Entry Condition | Written plan exists |
| Stop Conditions | Blocker, critical gap, repeated verification failure |
| Exit Action | Invoke `finishing-a-development-branch` |
| Subagent Note | "Quality significantly higher if run on a platform with subagent support" |

Key constraint: **Don't skip verifications. Stop when blocked, don't guess.**

#### `requesting-code-review` — Review Early, Review Often

| Element | Content |
|---------|---------|
| Identity | "Dispatch a code reviewer subagent to catch issues before they cascade." |
| Mandatory | After each task in subagent-driven dev; after major feature; before merge |
| Reviewer Context | Precisely crafted — never session history |
| Action | Critical → fix immediately; Important → fix before proceed; Minor → note for later |

Key constraint: Reviewer gets **work product context only**, not agent's thought process.

#### `finishing-a-development-branch` — Verify → Present → Execute

| Element | Content |
|---------|---------|
| Identity | "Guide completion of development work by presenting clear options." |
| Core Principle | Verify tests → Detect environment → Present options → Execute choice → Clean up |
| Options | 1. Merge locally / 2. Push & PR / 3. Keep branch / 4. Discard |
| Worktree Awareness | Detects normal repo vs git worktree vs detached HEAD |

### 2.3 Superpowers Scripts & Agents

Beyond SKILL.md files, Superpowers includes:

| File | Purpose |
|------|---------|
| `brainstorming/scripts/visual-companion.md` | Local HTTP server for visual design companion |
| `brainstorming/spec-document-reviewer-prompt.md` | Spec reviewer subagent prompt |
| `writing-plans/plan-document-reviewer-prompt.md` | Plan reviewer subagent prompt |
| `requesting-code-review/code-reviewer.md` | Code reviewer subagent prompt |
| `systematic-debugging/find-polluter.sh` | Bash script for finding test polluters |
| `subagent-driven-development/{implementer,spec-reviewer,code-quality-reviewer}-prompt.md` | Subagent prompt templates |

### 2.4 Tachi Integration Notes

Superpowers provides **workflow skeleton** (brainstorm → plan → execute → review → ship). Tachi's `shell_ops` 5-stage lifecycle maps directly:

```rust
fn meta_skill_for_stage(stage: &str) -> Option<&'static str> {
    match stage {
        "brainstorm" => Some("skill/superpowers/skills/brainstorming/SKILL.md"),
        "plan"       => Some("skill/superpowers/skills/writing-plans/SKILL.md"),
        "dispatch"   => Some("skill/superpowers/skills/executing-plans/SKILL.md"),
        "review"     => Some("skill/superpowers/skills/requesting-code-review/SKILL.md"),
        "ship"       => Some("skill/superpowers/skills/finishing-a-development-branch/SKILL.md"),
        _ => None,
    }
}
```

**Gap:** Superpowers skills are static Markdown — no runtime, no tool schema, no machine-readable I/O contract between stages.

---

## 3. Waza Capability Envelope

**Source:** `/Users/kckylechen/.agents/skills/*/SKILL.md`
**Maintainer:** tw93
**License:** MIT
**Pattern:** Capability muscles (8 high-quality prompt skills)

### 3.1 Envelope Format

Waza skills use YAML frontmatter + structured Markdown with sections:
- Outcome Contract
- Mode Picker
- Hard Stops
- Autofix routing
- Evidence requirements

### 3.2 Skill-by-Skill Envelope Breakdown

#### `think` — Design and Validate Before You Build

| Element | Content |
|---------|---------|
| Identity | "Turn a rough idea into an approved plan. No code until user approves." |
| Modes | Lightweight (2-3 sentences) / Full / Evaluation (Kill/Keep/Pivot) |
| Hard Stop | "Do not use a build-plan template here. Do not list options. Give one verdict." |
| Evidence | Current repo state, project docs, live external docs, prior decisions |

#### `check` — Review Before You Ship

| Element | Content |
|---------|---------|
| Identity | "Read the diff, find the problems, fix what can be fixed safely, ask about the rest." |
| Worktree Safety | `git status --short --branch -uall` preflight |
| Modes | Plan Execution / Default Review / Triage / Release Worthiness / Ship / Project Audit |
| Hard Stop | "Do not run `git switch`, `git checkout`, `git reset --hard`, `git clean`, `git stash` without explicit approval." |

#### `hunt` — Find Root Cause Before Fixing

| Element | Content |
|---------|---------|
| Identity | "Find root cause before applying fixes." |
| Evidence Chain | Reproduction → Isolation → Hypothesis → Verification |
| Anti-Pattern | "Don't fix symptoms. Don't patch around the bug." |

#### `design` — Distinctive Production UI

| Element | Content |
|---------|---------|
| Identity | "Produce distinctive, production-grade UI." |
| Anti-AI-Slop | Typography, color, motion, background, layout — all have "avoid defaults" rules |
| Screenshot-Driven | Can take screenshots and iterate visually |

#### `write` — Polish Prose (Chinese/English)

| Element | Content |
|---------|---------|
| Identity | "Remove AI-like wording while preserving intent." |
| Target | Drafts, docs, release notes, launch copy, social posts |
| Anti-Pattern | "Not for code comments or technical specs." |

#### `learn` — Six-Phase Research Workflow

| Element | Content |
|---------|---------|
| Identity | "Turn unfamiliar domains into publish-ready output." |
| Phases | Scope → Source → Extract → Synthesize → Structure → Polish |
| Output | One coherent reference document |

#### `read` — Fetch and Summarize URLs/PDFs

| Element | Content |
|---------|---------|
| Identity | "Fetch source content, default to concise summaries." |
| Capabilities | URLs, PDFs, HTML → Markdown |

#### `health` — Engineering Health Audit

| Element | Content |
|---------|---------|
| Identity | "Budget-aware agent-assisted engineering health audit." |
| Targets | Config drift, hooks/MCP, verifier surfaces, AI maintainability |

### 3.3 Tachi Integration Notes

Waza provides **capability muscles** that Tachi can dispatch as sub-tasks:

```rust
pub fn waza_skill_for_task(task_type: &str) -> Option<&'static str> {
    match task_type {
        "design"    => Some("/Users/kckylechen/.agents/skills/design/SKILL.md"),
        "review"    => Some("/Users/kckylechen/.agents/skills/check/SKILL.md"),
        "debug"     => Some("/Users/kckylechen/.agents/skills/hunt/SKILL.md"),
        "plan"      => Some("/Users/kckylechen/.agents/skills/think/SKILL.md"),
        "doc"       => Some("/Users/kckylechen/.agents/skills/write/SKILL.md"),
        "research"  => Some("/Users/kckylechen/.agents/skills/learn/SKILL.md"),
        "audit"     => Some("/Users/kckylechen/.agents/skills/health/SKILL.md"),
        _ => None,
    }
}
```

**Gap:** Waza skills are static prompts — no runtime, no workflow chaining, no context transfer protocol between skills.

---

## 4. Codex Agent Envelope (Detailed)

**Source:** `~/.codex/agents/*.toml` (21 files)
**Maintainer:** OpenAI (oh-my-codex)
**Pattern:** Role-based agent definitions with XML-structured developer instructions

### 4.1 Envelope Format

Each Codex agent is a TOML file with 5 top-level fields:

```toml
name = "architect"
description = "System design, boundaries, interfaces, long-horizon tradeoffs"
model = "gpt-5.4"
model_reasoning_effort = "high"
developer_instructions = """
<identity>...</identity>
<constraints>...</constraints>
<execution_loop>...</execution_loop>
<style>...</style>
<posture_overlay>...</posture_overlay>
<model_class_guidance>...</model_class_guidance>
## OMX Agent Metadata
- role: architect
- posture: frontier-orchestrator
- model_class: frontier
- routing_role: leader
- resolved_model: gpt-5.4
"""
```

### 4.2 XML Tag Taxonomy (Common Across All Agents)

| Tag | Purpose | Frequency |
|-----|---------|-----------|
| `<identity>` | Role definition, self-concept | 100% |
| `<constraints>` | Scope guards, ask gates, reasoning effort | 100% |
| `<execution_loop>` | Step-by-step workflow with success criteria | 100% |
| `<verification_loop>` | Post-execution validation steps | ~60% |
| `<tool_persistence>` | Retry rules, never-skip rules | ~40% |
| `<tools>` | Available tool list with usage guidance | ~80% |
| `<style>` | Output contract, scenario handling, final checklist | 100% |
| `<delegation>` | Escalation rules, trust boundaries | ~30% |
| `<anti_patterns>` | Explicitly forbidden behaviors | ~50% |
| `<scenario_handling>` | Good/bad examples for common situations | ~60% |
| `<posture_overlay>` | Orchestrator vs worker posture | 100% |
| `<model_class_guidance>` | Frontier vs standard model tuning | 100% |
| `## OMX Agent Metadata` | Machine-readable role classification | 100% |

### 4.3 Agent-by-Agent Envelope Breakdown

#### `architect` — System Design Oracle

```toml
name = "architect"
description = "System design, boundaries, interfaces, long-horizon tradeoffs"
model = "gpt-5.4"
model_reasoning_effort = "high"
posture = "frontier-orchestrator"
routing_role = "leader"
```

**Key constraints:**
- Never write or edit files (read-only)
- Never judge code you have not opened
- Default to concise, evidence-dense analysis

**Execution loop:** Gather context → Form hypothesis → Cross-check against code → Return summary + root cause + recommendations + tradeoffs

**Output contract:**
```markdown
## Summary
## Analysis (file:line references)
## Root Cause
## Recommendations (priority, effort, impact)
## Trade-offs (table: Option | Pros | Cons)
## Consensus Addendum (ralplan reviews only)
```

#### `executor` — Implementation Worker

```toml
name = "executor"
description = "Code implementation, refactoring, feature work"
model = "gpt-5.4"
model_reasoning_effort = "high"
posture = "deep-worker"
routing_role = "executor"
```

**Key constraints:**
- KEEP GOING UNTIL THE TASK IS FULLY RESOLVED
- Prefer smallest viable diff
- Do not stop at partial completion
- `.omx/plans/` files are read-only

**Execution loop:** Explore → Plan → TodoWrite → Implement → Verify (lsp_diagnostics, tests, build)

**Success criteria (6 points):**
1. Requested behavior implemented
2. lsp_diagnostics clean on modified files
3. Relevant tests pass
4. Build/typecheck succeeds
5. No temporary/debug leftovers
6. Concrete verification evidence in final output

**Unique feature:** Lore commit protocol
```
Intent line first (why, not what)
Constraint: external forces
Rejected: <alternative> | <reason>
Directive: warnings for future modifiers
Confidence: low/medium/high
Scope-risk: narrow/moderate/broad
Tested: / Not-tested:
```

#### `team-executor` — Supervised Team Worker

```toml
name = "team-executor"
description = "Supervised team execution for conservative delivery lanes"
model = "gpt-5.4"
model_reasoning_effort = "medium"
posture = "deep-worker"
routing_role = "executor"
```

**Key difference from executor:**
- Respects leader's plan and task boundaries
- Prefer direct completion over speculative fanout
- Conservative interpretation in ambiguous work
- Report concise evidence back to leader

#### `code-reviewer` — Code Review Specialist

**Key constraints:**
- Read code before judging it
- Distinguish style preferences from real issues
- Check for security, correctness, maintainability
- Verify tests cover the change

**Output contract:**
```markdown
## Summary (verdict + confidence)
## Issues Found (severity: Critical / Important / Minor)
## Strengths
## Recommendations
```

#### `debugger` — Debug Investigator

**Key constraints:**
- Reproduce before diagnosing
- Isolate variables systematically
- Form hypothesis before searching
- Verify fix with tests

#### `planner` — Task Planner

**Key constraints:**
- Break tasks into independently testable pieces
- Identify dependencies and order
- Estimate effort and risk
- Define done criteria for each task

#### `analyst` — Data/Behavior Analyst

**Key constraints:**
- Distinguish correlation from causation
- Quantify where possible
- Acknowledge uncertainty
- Suggest next investigation steps

#### `test-engineer` — Test Specialist

**Key constraints:**
- Cover happy path, edge cases, and error paths
- Prefer table-driven tests
- Mock external dependencies
- Verify test failure before fix, test pass after fix

#### `security-reviewer` — Security Auditor

**Key constraints:**
- Check input validation
- Verify authentication/authorization
- Look for injection vulnerabilities
- Review secret handling
- Assess supply chain risks

### 4.4 Complete Agent Registry

| Agent | Role | Model | Posture | Routing |
|-------|------|-------|---------|---------|
| `architect` | Design | gpt-5.4 | frontier-orchestrator | leader |
| `executor` | Implement | gpt-5.4 | deep-worker | executor |
| `team-executor` | Team impl | gpt-5.4 | deep-worker | executor |
| `code-reviewer` | Review | gpt-5.4 | — | reviewer |
| `debugger` | Debug | gpt-5.4 | — | investigator |
| `planner` | Plan | gpt-5.4 | — | planner |
| `analyst` | Analyze | gpt-5.4 | — | analyst |
| `test-engineer` | Test | gpt-5.4 | — | tester |
| `security-reviewer` | Security | gpt-5.4 | — | auditor |
| `designer` | UI/UX | gpt-5.4 | — | designer |
| `writer` | Docs | gpt-5.4 | — | writer |
| `researcher` | Research | gpt-5.4 | — | researcher |
| `verifier` | Verify | gpt-5.4 | — | verifier |
| `vision` | Image | gpt-5.4 | — | vision |
| `dependency-expert` | Deps | gpt-5.4 | — | expert |
| `git-master` | Git | gpt-5.4 | — | expert |
| `build-fixer` | Build | gpt-5.4 | — | fixer |
| `code-simplifier` | Simplify | gpt-5.4 | — | refactor |
| `explorer` | Explore | gpt-5.4 | — | scout |
| `critic` | Critique | gpt-5.4 | — | critic |

### 4.5 Tachi Integration: Codex Agent Envelope Adapter

Since `codex exec` has **no `--agent` CLI flag**, Tachi must inject agent instructions manually:

```rust
pub fn apply_codex_agent_envelope(prompt: &str, agent_name: &str) -> String {
    let toml_path = format!("{}/.codex/agents/{}.toml", home_dir(), agent_name);
    let agent = parse_toml(&toml_path).ok()?;

    format!(
        "[AGENT: {}] {}\n\n<developer_instructions>\n{}\n</developer_instructions>\n\n---\n\n{}",
        agent.name,
        agent.description,
        agent.developer_instructions.trim(),
        prompt
    )
}
```

**Task type mapping:**

| Tachi Task Type | Codex Agent |
|-----------------|-------------|
| `design` | `architect` |
| `code` | `executor` |
| `review` | `code-reviewer` |
| `test` | `test-engineer` |
| `doc` | `writer` |
| `debug` | `debugger` |
| `plan` | `planner` |
| `security` | `security-reviewer` |
| `dependency` | `dependency-expert` |
| `git` | `git-master` |
| `vision` | `vision` |

---

## 5. Amp Envelope & Subagent Architecture

**Source:** `~/.amp/bin/amp` (67MB Bun binary, reverse-engineered)
**Maintainer:** Amp Code
**Pattern:** 9 prompt variants × 3 subagent types × feature-flag model routing

### 5.1 Prompt-Model Joint Routing

Amp does not use one prompt for all models. It has **9 prompt variants** selected dynamically:

```
agentMode === "deep"      → sFR()  // GPT-5.5 Deep Autonomous
agentMode === "deep" + FF → nFR()  // GPT-5.4 Deep Fallback
agentMode === "rush"      → qFR()  // Quick execution
agentMode === "aggman"    → _FR()  // Slack/Project management
provider === "openai"     → yFR()  // GPT generic
model === "gpt-5-codex"   → kFR()  // Codex mode
provider === "xai"        → SFR()  // xAI
model === "kimi-k2"       → mFR()  // Kimi
provider === "vertexai"   → lFR()  // Gemini
default                   → oFR()  // Pair Programming
```

**Feature flag model selection:**
```javascript
if (agentMode === "deep") {
    let canUseGPT55 = ATR(serverStatus);  // server-side feature flag
    return canUseGPT55 ? "deep" : "deep-gpt5.4";
}
```

This enables **gray-scale releases** and **instant rollback** without CLI updates.

### 5.2 Deep Mode Envelope (sFR) — GPT-5.5

| Element | Content |
|---------|---------|
| Identity | "You are Amp, an autonomous coding agent." |
| Autonomy | "carry through implementation and verification rather than stopping at a proposal" |
| Discovery | "Read enough code to avoid guessing, then stop." |
| Pragmatism | "The best change is often the smallest correct change." |
| Verification | "scale with risk and blast radius" |
| Communication | commentary channel (1-2 sentences) + final channel (outcome) |
| Anti-AI-Slop | None needed (GPT-5.5 handles it natively) |

### 5.3 Deep Fallback Envelope (nFR) — GPT-5.4

Same as Deep but adds:
- Complete frontend anti-AI-slop rules (typography, color, motion, background, layout)
- More explicit reasoning guidance
- "Verify your work before reporting"

### 5.4 Three Subagent Types

| Subagent | Model | Purpose | Prompt Strategy |
|----------|-------|---------|-----------------|
| **Oracle** | GPT-5.5 | Senior engineering advisor | Precise problem, attach files, ask for trade-offs |
| **Task Tool** | Same as parent | Fire-and-forget executor | Detailed instructions, deliverables, validation steps |
| **Codebase Search** | Same as parent | Conceptual code explorer | Real-world behavior, hints, desired output format |

**Decision tree:**
```
"Senior engineer to think with" → Oracle
"Find code matching a concept"  → Codebase Search
"Large-scale multi-step execution" → Task Tool
```

**Recommended workflow:**
```
Oracle (plan) → Codebase Search (validate scope) → Task Tool (execute)
```

### 5.5 Parallel Execution Rules

| Can Parallel | Must Serial |
|-------------|-------------|
| Oracle on different topics | Plan → Code |
| Codebase Search on different paths | Same file, multiple Tasks |
| Tasks with disjoint write targets | Chain transforms (B depends on A) |
| All reads/searches/diagnostics | — |

### 5.6 Compaction / TODO / Handoff (Cross-Agent Comparison)

| Feature | Claude Code | OpenCode | Amp | Codex CLI |
|---------|-------------|----------|-----|-----------|
| **Compaction trigger** | Context limit | 95% context | Server-side | None (direct truncation) |
| **TODO storage** | TaskCreate tool (independent) | SQLite `todo` table | `todo_read/write` tool | None |
| **TODO survives compaction?** | ✅ Yes | ❌ No (unstructured summary) | ✅ Yes | N/A |
| **Session storage** | Local JSONL | Local SQLite | Server-side (ampcode.com) | None |
| **Handoff** | Memory system | `parent_id` | Thread workflow + callback | None |
| **Subagent** | Generic Agent tool | Read-only Agent tool | 3 specialized types | None |

**Key lesson for Tachi:** TODO must be **tool-level persistent** (not conversation-level) to survive compaction. Tachi's `tachi_task` tool already does this.

### 5.7 Amp Engineering Highlights (Relevant to Tachi)

| Feature | Amp Implementation | Tachi Equivalent |
|---------|-------------------|------------------|
| Prompt-model joint optimization | 9 prompt variants | Request Envelope layer |
| Dual-channel output | commentary + final | Not yet implemented |
| Scaffold customization | `replaceAll` / `replaceBase` | Request Envelope template |
| Feature flag model routing | Server-side ATR() | AgentRegistry + Classifier Router |
| Parallel execution rules | Explicit can/must matrix | Dispatcher concurrency config |
| Verification risk grading | typo → local → cross-module | Not yet implemented |
| File change recording | before/after JSON (not diff) | Not yet implemented |
| MCP engineering | OAuth, auto-discovery, deferred load | MCP Config Adapter |
| Git safety discipline | NEVER revert others' changes | Reliability wrappers |
| Discovery discipline | "Read enough, then stop" | Request Envelope guidance |

---

## 6. Cross-Agent Envelope Comparison Matrix

| Dimension | Superpowers | Waza | Codex Agents | Amp |
|-----------|-------------|------|--------------|-----|
| **What** | Workflow skeleton | Capability muscles | Role-based workers | Full IDE agent |
| **Format** | Markdown + YAML frontmatter | Markdown + YAML frontmatter | TOML + XML instructions | Binary + server-side prompts |
| **Runtime** | None (static) | None (static) | Node.js (`codex exec`) | Bun SEA + server |
| **Subagent** | Yes (subagent-driven-dev) | No | No (internal agent TOML) | Yes (3 types) |
| **Envelope injection** | Skill invocation | Skill invocation | `--agent` (interactive only) | Server-side prompt routing |
| **Tool schema** | None | None | None (implicit) | 12 built-in tools |
| **Chainability** | Fixed 5-stage chain | None | None | Oracle→Search→Task chain |
| **Model routing** | None | None | Per-agent `model` field | 9 prompts × feature flag |
| **Compaction-aware** | N/A | N/A | N/A | Server-side + tool-level TODO |
| **Cost control** | None | None | None | Risk-graded verification |

---

## 7. Tachi Integration Strategy

### 7.1 Envelope Normalization Layer

Tachi needs a **unified envelope format** that can consume from all four sources:

```rust
pub struct UnifiedEnvelope {
    pub source: EnvelopeSource,  // Superpowers / Waza / Codex / Amp
    pub identity: String,
    pub description: String,
    pub constraints: Vec<String>,
    pub execution_loop: Vec<String>,
    pub success_criteria: Vec<String>,
    pub tools: Vec<String>,
    pub output_contract: String,
    pub posture: AgentPosture,
    pub model_class: ModelClass,
}

pub enum EnvelopeSource {
    Superpowers { skill_name: String, skill_path: String },
    Waza { skill_name: String, skill_path: String },
    Codex { agent_name: String, toml_path: String },
    Amp { prompt_variant: String, agent_mode: String },
}
```

### 7.2 Envelope Application Pipeline

```
User Prompt
    ↓
Tachi Selector (task_type → agent)
    ↓
Envelope Loader (read skill/agent definition)
    ↓
Envelope Normalizer (convert to UnifiedEnvelope)
    ↓
Envelope Renderer (inject into agent-specific format)
    ↓
Dispatch (spawn agent subprocess)
```

### 7.3 Per-Source Rendering

| Source | Render Strategy |
|--------|----------------|
| **Claude `-p`** | `--system-prompt "<envelope>"` or `--agents <json>` |
| **Codex `exec`** | Prepend to prompt (no `--agent` CLI flag) |
| **Gemini `-p`** | Inject in prompt (no `--system-prompt` flag) |
| **Qwen** | `--system-prompt "<envelope>"` |
| **Copilot `-p`** | `--agent <name>` (if predefined) or prepend |
| **Droid `exec`** | `--append-system-prompt "<envelope>"` |

### 7.4 Gaps to Fill

1. **Machine-readable I/O schema** between skills/stages — none of the four sources define structured input/output contracts
2. **Context transfer protocol** — how does one agent hand off state to another?
3. **Cost attribution** — per-envelope, per-stage cost tracking
4. **Envelope versioning** — how to detect when a skill/agent definition has changed?
5. **Runtime envelope mutation** — can Tachi dynamically adjust an envelope based on mid-task feedback?

---

## 8. Reference Source Files

### 8.1 Superpowers (obra)

**In-project path:** `skill/superpowers/skills/`

```
skill/superpowers/skills/
├── brainstorming/SKILL.md
│   ├── spec-document-reviewer-prompt.md
│   └── visual-companion.md
├── writing-plans/SKILL.md
│   └── plan-document-reviewer-prompt.md
├── executing-plans/SKILL.md
├── requesting-code-review/SKILL.md
│   └── code-reviewer.md
└── finishing-a-development-branch/SKILL.md

skill/superpowers/skills/systematic-debugging/find-polluter.sh
skill/superpowers/skills/subagent-driven-development/
    ├── implementer-prompt.md
    ├── spec-reviewer-prompt.md
    └── code-quality-reviewer-prompt.md
```

### 8.2 Waza (tw93)

**In-project path:** `skill/waza/skills/` (copied from `~/.agents/skills/`)

```
skill/waza/skills/
├── check/SKILL.md
├── design/SKILL.md
├── frontend-design/SKILL.md
├── health/SKILL.md
├── hunt/SKILL.md
├── learn/SKILL.md
├── read/SKILL.md
├── tachi/SKILL.md
├── think/SKILL.md
└── write/SKILL.md
```

### 8.3 Codex Agents (OpenAI)

**In-project path:** `skill/codex/agents/` (copied from `~/.codex/agents/`)

```
skill/codex/agents/
├── architect.toml
├── executor.toml
├── team-executor.toml
├── code-reviewer.toml
├── debugger.toml
├── planner.toml
├── analyst.toml
├── test-engineer.toml
├── security-reviewer.toml
├── designer.toml
├── writer.toml
├── researcher.toml
├── verifier.toml
├── vision.toml
├── dependency-expert.toml
├── git-master.toml
├── build-fixer.toml
├── code-simplifier.toml
├── explorer.toml
├── critic.toml
└── (21 total)
```

### 8.4 Amp (Amp Code)

**In-project path:** `skill/amp/analysis/` (copied from `~/Desktop/amp_analysis/`)

```
skill/amp/analysis/
├── 00_agent_model_analysis_report.md      # Model comparison (768 sessions)
├── 01_deep_autonomous_prompt.md           # sFR() GPT-5.5 Deep
├── 02_deep_fallback_prompt.md             # nFR() GPT-5.4 Fallback
├── 03_pair_programming_prompt.md          # oFR() Pair Programming
├── 04_agent_architecture.md               # Subagent architecture
├── 05_prompt_routing_and_models.md        # NFR/wFR/HFR routing
├── 06_engineering_highlights.md           # 13 engineering highlights
├── 07_compaction_todo_handoff_comparison.md  # Cross-agent comparison
├── 08_domestic_vs_frontier_models.md      # Model behavior analysis
├── 09_coding_agent_analysis_report.md     # 2,841 sessions, 278K turns
└── 12_full_session_distillation_report.md # Cross-platform analysis

# Original binary:
~/.amp/bin/amp                              # 67MB Bun SEA binary
~/.amp/file-changes/{thread_id}/            # File diff storage
```

### 8.5 UltraCode-Shim (OnlyTerp)

**External reference:** `https://github.com/OnlyTerp/UltraCode-Shim/`

```
config.example.json            # Model registry + route definitions
docs/HOW_IT_WORKS.md           # Reverse-engineering evidence
docs/AUTO_ROUTER.md            # Classifier router design
docs/ADD_A_MODEL.md            # Backend integration guide
```

### 8.6 Tachi Internal

```
crates/memory-server/src/shell_ops/mod.rs    # 5-stage lifecycle
crates/memory-server/src/dispatch_ops/       # Agent dispatch
crates/memory-server/src/claude_pool.rs      # Bounded concurrency
crates/memory-server-params/src/             # MCP tool schemas
crates/memory-server/src/llm.rs              # Multi-model backend lanes (Qwen/Claude)
```

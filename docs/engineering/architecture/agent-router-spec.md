# Agent Router Spec: Unified Multi-Agent Dispatch for Tachi

**Status:** Draft  
**Date:** 2026-06-03  
**Author:** kckylechen  
**Related:** `tachi_dispatch` tool, `ClaudePool`, `DispatchOps`  
**Fleet policy (canonical):** [`agent-fleet.md`](agent-fleet.md) — Phase 1 is **four** dispatch agents only.

> **Superseded execution default (2026-07-20, #1312):** this document describes Tachi's retained durable/remote dispatch backend, not the ordinary subagent path. Harness-native subagents are now the default; Tachi dispatch requires an explicit native-first exception. See [`dispatch-lifecycle.md`](dispatch-lifecycle.md#21-tiers--one-table-no-vibes) for current authority.

---

## 1. Goal

Tachi dispatch uses a **four-agent fleet** (`claude`, `codex`, `grok`, `kimi`) plus `custom` for ad-hoc commands. This spec describes the unified router, registry, envelopes, and reliability layers on top of that fleet.

| Agent | Model Family | Non-interactive Mode | JSON Output | MCP Injection | Context Window | Cost Tier |
|-------|-------------|---------------------|-------------|---------------|----------------|-----------|
| `claude` | Claude 4 | `-p` | ✅ `--output-format json` | ✅ `--mcp-config <file>` | 200K | premium |
| `codex` | GPT-5 / o3 | `exec` | ✅ `--json` | ❌ (via `~/.codex/config.toml`) | 200K | premium |
| `grok` | Grok | `-p` / `--single` | ✅ `--output-format json` | ✅ best-effort `--mcp-config` | 200K | premium |
| `kimi` | Kimi | `-p` | ✅ `--output-format json` | ❌ | 200K+ | standard |

**Deferred (not in Phase 1 registry):** gemini, qwen, copilot, droid — see `agent-fleet.md` for SFT remapping.

---

## 2. Non-Goals

- **Extra CLI agents** (gemini, qwen, copilot, droid, …) — not part of the Phase 1 fleet; see [`agent-fleet.md`](agent-fleet.md).
- **GUI agents** (Cursor, Trae, Cline, etc.) are out of scope for Phase 1. They may be added later via MCP server interface or HTTP API.
- **Agent-specific feature parity** (e.g. Codex's `codex --sandbox`) — we expose the common subset.
- **Real-time streaming** — Phase 1 returns complete results. Streaming may be added in Phase 3.
- **Qwen / ollama backend lanes** — background extract/summary/kanban only ([#151](https://github.com/kckylechen1/tachi/issues/151)), not `tachi_dispatch` workers.

---

## 3. Architecture

Inspired by [UltraCode-Shim](https://github.com/OnlyTerp/UltraCode-Shim/)'s proxy pattern, Tachi adds a **Request Envelope** layer and **Classifier Router** to normalize agent behavior regardless of backend.

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Tachi Memory Server                           │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌───────────┐  │
│  │tachi_staff │  │tachi_briefing (RETIRED)│  │tachi_memory │  │tachi_task │  │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘  └─────┬─────┘  │
│         └─────────────────┴─────────────────┴───────────────┘       │
│                                 │                                   │
│                    ┌────────────▼────────────┐                     │
│                    │     Agent Router        │                     │
│                    │  ┌──────────────────┐  │                     │
│                    │  │ Agent Registry   │  │  ← static config     │
│                    │  │ ┌───┬───┬───┬──┐ │  │                     │
│                    │  │ │cld│cdx│grk│ki│ │  │                     │
│                    │  │ │custom │auto│ │ │  │                     │
│                    │  │ └───┴───┴───┴──┘ │  │                     │
│                    │  └──────────────────┘  │                     │
│                    │  ┌──────────────────┐  │                     │
│                    │  │ Classifier Router│  │  ← active fleet scores tasks │
│                    │  └──────────────────┘  │                     │
│                    │  ┌──────────────────┐  │                     │
│                    │  │  Selector        │  │  ← pick agent(s)    │
│                    │  └──────────────────┘  │                     │
│                    │  ┌──────────────────┐  │                     │
│                    │  │ Request Envelope │  │  ← normalize prompt │
│                    │  └──────────────────┘  │                     │
│                    │  ┌──────────────────┐  │                     │
│                    │  │  Dispatcher      │  │  ← spawn + wait     │
│                    │  └──────────────────┘  │                     │
│                    │  ┌──────────────────┐  │                     │
│                    │  │ Reliability Layer│  │  ← retry, timeout   │
│                    │  └──────────────────┘  │                     │
│                    │  ┌──────────────────┐  │                     │
│                    │  │  Aggregator      │  │  ← merge results    │
│                    │  └──────────────────┘  │                     │
│                    │  ┌──────────────────┐  │                     │
│                    │  │  CostTracker     │  │  ← budget guard     │
│                    │  └──────────────────┘  │                     │
│                    │  ┌──────────────────┐  │                     │
│                    │  │ Error Classifier │  │  ← agent-specific   │
│                    │  └──────────────────┘  │                     │
│                    └────────────┬───────────┘                     │
│                                 │                                   │
│                    ┌────────────▼────────────┐                     │
│                    │   MCP Config Adapter    │                     │
│                    └────────────┬────────────┘                     │
│                                 │                                   │
│         ┌─────────┬──────────┬──┴──┬──────────┬──────────┐        │
│         ▼         ▼          ▼     ▼          ▼          ▼        │
│    ┌────────┐ ┌──────┐ ┌────────┐ ┌────┐ ┌──────────┐ ┌──────┐   │
│    │ claude │ │ codex│ │  grok  │ │kimi│ │ custom  │              │
│    └───┬────┘ └──┬───┘ └───┬────┘ └─┬──┘ └────┬─────┘              │
│        └─────────┴─────────┴────────┴─────────┘                    │
│                              │                                      │
│                    ┌─────────▼──────────┐                          │
│                    │   Agent Results    │  ← unified JSON schema   │
│                    └────────────────────┘                          │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 4. Agent Registry

### 4.1 Static Configuration

Each agent is described by a static `AgentDef`. No dynamic discovery in Phase 1.

```rust
pub struct AgentDef {
    pub name: &'static str,           // "claude", "codex", ...
    pub display_name: &'static str,   // "Claude Code"
    pub model_family: &'static str,   // "anthropic", "openai", "google", "alibaba", "github"
    pub version: &'static str,        // "2.1.150"
    pub capabilities: AgentCaps,
    pub constraints: AgentConstraints,
    pub spawn: SpawnDef,
    pub mcp_format: McpFormat,
}

pub struct AgentCaps {
    pub design: u8,        // 0-10, design/UX capability
    pub code: u8,          // 0-10, coding capability
    pub long_context: u8,  // 0-10, long context handling
    pub chinese: u8,       // 0-10, Chinese language capability
    pub reasoning: u8,     // 0-10, complex reasoning
    pub speed: u8,         // 0-10, response speed
}

pub struct AgentConstraints {
    pub max_context_tokens: usize,    // e.g. 200_000
    pub cost_tier: CostTier,          // cheap / standard / premium
    pub supports_json: bool,          // structured output
    pub supports_mcp: bool,           // can receive MCP config
    pub supports_sandbox: bool,       // has sandbox/isolation
}

pub enum CostTier {
    Cheap,      // reserved for future local/small workers
    Standard,   // kimi, grok
    Premium,    // claude, codex
}

pub struct SpawnDef {
    pub binary: &'static str,         // "claude"
    pub mode: SpawnMode,
    pub default_timeout_secs: u64,    // per-agent timeout
    pub env_inject: Vec<(&'static str, &'static str)>,  // env vars to set
}

pub enum SpawnMode {
    ClaudePrompt {
        output_format_flag: &'static str,  // "--output-format"
        json_format: &'static str,         // "json"
        permissions_flag: &'static str,    // "--dangerously-skip-permissions"
        mcp_config_flag: &'static str,     // "--mcp-config"
    },
    CodexExec {
        json_flag: &'static str,           // "--json"
        sandbox_flag: Option<&'static str>, // "--sandbox"
        // MCP via ~/.codex/config.toml (not CLI arg)
    },
    GrokExec {
        output_format_flag: &'static str,
        json_format: &'static str,
        // MCP TBD; prompt envelope carries Tachi context when MCP is unavailable
    },
    KimiExec {
        output_format_flag: &'static str,
        json_format: &'static str,
        // MCP TBD; preferred for Chinese and long-context tasks
    },
    Custom {
        args_template: Vec<String>,  // template with placeholders
    },
}

pub enum McpFormat {
    JsonFile,           // --mcp-config /path/to/file.json
    JsonString,         // --additional-mcp-config '{"mcpServers":...}'
    TomlFile,           // write to ~/.codex/config.toml
    EnvVar,             // TACHI_MCP_CONFIG=...
    Unsupported,        // no MCP injection possible
}
```

### 4.2 Default Registry

```rust
pub static AGENT_REGISTRY: LazyLock<Vec<AgentDef>> = LazyLock::new(|| vec![
    AgentDef {
        name: "claude",
        display_name: "Claude Code",
        model_family: "anthropic",
        version: "2.1.150",
        capabilities: AgentCaps { design: 10, code: 10, long_context: 8, chinese: 7, reasoning: 10, speed: 7 },
        constraints: AgentConstraints { max_context_tokens: 200_000, cost_tier: CostTier::Premium, supports_json: true, supports_mcp: true, supports_sandbox: false },
        spawn: SpawnDef { binary: "claude", mode: SpawnMode::ClaudePrompt { ... }, default_timeout_secs: 300, env_inject: vec![] },
        mcp_format: McpFormat::JsonFile,
    },
    AgentDef {
        name: "codex",
        display_name: "Codex CLI",
        model_family: "openai",
        version: "0.135.0",
        capabilities: AgentCaps { design: 7, code: 10, long_context: 8, chinese: 6, reasoning: 9, speed: 8 },
        constraints: AgentConstraints { max_context_tokens: 200_000, cost_tier: CostTier::Premium, supports_json: true, supports_mcp: false, supports_sandbox: true },
        spawn: SpawnDef { binary: "codex", mode: SpawnMode::CodexExec { ... }, default_timeout_secs: 300, env_inject: vec![] },
        mcp_format: McpFormat::TomlFile,
    },
    AgentDef {
        name: "grok",
        display_name: "Grok Build",
        model_family: "xai",
        version: "current",
        capabilities: AgentCaps { design: 8, code: 9, long_context: 8, chinese: 7, reasoning: 9, speed: 8 },
        constraints: AgentConstraints { max_context_tokens: 200_000, cost_tier: CostTier::Premium, supports_json: true, supports_mcp: true, supports_sandbox: false },
        spawn: SpawnDef { binary: "grok", mode: SpawnMode::ClaudePrompt { ... }, default_timeout_secs: 300, env_inject: vec![] },
        mcp_format: McpFormat::JsonFile,
    },
    AgentDef {
        name: "kimi",
        display_name: "Kimi Code",
        model_family: "moonshot",
        version: "current",
        capabilities: AgentCaps { design: 7, code: 8, long_context: 10, chinese: 10, reasoning: 8, speed: 8 },
        constraints: AgentConstraints { max_context_tokens: 200_000, cost_tier: CostTier::Standard, supports_json: true, supports_mcp: false, supports_sandbox: false },
        spawn: SpawnDef { binary: "kimi", mode: SpawnMode::Custom { ... }, default_timeout_secs: 300, env_inject: vec![] },
        mcp_format: McpFormat::Unsupported,
    },
]);
```

### 4.3 Runtime Capabilities Check

Before dispatch, verify the agent binary exists and the requested capabilities are available:

```rust
pub fn is_agent_available(name: &str) -> bool {
    find_agent(name).map_or(false, |agent| {
        Command::new(agent.spawn.binary)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}
```

### 4.4 Per-Agent CLI Parameter Mapping

Based on the Phase 1 active agent fleet:

| Parameter | Claude `-p` | Codex `exec` | Grok `exec` | Kimi `exec` | Custom |
|-----------|------------|-------------|-------------|-------------|--------|
| **Prompt** | positional / `-p` | positional / stdin | positional / stdin | positional / stdin | template placeholder |
| **JSON output** | `--output-format json` | `--json` (JSONL) | JSONL / stdout capture | JSONL / stdout capture | adapter-defined |
| **System prompt** | `--system-prompt` / `--append-system-prompt` | inject in prompt | inject in prompt | inject in prompt | adapter-defined |
| **Model select** | `--model` | `-m, --model` | adapter-defined | adapter-defined | adapter-defined |
| **MCP config** | `--mcp-config <file>` | `~/.codex/config.toml` side-effect | prompt envelope fallback | prompt envelope fallback | unsupported unless adapter owns it |
| **Sandbox** | n/a | `-s, --sandbox <mode>` | adapter-defined | adapter-defined | adapter-defined |
| **Budget/time** | `--max-budget-usd` | n/a | adapter-defined | adapter-defined | adapter-defined |
| **JSON Schema** | `--json-schema` | `--output-schema <file>` | ❌ | `--json-schema` | ❌ | ❌ |
| **Agent preset** | `--agent` / `--agents <json>` | ❌ (TOML only, no CLI) | ❌ | ❌ | `--agent <agent>` | ❌ |
| **Approval** | `--permission-mode` | `--dangerously-bypass-approvals-and-sandbox` | `--approval-mode` | `--approval-mode` | `--allow-all` / `--yolo` | `--auto <level>` / `--skip-permissions-unsafe` |
| **Multi-agent** | `--brief` (agent comms) | ❌ | `--acp` | `--acp` | `--autopilot` | `--mission` |
| **Work dir** | `--add-dir` | `-C, --cd` / `--add-dir` | `--include-directories` | `--include-directories` | `--add-dir` | `--cwd` |
| **Bare/minimal** | `--bare` | `--ignore-user-config` | ❌ | `--bare` | ❌ | ❌ |
| **Tools filter** | `--allowedTools` / `--disallowedTools` | ❌ | `--allowed-tools` | `--allowed-tools` / `--exclude-tools` | `--allow-tool` / `--deny-tool` | `--enabled-tools` / `--disabled-tools` |

---

## 5. Dispatch Interface

### 5.1 Tool Schema (`tachi_dispatch`)

Extend the existing `tachi_dispatch` tool parameters:

```json
{
  "name": "tachi_dispatch",
  "description": "Dispatch a task to one or more CLI agents",
  "inputSchema": {
    "type": "object",
    "properties": {
      "agent": {
        "type": "string",
        "description": "Agent name or 'auto' for automatic selection",
        "enum": ["claude", "codex", "grok", "kimi", "custom", "auto"]
      },
      "prompt": {
        "type": "string",
        "description": "The prompt to send to the agent"
      },
      "strategy": {
        "type": "string",
        "description": "Execution strategy",
        "enum": ["first", "consensus", "best_of_n", "fallback", "parallel_all"],
        "default": "first"
      },
      "agents": {
        "type": "array",
        "description": "Explicit list of agents for consensus/parallel strategies",
        "items": { "type": "string" }
      },
      "task_type": {
        "type": "string",
        "description": "Hint for automatic agent selection",
        "enum": ["design", "code", "review", "test", "doc", "debug", "plan", "chinese", "long_context"],
        "default": "code"
      },
      "context": {
        "type": "string",
        "description": "Additional context files or directory to include"
      },
      "timeout_secs": {
        "type": "number",
        "description": "Override default timeout"
      },
      "budget_hint": {
        "type": "string",
        "description": "Cost preference",
        "enum": ["cheap", "balanced", "best"],
        "default": "balanced"
      },
      "orchestrator": {
        "type": "string",
        "description": "Agent for orchestrator role (planning, user-facing). Defaults to premium agent.",
        "default": "claude"
      },
      "worker": {
        "type": "string",
        "description": "Agent for worker role (sub-tasks, parallel execution). Defaults to cheapest capable agent.",
        "default": "auto"
      }
    },
    "required": ["agent", "prompt"]
  }
}
```

### 5.2 Agent Selection Logic

When `agent="auto"`, the Selector picks agents based on `task_type` and `budget_hint`.

**Hardcoded fallback map** (used when classifier is unavailable or disabled):

```rust
pub fn select_agents_fallback(task_type: &str, budget: BudgetHint, strategy: Strategy) -> Vec<&'static str> {
    let candidates = match task_type {
        "design"       => vec!["claude", "grok"],
        "code"         => vec!["claude", "codex", "grok"],
        "review"       => vec!["claude", "kimi"],
        "test"         => vec!["codex", "claude"],
        "doc"          => vec!["claude", "kimi"],
        "debug"        => vec!["claude", "codex", "grok"],
        "plan"         => vec!["claude", "grok"],
        "chinese"      => vec!["kimi", "claude"],
        "long_context" => vec!["kimi", "claude", "codex"],
        _              => vec!["claude"],
    };

    // Filter by budget
    let filtered = candidates.into_iter()
        .filter(|name| {
            let agent = find_agent(name).unwrap();
            match budget {
                BudgetHint::Cheap => agent.constraints.cost_tier == CostTier::Cheap,
                BudgetHint::Balanced => agent.constraints.cost_tier != CostTier::Premium,
                BudgetHint::Best => true,
            }
        })
        .collect::<Vec<_>>();

    // Strategy determines how many agents to pick
    match strategy {
        Strategy::First | Strategy::Fallback => vec![filtered.first().copied().unwrap_or("claude")],
        Strategy::Consensus => filtered.into_iter().take(3).collect(),
        Strategy::BestOfN => filtered.into_iter().take(3).collect(),
        Strategy::ParallelAll => filtered,
    }
}
```

### 5.3 Classifier Router (Auto Router)

Inspired by UltraCode-Shim's Auto Router. Instead of hardcoded `task_type → agent` mapping, use a configured classifier lane from the active fleet to score each candidate agent 0–1 for the current task.

**Classifier prompt template:**

```
You are a task-router classifier. Given a task description and a list of
available agents with capability cards, score each agent 0-1 on how well
it would perform this task. Only output JSON.

Task: {task_description}

Agents:
- claude: design=10, code=10, long_context=8, chinese=7, reasoning=10, speed=7
- codex: design=7, code=10, long_context=8, chinese=6, reasoning=9, speed=8
- grok: design=8, code=9, long_context=8, chinese=7, reasoning=9, speed=8
- kimi: design=7, code=8, long_context=10, chinese=10, reasoning=8, speed=8

Output format: {"scores": {"<agent>": <0-1>, ...}}
```

**Selection logic:**

```rust
pub async fn select_agents_classifier(
    task: &str,
    candidates: &[&'static str],
    quality_bar: f64,      // default 0.7
    budget: BudgetHint,
) -> Vec<&'static str> {
    // 1. Check cache
    if let Some(cached) = CLASSIFIER_CACHE.get(task) {
        return cached;
    }

    // 2. Run configured classifier from the active fleet
    let prompt = build_classifier_prompt(task, candidates);
    let result = dispatch_single(classifier_agent, &prompt).await;
    
    // 3. Parse scores
    let scores: HashMap<String, f64> = parse_classifier_output(&result.content);
    
    // 4. Filter by quality bar, then sort by cost
    let mut viable: Vec<_> = candidates.iter()
        .filter(|name| scores.get(*name).unwrap_or(&0.0) >= &quality_bar)
        .map(|name| (*name, find_agent(name).unwrap().constraints.cost_tier))
        .collect();
    
    // 5. Apply budget filter
    viable.retain(|(_, tier)| match budget {
        BudgetHint::Cheap => *tier == CostTier::Cheap,
        BudgetHint::Balanced => *tier != CostTier::Premium,
        BudgetHint::Best => true,
    });
    
    // 6. Sort by cost ascending, pick cheapest
    viable.sort_by_key(|(_, tier)| *tier as u8);
    let selected = viable.into_iter().map(|(name, _)| name).collect::<Vec<_>>();
    
    // 7. Cache and return
    CLASSIFIER_CACHE.insert(task.to_string(), selected.clone());
    selected
}
```

**Properties:**
- Classifier never sees price → can't game toward expensive models
- Decisions cached per task → no repeated classification
- Safe degradation → any failure falls back to hardcoded map
- Tunable via `quality_bar` and `budget_hint`

---

## 6. Request Envelope

Inspired by UltraCode-Shim's "UltraCode envelope" — a standard wrapper injected before every prompt to normalize agent behavior regardless of backend.

### 6.1 Envelope Structure

```rust
pub struct RequestEnvelope {
    pub system_prompt: String,       // capability description + behavior rules
    pub mcp_tools_description: String, // what tools are available
    pub effort_level: EffortLevel,   // quick / standard / deep
    pub output_format: OutputFormat, // plain / json / markdown
}

pub enum EffortLevel {
    Quick,      // fast, good enough
    Standard,   // normal reasoning
    Deep,       // thorough, xhigh effort (UltraCode-style)
}

pub enum OutputFormat {
    Plain,
    Json,
    Markdown,
}
```

### 6.2 System Prompt Template

```
You are {agent_name} ({agent_display_name}), a coding assistant.
Model family: {model_family}. Context window: {max_context_tokens} tokens.

Your capabilities:
{capability_card}

You have access to the following MCP tools:
{mcp_tools_description}

Effort level: {effort_level}
Output format: {output_format}

Rules:
- Always use the specified output format.
- When calling tools, follow the exact JSON schema.
- If you need more context, use tachi_memory(action="briefing") or tachi_task(action="brief") tools. (tachi_briefing RETIRED)
- Do not hallucinate file contents — read them via tools.
```

### 6.3 Envelope Application

```rust
pub fn apply_envelope(prompt: &str, agent: &AgentDef, envelope: &RequestEnvelope) -> String {
    let capability_card = format_capabilities(&agent.capabilities);
    let system = ENVELOPE_TEMPLATE
        .replace("{agent_name}", agent.name)
        .replace("{agent_display_name}", agent.display_name)
        .replace("{model_family}", agent.model_family)
        .replace("{max_context_tokens}", &agent.constraints.max_context_tokens.to_string())
        .replace("{capability_card}", &capability_card)
        .replace("{mcp_tools_description}", &envelope.mcp_tools_description)
        .replace("{effort_level}", &format!("{:?}", envelope.effort_level))
        .replace("{output_format}", &format!("{:?}", envelope.output_format));
    
    format!("{system}\n\n---\n\n{prompt}")
}
```

**Rationale:** Different agents have different default behaviors. The envelope forces a consistent baseline — like UltraCode-Shim forcing `effort=xhigh` on every backend.

### 6.3 Codex Agent Envelope

Codex maintains **23 pre-defined agents** in `~/.codex/agents/*.toml`. Tachi can read these definitions and inject their `developer_instructions` into the prompt when dispatching to Codex `exec` mode (which has no `--agent` CLI flag).

#### 6.3.1 Codex Agent TOML Schema

```toml
# ~/.codex/agents/architect.toml
name = "architect"
description = "System design, boundaries, interfaces, long-horizon tradeoffs"
model = "gpt-5.4"
model_reasoning_effort = "high"
developer_instructions = """
<identity>
You are Architect (Oracle). Diagnose, analyze, and recommend with file-backed evidence.
</identity>
<constraints>
<scope_guard>
- Never write or edit files.
- Never judge code you have not opened.
- Acknowledge uncertainty instead of speculating.
</scope_guard>
</constraints>
<execution_loop>
1. Gather context first.
2. Form a hypothesis.
3. Cross-check it against the code.
4. Return summary, root cause, recommendations, and tradeoffs.
</execution_loop>
"""
```

#### 6.3.2 Mapping Codex Agents to Tachi Task Types

```rust
pub fn codex_agent_for_task(task_type: &str) -> Option<&'static str> {
    match task_type {
        "design"      => Some("architect"),
        "code"        => Some("executor"),
        "review"      => Some("code-reviewer"),
        "test"        => Some("test-engineer"),
        "doc"         => Some("writer"),
        "debug"       => Some("debugger"),
        "plan"        => Some("planner"),
        "security"    => Some("security-reviewer"),
        "dependency"  => Some("dependency-expert"),
        "git"         => Some("git-master"),
        "vision"      => Some("vision"),
        _             => None,
    }
}
```

#### 6.3.3 Envelope Application for Codex

Since `codex exec` has no `--agent` or `--system-prompt` CLI flag, Tachi constructs a pseudo-system-prompt by prepending the agent instructions:

```rust
pub fn apply_codex_agent_envelope(prompt: &str, agent_name: &str) -> String {
    let toml_path = format!("{}/.codex/agents/{}.toml", home_dir(), agent_name);
    let agent_def = parse_toml(&toml_path).ok();
    
    if let Some(agent) = agent_def {
        format!(
            "[AGENT: {}] {}\n\n<developer_instructions>\n{}\n</developer_instructions>\n\n---\n\n{}",
            agent.name,
            agent.description,
            agent.developer_instructions.trim(),
            prompt
        )
    } else {
        prompt.to_string()
    }
}
```

**Usage example:**
```bash
# Tachi internally reads ~/.codex/agents/architect.toml
# and constructs the full prompt before dispatching:
tachi dispatch codex "Design the auth module" --task-type design
# → codex exec --json "[AGENT: architect] System design...\n\n---\n\nDesign the auth module"
```

**Available Codex agents:**
| Agent | Role | Tachi Task Type |
|-------|------|----------------|
| `architect` | System design, boundaries, tradeoffs | `design` |
| `executor` | Code implementation, refactoring | `code` |
| `team-executor` | Supervised team execution | `code` (conservative) |
| `code-reviewer` | Code review | `review` |
| `security-reviewer` | Security audit | `security` |
| `test-engineer` | Write tests | `test` |
| `debugger` | Debug issues | `debug` |
| `planner` | Plan tasks | `plan` |
| `analyst` | Analyze code/data | `analyze` |
| `critic` | Critical review | `review` |
| `designer` | UI/UX design | `design` |
| `writer` | Documentation | `doc` |
| `researcher` | Research topics | `research` |
| `verifier` | Verify correctness | `verify` |
| `vision` | Image analysis | `vision` |
| `dependency-expert` | Dependency management | `dependency` |
| `git-master` | Git operations | `git` |
| `build-fixer` | Fix build issues | `debug` |
| `code-simplifier` | Simplify code | `refactor` |
| `explorer` | Explore codebase | `explore` |

### 6.4 Cross-Agent Envelope Ecosystem (Reference)

For a comprehensive analysis of how Superpowers, Waza, Codex, and Amp define agent envelopes — including XML tag taxonomy, prompt-model joint routing, subagent architectures, compaction/TODO/handoff comparison, and a cross-platform comparison matrix — see:

**📄 `docs/engineering/architecture/agent-envelope-ecosystem.md`**

That document covers:
- **Superpowers** 5-stage workflow envelope (brainstorm → plan → execute → review → ship)
- **Waza** 8 capability skill envelopes (think, check, hunt, design, write, learn, read, health)
- **Codex** 21 agent envelopes with full XML structure breakdown
- **Amp** 9-prompt routing system + 3 subagent types (Oracle / Task Tool / Codebase Search)
- Cross-agent compaction/TODO/handoff comparison (Claude Code / OpenCode / Amp / Codex CLI)
- Unified envelope normalization strategy for Tachi
- Complete reference source file paths for all four ecosystems

---

## 7. Orchestrator / Worker Split

Inspired by UltraCode-Shim's dual-slot design. Tachi distinguishes between:

- **Orchestrator** (主任务): Planning, architecture, user-facing decisions, complex reasoning
- **Worker** (子任务): Implementation, review, testing, data processing — tasks that can be parallelized

### 7.1 Role Definition

```rust
pub enum DispatchRole {
    Orchestrator,  // premium model, single-threaded, user-facing
    Worker,        // cheap/fast model, concurrent, background
}

pub struct RoleConfig {
    pub preferred_agent: &'static str,
    pub fallback_agents: Vec<&'static str>,
    pub max_concurrent: usize,
    pub effort: EffortLevel,
}
```

### 7.2 Default Role Mapping

```rust
pub fn default_role_config(role: DispatchRole) -> RoleConfig {
    match role {
        DispatchRole::Orchestrator => RoleConfig {
            preferred_agent: "claude",
            fallback_agents: vec!["codex", "grok"],
            max_concurrent: 1,
            effort: EffortLevel::Deep,
        },
        DispatchRole::Worker => RoleConfig {
            preferred_agent: "kimi",
            fallback_agents: vec!["grok", "codex"],
            max_concurrent: 5,  // fan-out limit
            effort: EffortLevel::Standard,
        },
    }
}
```

### 7.3 Task Classification

```rust
pub fn classify_task(prompt: &str) -> DispatchRole {
    // Simple heuristic + classifier hybrid
    let keywords_orchestrator = ["design", "architect", "plan", "strategy", "decide", "review architecture"];
    let keywords_worker = ["implement", "write tests", "refactor", "fix bug", "generate", "summarize"];
    
    let lower = prompt.to_lowercase();
    let orch_score = keywords_orchestrator.iter().filter(|k| lower.contains(*k)).count();
    let worker_score = keywords_worker.iter().filter(|k| lower.contains(*k)).count();
    
    if orch_score > worker_score {
        DispatchRole::Orchestrator
    } else {
        DispatchRole::Worker
    }
}
```

### 7.4 Usage Example

```rust
// User dispatches a complex task
let result = dispatch_orchestrator("claude", "Design a microservice architecture for...").await;

// Orchestrator breaks it into sub-tasks, dispatches workers in parallel
let workers = vec![
    dispatch_worker("kimi", "Implement the auth service"),
    dispatch_worker("grok", "Implement the API gateway"),
    dispatch_worker("codex", "Write integration tests"),
];
let worker_results = futures::future::join_all(workers).await;

// Orchestrator aggregates and returns final answer
let final = aggregate_orchestrator_result(&result, &worker_results);
```

### 7.5 Cost Optimization

| Pattern | Orchestrator | Worker | Total Cost |
|---------|-------------|--------|------------|
| Single premium | Claude 4 × 1 | — | $5.0 |
| Split | Claude 4 × 0.2 (plan) | Kimi × 3 (implement) | $1.0 + worker cost |
| All standard | — | Kimi/Grok × 4 | worker cost only |
| Consensus | Claude 4 × 1 | Kimi + Codex (review) | $5.0 + worker cost |

### 7.4 Deferred Research: Droid Mission Mode (Built-in Multi-Agent)

**Critical discovery:** Droid `exec` has a `--mission` flag that enables **multi-agent orchestration in non-interactive mode**:

```bash
droid exec --mission --auto high "Build a full authentication system with OAuth2"
```

When `--mission` is active:
- Session auto-upgrades to **orchestrator mode**
- Spawns **worker sessions** via `factoryd` to implement features
- Uses **GPT-5.2 High** reasoning by default
- Auto-approves proposals (no interactive confirmation)
- Requires `--auto high` or `--skip-permissions-unsafe`

#### Mission Mode Parameters

```rust
pub struct DroidMissionConfig {
    pub mission: bool,                        // --mission
    pub worker_model: Option<String>,         // --worker-model
    pub worker_reasoning_effort: Option<String>, // --worker-reasoning-effort
    pub validator_model: Option<String>,      // --validator-model
    pub validator_reasoning_effort: Option<String>, // --validator-reasoning-effort
}
```

#### Tachi Integration Strategy

Tachi can **wrap Droid Mission Mode** as a special dispatch path:

```rust
pub async fn dispatch_droid_mission(
    prompt: &str,
    config: &DroidMissionConfig,
) -> Result<AgentResult, DispatchError> {
    let mut args = vec!["exec", "--mission"];
    
    if let Some(ref model) = config.worker_model {
        args.extend(["--worker-model", model]);
    }
    if let Some(ref validator) = config.validator_model {
        args.extend(["--validator-model", validator]);
    }
    // ...
    
    // Droid Mission spawns its own workers internally;
    // Tachi waits for the orchestrator to return final result
    spawn_and_wait("droid", &args, prompt).await
}
```

**Design decision:** Droid Mission is the **only CLI agent with built-in non-interactive multi-agent**. Tachi can either:
1. Treat it as a black-box Worker (dispatch mission, wait for result)
2. Treat it as a special Orchestrator (Droid manages workers, Tachi manages Droid)

Option 1 is simpler and aligns with "all CLI agents are Workers" philosophy.

### 7.6 Qwen Secretary (Auxiliary Role)

**New role alongside Orchestrator/Worker:** The **Secretary** is not a task executor — it is a lightweight background helper that handles administrative, formatting, and extraction work.

| Responsibility | Secretary (Qwen 32B local ollama) | Orchestrator (Claude) | Worker (Any agent) |
|----------------|-----------------------------------|----------------------|-------------------|
| Task planning | ❌ | ✅ | ❌ |
| Code writing | ❌ | ✅ | ✅ |
| Reference extraction from text | ✅ | — | — |
| Wiki formatting / metadata | ✅ | — | — |
| Eval completeness check | ✅ | — | — |
| Issue label suggestion | ✅ (advisory) | ❌ | ❌ |
| Cost estimation | ✅ (fast heuristic) | ❌ | ❌ |

**Design principle:** The Secretary is **transactional, not decisional**. It never decides which agent to dispatch, never approves a PR, never designs architecture. It automates the busywork so humans and premium models focus on high-value decisions.

```rust
pub enum DispatchRole {
    Orchestrator,  // premium model, single-threaded, user-facing
    Worker,        // cheap/fast model, concurrent, background
    Secretary,     // local small model, high-frequency, administrative
}

pub fn default_role_config(role: DispatchRole) -> RoleConfig {
    match role {
        // ... existing Orchestrator/Worker ...
        DispatchRole::Secretary => RoleConfig {
            preferred_agent: "qwen-local",  // ollama qwen2.5:32b
            fallback_agents: vec![],
            max_concurrent: 10,  // high throughput, local inference
            effort: EffortLevel::Fast,
        },
    }
}
```

**Secretary integration points:**
- `tachi_complete` (RETIRED) → auto-check eval completeness (missing cost? missing diff?)  (use tachi_task(action="complete"))
- `tachi_wiki_write` → auto-extract `references` from content if empty
- `tachi_dispatch` → pre-flight cost heuristic, post-flight format normalization
- Kanban → auto-suggest label/priority from card title/body

See `docs/engineering/architecture/wiki-references-spec.md` §10 for the `qwen_secretary::ref_extractor` design.

---

## 8. MCP Config Adapter

### 8.1 Adapter Interface

```rust
pub trait McpAdapter {
    /// Generate the MCP configuration for a given agent
    fn generate(&self, agent: &AgentDef, tachi_endpoint: &str) -> Result<McpConfigPayload, McpError>;
    
    /// Return the CLI arguments to inject the MCP config
    fn cli_args(&self, payload: &McpConfigPayload) -> Result<Vec<String>, McpError>;
    
    /// Any side effects needed before spawn (e.g. write to ~/.codex/config.toml)
    fn prepare(&self, payload: &McpConfigPayload) -> Result<(), McpError>;
    
    /// Cleanup after spawn (e.g. restore ~/.codex/config.toml)
    fn cleanup(&self, payload: &McpConfigPayload) -> Result<(), McpError>;
}
```

### 8.2 Per-Agent Implementations

#### Claude — JSON File
```rust
fn generate_claude_mcp(tachi_endpoint: &str) -> McpConfigPayload {
    let json = json!({
        "mcpServers": {
            "tachi": {
                "command": "tachi",
                "args": ["mcp-server"],
                "env": { "TACHI_ENDPOINT": tachi_endpoint }
            }
        }
    });
    let path = temp_dir().join(format!("tachi-mcp-{}.json", uuid()));
    fs::write(&path, json.to_string()).unwrap();
    McpConfigPayload::JsonFile(path)
}

fn claude_cli_args(payload: &McpConfigPayload) -> Vec<String> {
    match payload {
        McpConfigPayload::JsonFile(path) => vec![
            "--mcp-config".to_string(),
            path.to_string_lossy().to_string(),
        ],
        _ => panic!("invalid payload for claude"),
    }
}
```

#### Codex — TOML File (side-effect)
```rust
fn generate_codex_mcp(tachi_endpoint: &str) -> McpConfigPayload {
    let toml = format!(r#"
[mcpServers.tachi]
command = "tachi"
args = ["mcp-server"]

[env]
TACHI_ENDPOINT = "{tachi_endpoint}"
"#);
    McpConfigPayload::TomlFile {
        path: home_dir().join(".codex/config.toml"),
        content: toml,
        backup: None,  // filled by prepare()
    }
}

fn codex_prepare(payload: &mut McpConfigPayload) -> Result<(), McpError> {
    match payload {
        McpConfigPayload::TomlFile { path, content, backup } => {
            if path.exists() {
                *backup = Some(fs::read_to_string(path)?);
            }
            fs::write(path, content)?;
            Ok(())
        }
        _ => panic!("invalid payload for codex"),
    }
}

fn codex_cleanup(payload: &McpConfigPayload) -> Result<(), McpError> {
    match payload {
        McpConfigPayload::TomlFile { path, backup, .. } => {
            if let Some(ref original) = backup {
                fs::write(path, original)?;
            } else {
                fs::remove_file(path)?;
            }
            Ok(())
        }
        _ => panic!("invalid payload for codex"),
    }
}
```

#### Deferred Research: Copilot — JSON String Inline
```rust
fn generate_copilot_mcp(tachi_endpoint: &str) -> McpConfigPayload {
    let json = json!({"mcpServers": {"tachi": {"command": "tachi", "args": ["mcp-server"]}}});
    McpConfigPayload::JsonString(json.to_string())
}

fn copilot_cli_args(payload: &McpConfigPayload) -> Vec<String> {
    match payload {
        McpConfigPayload::JsonString(s) => vec![
            "--additional-mcp-config".to_string(),
            s.clone(),
        ],
        _ => panic!("invalid payload for copilot"),
    }
}
```

#### Gemini / Qwen / Droid — TBD
```rust
// These agents' MCP support is not yet confirmed.
// For now, they dispatch without MCP callback.
// TODO: investigate each agent's MCP or tool-call capabilities.
```

---

## 9. Result Aggregation

### 9.1 Unified Result Schema

All agents return a normalized `AgentResult`:

```rust
pub struct AgentResult {
    pub agent: &'static str,
    pub success: bool,
    pub content: String,           // stdout or parsed JSON
    pub metadata: ResultMetadata,
    pub error: Option<String>,     // stderr or error message
}

pub struct ResultMetadata {
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub tokens_used: Option<u64>,  // if agent reports it
    pub cost_usd: Option<f64>,     // estimated cost
}
```

### 9.2 Aggregation Strategies

```rust
pub enum AggregationStrategy {
    /// Return the first successful result (fastest agent wins)
    First,
    
    /// Run all agents, return all results (caller decides)
    ParallelAll,
    
    /// Run N agents, find the answer that appears most frequently
    Consensus { min_agents: usize },
    
    /// Run N agents, use a judge agent to pick the best
    BestOfN { judge: &'static str },
    
    /// Try agents in order until one succeeds
    FallbackChain,
}
```

#### Consensus Implementation
```rust
pub fn consensus(results: Vec<AgentResult>) -> AggregatedResult {
    // Simple string similarity: normalize whitespace and compare
    let mut groups: HashMap<String, Vec<&AgentResult>> = HashMap::new();
    
    for r in &results {
        if !r.success { continue; }
        let key = normalize_answer(&r.content);
        groups.entry(key).or_default().push(r);
    }
    
    // Find the largest group
    let (best_answer, group) = groups.into_iter()
        .max_by_key(|(_, g)| g.len())
        .unwrap_or_default();
    
    AggregatedResult {
        answer: best_answer,
        confidence: group.len() as f64 / results.len() as f64,
        sources: group.iter().map(|r| r.agent).collect(),
        dissent: results.iter().filter(|r| r.success && !group.contains(r))
            .map(|r| (r.agent, r.content.clone())).collect(),
    }
}
```

#### BestOfN Implementation
```rust
pub async fn best_of_n(
    results: Vec<AgentResult>,
    judge: &str,
    prompt: &str,
) -> AggregatedResult {
    let judge_prompt = format!(
        "You are judging {} answers to the following task. Pick the best one.\n\nTask: {}\n\n{}",
        results.len(),
        prompt,
        results.iter().enumerate()
            .map(|(i, r)| format!("Answer {} (from {}):\n{}\n", i + 1, r.agent, r.content))
            .collect::<String>()
    );
    
    let judge_result = dispatch_single(judge, &judge_prompt).await;
    // Parse judge's pick and return corresponding result
    // ...
}
```

---

## 10. Reliability Wrappers

Inspired by UltraCode-Shim's production hardening. Three failure modes handled transparently:

### 10.1 Empty Turn Auto-Retry

Some backends return a turn with no text and no tool call (transient blip, or budget-exhausted reasoning turn). The wrapper transparently re-issues the request.

```rust
pub struct EmptyTurnRetry {
    pub max_retries: usize,      // default 2
    pub buffer_until_token: bool, // true: only retry if zero output received
}

pub async fn dispatch_with_empty_turn_retry(
    agent: &str,
    prompt: &str,
    config: &EmptyTurnRetry,
) -> Result<AgentResult, DispatchError> {
    for attempt in 0..=config.max_retries {
        let result = dispatch_single(agent, prompt).await?;
        
        if result.content.trim().is_empty() && result.success {
            // Empty but successful → retry
            if attempt < config.max_retries {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        }
        
        return Ok(result);
    }
    
    Err(DispatchError::EmptyTurnExhausted { agent })
}
```

### 10.2 Idle Timeout (Stream Stall Protection)

If a backend stream opens and then goes silent mid-turn, a bounded idle timeout turns the stall into a quick retry instead of a ~10-minute hang.

```rust
pub struct IdleTimeoutConfig {
    pub idle_timeout_secs: u64,   // default 60
    pub max_retries: usize,       // default 1
}

pub async fn dispatch_with_idle_timeout(
    agent: &str,
    prompt: &str,
    config: &IdleTimeoutConfig,
) -> Result<AgentResult, DispatchError> {
    let mut child = spawn_agent(agent, prompt)?;
    
    let mut last_output = Instant::now();
    let mut output_buffer = String::new();
    
    loop {
        match tokio::time::timeout(
            Duration::from_secs(config.idle_timeout_secs),
            read_output_chunk(&mut child),
        ).await {
            Ok(Some(chunk)) => {
                output_buffer.push_str(&chunk);
                last_output = Instant::now();
            }
            Ok(None) => {
                // Process finished
                return Ok(AgentResult {
                    agent,
                    success: child.wait().await?.success(),
                    content: output_buffer,
                    metadata: ResultMetadata { /* ... */ },
                    error: None,
                });
            }
            Err(_) => {
                // Idle timeout → kill and retry
                child.kill().await.ok();
                return Err(DispatchError::IdleTimeout { agent, duration: config.idle_timeout_secs });
            }
        }
    }
}
```

### 10.3 Tool Call Rejection Repair

Some strict backends (like DeepSeek) 400 when a tool call is declined mid-run. The wrapper repairs the tool-call sequence and synthesizes a stub reply.

```rust
pub struct ToolRejectionRepair {
    pub enabled: bool,
    pub stub_reply_template: String,  // "User declined to run {tool_name}"
}

pub fn repair_tool_sequence(
    raw_output: &str,
    rejected_tools: &[&str],
    config: &ToolRejectionRepair,
) -> String {
    // Parse tool-call sequence
    // For each rejected tool, inject stub reply
    // Re-serialize and return
    // ...
}
```

### 10.4 Combined Reliability Stack

```rust
pub async fn reliable_dispatch(
    agent: &str,
    prompt: &str,
    reliability: &ReliabilityConfig,
) -> Result<AgentResult, DispatchError> {
    let result = dispatch_with_empty_turn_retry(agent, prompt, &reliability.empty_turn).await?;
    let result = dispatch_with_idle_timeout(agent, prompt, &reliability.idle_timeout).await?;
    // Tool rejection repair applied post-hoc on result parsing
    Ok(result)
}
```

---

## 11. Error Classifier

Different agents have different error patterns. A centralized error classifier routes each failure to the appropriate recovery strategy.

### 11.1 Error Taxonomy

```rust
pub enum AgentErrorKind {
    // Transient — retry
    RateLimit { retry_after: Option<Duration> },
    Timeout,
    StreamStall,
    EmptyResponse,
    
    // Agent-specific — switch agent
    UnsupportedTool { tool_name: String },
    ContextExceeded { requested: usize, limit: usize },
    JsonParseError,
    
    // Fatal — propagate
    AuthFailure,
    BinaryNotFound,
    UnknownExitCode(i32),
}
```

### 11.2 Agent-Specific Error Patterns

```rust
pub fn classify_error(agent: &str, stdout: &str, stderr: &str, exit_code: Option<i32>) -> AgentErrorKind {
    let combined = format!("{} {}", stdout, stderr);
    
    match agent {
        "claude" => {
            if combined.contains("rate limit") {
                AgentErrorKind::RateLimit { retry_after: parse_retry_after(&combined) }
            } else if combined.contains("context length exceeded") {
                AgentErrorKind::ContextExceeded { requested: 0, limit: 200_000 }
            } else {
                AgentErrorKind::UnknownExitCode(exit_code.unwrap_or(-1))
            }
        }
        "codex" => {
            if combined.contains("sandbox") {
                AgentErrorKind::UnsupportedTool { tool_name: "sandbox".to_string() }
            } else if combined.contains("timeout") {
                AgentErrorKind::Timeout
            } else {
                AgentErrorKind::UnknownExitCode(exit_code.unwrap_or(-1))
            }
        }
        "grok" => {
            if combined.contains("quota") {
                AgentErrorKind::RateLimit { retry_after: None }
            } else {
                AgentErrorKind::UnknownExitCode(exit_code.unwrap_or(-1))
            }
        }
        // ... etc
        _ => AgentErrorKind::UnknownExitCode(exit_code.unwrap_or(-1)),
    }
}
```

### 11.3 Recovery Strategy

```rust
pub fn recovery_strategy(error: &AgentErrorKind, agent: &AgentDef) -> RecoveryAction {
    match error {
        AgentErrorKind::RateLimit { retry_after } => {
            RecoveryAction::RetryAfter(retry_after.unwrap_or(Duration::from_secs(60)))
        }
        AgentErrorKind::Timeout | AgentErrorKind::StreamStall | AgentErrorKind::EmptyResponse => {
            RecoveryAction::RetryNow
        }
        AgentErrorKind::ContextExceeded { .. } => {
            RecoveryAction::SwitchAgent("kimi")  // long context fallback
        }
        AgentErrorKind::UnsupportedTool { .. } => {
            RecoveryAction::SwitchAgent("claude")  // most capable fallback
        }
        AgentErrorKind::AuthFailure | AgentErrorKind::BinaryNotFound => {
            RecoveryAction::FailPermanent
        }
        _ => RecoveryAction::RetryNow,
    }
}
```

---

## 12. Cost & Timeout Management

### 12.1 Per-Request Budget

```rust
pub struct DispatchBudget {
    pub max_cost_usd: Option<f64>,
    pub max_duration_secs: Option<u64>,
    pub max_agents: usize,
}

impl Default for DispatchBudget {
    fn default() -> Self {
        Self {
            max_cost_usd: None,
            max_duration_secs: Some(300),
            max_agents: 3,
        }
    }
}
```

### 12.2 Cost Estimation (Pre-flight)

```rust
pub fn estimate_cost(agents: &[&str], prompt_tokens: usize) -> f64 {
    agents.iter().map(|name| {
        let agent = find_agent(name).unwrap();
        let rate = match agent.constraints.cost_tier {
            CostTier::Cheap => 0.0005,     // $0.50 / 1M tokens
            CostTier::Standard => 0.002,   // $2 / 1M tokens
            CostTier::Premium => 0.015,    // $15 / 1M tokens
        };
        prompt_tokens as f64 * rate
    }).sum()
}
```

### 12.3 Timeout with Cancellation

Use `tokio::time::timeout` with graceful cancellation:

```rust
pub async fn dispatch_with_timeout(
    agent: &str,
    prompt: &str,
    timeout_secs: u64,
) -> Result<AgentResult, DispatchError> {
    let child = spawn_agent(agent, prompt)?;
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        wait_for_agent(child),
    ).await;
    
    match result {
        Ok(r) => r,
        Err(_) => {
            // Kill the child process
            child.kill().await.ok();
            Err(DispatchError::Timeout { agent, timeout_secs })
        }
    }
}
```

---

## 13. CLI Integration

### 13.1 New `tachi dispatch` subcommand

```bash
# Dispatch to a specific agent
tachi dispatch claude "Refactor this function to use iterators"

# Auto-select agent based on task type (with classifier)
tachi dispatch auto "Design a logo for my startup" --task-type design --classifier

# Consensus strategy with 3 agents
tachi dispatch auto "Review this PR" --strategy consensus --agents claude,kimi,codex

# Fallback chain
tachi dispatch auto "Fix this bug" --strategy fallback --agents claude,codex,grok

# Budget-conscious
tachi dispatch auto "Summarize this document" --budget cheap

# Parallel all
tachi dispatch auto "Generate test cases" --strategy parallel-all

# Orchestrator + Worker split
tachi dispatch auto "Build a full-stack app" \
  --orchestrator claude \
  --worker kimi \
  --strategy orchestrator-worker
```

### 13.2 Configuration File

`~/.tachi/agent-router.toml`:

```toml
[router]
default_strategy = "first"
default_budget = "balanced"
max_concurrent_agents = 3
classifier_enabled = true
classifier_agent = "claude"
classifier_quality_bar = 0.7

[orchestrator]
preferred = "claude"
fallback = ["codex", "grok"]
max_concurrent = 1
effort = "deep"

[worker]
preferred = "kimi"
fallback = ["grok", "codex"]
max_concurrent = 5
effort = "standard"

[agents.claude]
enabled = true
weight = 1.0
custom_timeout_secs = 600

[agents.codex]
enabled = true
weight = 1.0

[agents.grok]
enabled = true
weight = 0.8

[agents.kimi]
enabled = true
weight = 0.9

[aggregation.consensus]
min_agents = 2
similarity_threshold = 0.85

[aggregation.best_of_n]
judge = "claude"

[reliability]
empty_turn_max_retries = 2
idle_timeout_secs = 60
tool_rejection_repair = true
```

---

## 14. Implementation Phases

### Phase 1: Agent Registry + Extended Dispatch (2-3 days)

**Goal:** Support the Phase 1 CLI fleet with `tachi_dispatch`.

- [ ] Define `AgentDef`, `AgentRegistry`, `SpawnMode`, `McpFormat` structs
- [ ] Implement `find_agent()`, `is_agent_available()`
- [ ] Refactor `build_claude_command()` → `build_command(agent: &AgentDef, ...)`
- [ ] Add `build_codex_command()`, `build_grok_command()`, `build_kimi_command()`
- [ ] Add MCP adapters for Claude and Grok (JSON file)
- [ ] Add Codex MCP adapter (TOML file with backup/restore)
- [ ] Update `TachiDispatchParams` JSON schema
- [ ] Extend `tachi_dispatch` to accept `"grok"`, `"kimi"`
- [ ] Add per-agent timeout support
- [ ] Add `tachi dispatch` CLI subcommand
- [ ] Add `RequestEnvelope` with basic system prompt template

**Acceptance:**
```bash
tachi dispatch grok "Hello world"  # works
tachi dispatch kimi "你好"         # works
```

### Phase 2: Auto-Select + Concurrent Execution + Classifier (2-3 days)

**Goal:** `agent="auto"`, classifier router, concurrent dispatch, basic strategies.

- [ ] Implement `select_agents_fallback()` with hardcoded task_type map
- [ ] Implement Classifier Router (active fleet scoring)
- [ ] Add classifier cache (LRU, per-task)
- [ ] Implement `agent="auto"` in `tachi_dispatch`
- [ ] Implement `Strategy::First` (fastest wins via `tokio::select!`)
- [ ] Implement `Strategy::ParallelAll` (return all results)
- [ ] Implement `Strategy::FallbackChain` (try in order)
- [ ] Add unified `AgentResult` schema
- [ ] Add `~/.tachi/agent-router.toml` config (including classifier settings)
- [ ] Add cost estimation (pre-flight)

**Acceptance:**
```bash
tachi dispatch auto "Design a React component" --task-type design --strategy first --classifier
# Returns first response among claude + grok, selected by the configured classifier
```

### Phase 3: Orchestrator/Worker + Aggregation + Reliability (3-5 days)

**Goal:** Orchestrator/worker split, consensus, BestOfN, reliability wrappers.

- [ ] Implement `DispatchRole::Orchestrator` and `DispatchRole::Worker`
- [ ] Implement task classification heuristic + classifier hybrid
- [ ] Implement `dispatch_orchestrator()` and `dispatch_worker()`
- [ ] Implement `Strategy::Consensus` with text similarity
- [ ] Implement `Strategy::BestOfN` with judge agent
- [ ] Implement `EmptyTurnRetry` wrapper
- [ ] Implement `IdleTimeout` wrapper (stream stall protection)
- [ ] Implement `ToolRejectionRepair` wrapper
- [ ] Add `CostTracker` module (per-request + cumulative)
- [ ] Add `DispatchBudget` enforcement
- [ ] Add `ErrorClassifier` with agent-specific patterns
- [ ] Add `RecoveryStrategy` with automatic agent switching

**Acceptance:**
```bash
tachi dispatch auto "Build a REST API" --strategy orchestrator-worker \
  --orchestrator claude --worker kimi
# Claude plans, Kimi implements, Claude reviews
```

### Phase 4: Production Hardening (2-3 days)

**Goal:** Observability, rate limiting, circuit breaker, integration tests.

- [ ] Add structured logging for all dispatches
- [ ] Add OpenTelemetry spans across agent boundaries
- [ ] Add per-agent rate limiting (token bucket)
- [ ] Add circuit breaker for failing agents
- [ ] Add retry with exponential backoff
- [ ] Add metrics: dispatch latency, cost per agent, success rate, classifier accuracy
- [ ] Add integration tests with mock agents
- [ ] Add `tachi_dispatch` streaming support (Phase 4.5)
- [ ] Design GUI agent API interface (stub only)

---

## 15. Multi-Model Backend Selection

### 15.1 Current Backend Map

Tachi's `LlmClient` (`crates/tachi-server/src/llm.rs`) already uses a **lane-based architecture** with multiple model tiers:

| Lane | Default Model | Backend | Usage |
|------|--------------|---------|-------|
| Extract | Qwen 27B | SiliconFlow API | Metadata extraction, fact extraction |
| Summary | Qwen 27B | SiliconFlow API | L0 summary |
| Distill | Qwen 27B | SiliconFlow API / Claude CLI | Foundry batch distill |
| Reasoning | Claude CLI → Qwen 27B | Claude CLI / SiliconFlow | Reasoning fallback |
| Kanban | qwen2.5:32b | Local ollama | Kanban classification |

### 15.2 Gap: Missing Mid-Tier

The current architecture has a **bimodal split**:
- **Cheap/Fast:** Qwen 27B (SiliconFlow) or local ollama — good for extraction, summary
- **Premium:** Claude CLI — good for reasoning, distill, complex synthesis

Missing: a **mid-tier model** for tasks that need more than Qwen 27B but less than Claude CLI:
- Wiki evolver synthesis
- Agent evolution proposals
- Complex contradiction detection
- Multi-document synthesis

### 15.3 Proposed: Three-Tier Backend

```rust
enum BackendModelTier {
    Fast,     // Qwen 27B / local ollama — high freq, low cost, administrative
    Balanced, // Qwen 72B / DeepSeek-V3 — mid complexity, synthesis, evolver
    Quality,  // Claude CLI / GPT-4 — critical reasoning, architecture, final review
}
```

**Per-lane env override:**
```bash
# Front-line lanes
TACHI_BACKEND_EXTRACT_TIER=fast      # Qwen 27B
TACHI_BACKEND_SUMMARY_TIER=fast      # Qwen 27B

# Foundry lanes
TACHI_BACKEND_DISTILL_TIER=balanced  # Qwen 72B or DeepSeek
TACHI_BACKEND_REASONING_TIER=quality # Claude CLI

# Secretary (local)
TACHI_BACKEND_SECRETARY_TIER=fast    # local ollama qwen2.5:32b
```

### 15.4 Selection Logic

```rust
pub fn select_backend_model(tier: BackendModelTier, lane: ChatLane) -> (String, String) {
    // (base_url, model_name)
    match tier {
        BackendModelTier::Fast => (
            std::env::var("TACHI_FAST_BASE_URL").unwrap_or_else(|_| "http://localhost:11434/v1".to_string()),
            std::env::var("TACHI_FAST_MODEL").unwrap_or_else(|_| "qwen2.5:32b".to_string()),
        ),
        BackendModelTier::Balanced => (
            std::env::var("TACHI_BALANCED_BASE_URL").unwrap_or_else(|_| "https://api.siliconflow.cn/v1".to_string()),
            std::env::var("TACHI_BALANCED_MODEL").unwrap_or_else(|_| "Qwen/Qwen3-72B".to_string()),
        ),
        BackendModelTier::Quality => (
            std::env::var("TACHI_QUALITY_BASE_URL").unwrap_or_else(|_| "claude-cli".to_string()),
            std::env::var("TACHI_QUALITY_MODEL").unwrap_or_else(|_| "claude-sonnet-4".to_string()),
        ),
    }
}
```

**Special case:** `Quality` tier with `base_url == "claude-cli"` triggers `call_claude_cli()` instead of HTTP API.

### 15.5 Migration Path

1. **Phase 1 (now):** Keep current lane defaults. Add `TACHI_BACKEND_*_TIER` env vars as overrides.
2. **Phase 2:** Add `BackendModelTier` to `LlmClient`, refactor `call_lane_llm` to accept tier.
3. **Phase 3:** Add per-task tier selection (e.g. simple extraction → Fast, complex synthesis → Balanced).

### 15.6 Related

- #151 — Background model selection discussion
- `llm.rs` lane architecture (`ChatLane::Extract`, `ChatLane::Distill`, etc.)
- `kanban.rs:310` — local ollama usage for kanban
- `daily_distill.rs` — `DistillBackend::ClaudeCli` vs `DistillBackend::RawApi`

---

## 16. Open Questions

| Question | Status | Owner |
|----------|--------|-------|
| Gemini CLI MCP support | 🔍 Need to check `gemini --help` | — |
| Qwen Code MCP support | 🔍 Need to check `qwen --help` | — |
| Droid CLI JSON output | 🔍 Need to check `droid exec --help` | — |
| Copilot `--additional-mcp-config` format | 🔍 Need to verify exact syntax | — |
| Codex `~/.codex/config.toml` MCP schema | 🔍 Need to find docs | — |
| GUI agents (Cursor, etc.) MCP server mode | 🔍 Future phase | — |
| Cost estimation accuracy | 🔍 Need real pricing data | — |
| Classifier accuracy with qwen | 🔍 Need to benchmark on real tasks | — |
| Idle timeout detection for non-streaming agents | 🔍 May need process-level monitoring | — |

---

## 17. Appendix: Agent CLI Discovery Notes

### A. Claude Code (v2.1.150) — Bun SEA binary

```bash
# Runtime: Bun Single Executable Application (JS→native)
# Binary: /opt/homebrew/Caskroom/claude-code/2.1.150/claude
# Config: ~/.claude/, ~/.claude/.mcp.json

claude -p --output-format json \
  --dangerously-skip-permissions \
  --mcp-config ~/.claude/.mcp.json \
  --system-prompt "You are a coding assistant" \
  --model sonnet \
  --max-budget-usd 5.0 \
  "Your prompt here"

# Key flags:
#   --agent <name>              Use a pre-defined agent
#   --agents <json>             Define custom agents inline
#   --system-prompt <text>      Override system prompt
#   --append-system-prompt      Append to default system prompt
#   --bare                      Minimal mode (skip hooks, LSP, etc.)
#   --tools <list>              Filter available tools
#   --json-schema <schema>      Enforce structured output
#   --max-budget-usd <n>        API budget cap
#   --output-format json|stream-json
#   --mcp-config <file>         MCP servers (JSON file)
#   --fallback-model <model>    Fallback when overloaded
```

### B. Codex CLI (v0.135.0) — Node.js ESM

```bash
# Runtime: Node.js (npm global: @openai/codex-cli)
# Binary: ~/.npm-global/bin/codex → bin/codex.js
# Config: ~/.codex/config.toml, ~/.codex/agents/*.toml (23 agents)

codex exec --json \
  --model gpt-5.4 \
  --sandbox workspace-write \
  --config model_reasoning_effort=high \
  -C /path/to/project \
  "Your prompt here"

# Key flags:
#   --config <key=value>        Override config.toml (nested paths supported)
#   -m, --model <model>         Model selection
#   -s, --sandbox <mode>        read-only | workspace-write | danger-full-access
#   --json                      JSONL output to stdout
#   -o, --output-last-message <file>  Write final message to file
#   --output-schema <file>      JSON Schema for structured output
#   --ephemeral                 Don't persist session files
#   -C, --cd <dir>              Working directory
#   --add-dir <dir>             Additional writable directories
#   --dangerously-bypass-approvals-and-sandbox
#   --dangerously-bypass-hook-trust

# Agents: ~/.codex/agents/ (23 TOML files)
#   architect, executor, team-executor, code-reviewer,
#   debugger, planner, analyst, critic, designer, writer,
#   researcher, verifier, vision, security-reviewer,
#   test-engineer, dependency-expert, git-master,
#   build-fixer, code-simplifier, explorer, etc.
# NOTE: exec mode has NO --agent flag. Tachi must inject instructions manually.
```

### C. Gemini CLI (v0.44.1) — Node.js bundle

```bash
# Runtime: Node.js (npm global: @google/gemini-cli)
# Binary: ~/.npm-global/bin/gemini → bundle/gemini.js
# Config: ~/.gemini/config/, ~/.gemini/config/mcp_config.json

gemini -p --output-format json \
  --mcp-config '{"mcpServers": {...}}' \
  --approval-mode yolo \
  --model gemini-2.5-pro \
  "Your prompt here"

# Key flags:
#   -p, --prompt                Non-interactive mode
#   --output-format json|stream-json|text
#   --mcp-config <json|file>    MCP config (JSON string OR file path)
#   --approval-mode <mode>      default | auto_edit | yolo | plan
#   --acp                       ACP (Agent Client Protocol) mode
#   --sandbox                   Enable sandbox
#   --model <model>             Model selection
#   --extensions <list>         Extensions to load
#   --resume <id>               Resume session
#   --include-directories <dirs>
#   --yolo                      Auto-approve all actions

# MCP: Already configured to connect to Tachi!
#   ~/.gemini/config/mcp_config.json contains:
#     { "tachi": { "command": ".../bin/tachi-server", "args": [...] } }
```

### D. Qwen Code (v0.17.0) — Node.js chunked

```bash
# Runtime: Node.js (npm global: @qwen-code/qwen-code)
# Binary: ~/.npm-global/bin/qwen → cli.js (6MB single file)
# Config: ~/.qwen/settings.json
# Bundled subcommands: batch, loop, new-app, qc-helper, review, stuck

qwen --output-format json \
  --system-prompt "You are a coding assistant" \
  --mcp-config '{"mcpServers": {...}}' \
  --approval-mode auto \
  --max-wall-time 5m \
  --sandbox \
  "Your prompt here"

# Key flags:
#   --output-format json|stream-json|text
#   --system-prompt <text>      Override system prompt
#   --append-system-prompt      Append to system prompt
#   --mcp-config <json|file>    MCP config (JSON string OR file path)
#   --approval-mode plan|default|auto-edit|auto|yolo
#   --sandbox                   Enable sandbox
#   --model <model>             Model selection
#   --max-wall-time <duration>  Budget: 90s, 5m, 1h, 1.5h
#   --max-tool-calls <n>        Tool call limit (-1 = unlimited)
#   --max-session-turns <n>     Session turn limit
#   --json-schema <schema>      Structured output schema
#   --json-fd <n>               File descriptor for JSON events
#   --json-file <path>          File path for JSON events
#   --bare                      Minimal mode
#   --acp                       ACP mode
#   -s, --continue              Resume recent session

# Subcommands:
#   qwen review                 Code review helper
#   qwen batch                  Batch processing
#   qwen loop                   Iterative mode
#   qwen serve                  HTTP daemon (experimental)
```

### E. GitHub Copilot CLI (v1.0.2) — Node/Bun SEA

```bash
# Runtime: Node.js/Bun Single Executable Application
# Binary: /opt/homebrew/Caskroom/copilot-cli/1.0.2/copilot
# Config: ~/.copilot/config.json

copilot -p --output-format json \
  --agent <custom-agent> \
  --additional-mcp-config '{"mcpServers": {...}}' \
  --allow-all-tools \
  --model claude-sonnet-4.6 \
  "Your prompt here"

# Key flags:
#   -p, --prompt                Non-interactive mode
#   --output-format text|json   JSON = JSONL output
#   --additional-mcp-config <json|@file>  MCP servers
#   --agent <agent>             Custom agent preset
#   --allow-all / --yolo        Enable all permissions
#   --allow-all-tools           Auto-approve all tools
#   --model <model>             Model selection
#   --autopilot                 Auto-continue in prompt mode
#   --max-autopilot-continues <n>
#   --no-ask-user               Disable ask_user tool
#   --allow-tool / --deny-tool  Tool whitelist/blacklist
#   --add-dir <dir>             Additional directories
#   --continue / --resume       Session management
#   --share                     Export session to markdown
#   --share-gist                Export to GitHub gist
#   --stream <on|off>           Streaming mode
```

### F. Factory Droid (v0.137.1) — Node/Bun SEA

```bash
# Runtime: Node.js/Bun Single Executable Application
# Binary: /opt/homebrew/Caskroom/droid/0.135.0/droid
# Config: ~/.factory/
# Models: Claude Opus/Sonnet, GPT-5.x, Gemini, DeepSeek, GLM, Kimi, MiniMax

droid exec --output-format json \
  --model gpt-5.5 \
  --auto high \
  --mission \
  --worker-model gpt-5.4 \
  --validator-model claude-opus-4-6 \
  "Build a full authentication system"

# Key flags:
#   --output-format <format>    Output format
#   --input-format <format>     stream-json | stream-jsonrpc
#   -m, --model <id>            Model ID (30+ models available)
#   -r, --reasoning-effort <level>
#   --auto low|medium|high      Autonomy tier
#   --skip-permissions-unsafe   Bypass ALL checks (DANGEROUS)
#   --mission                   Multi-agent mission orchestration
#   --worker-model <id>         Mission worker model
#   --validator-model <id>      Mission validator model
#   --enabled-tools <list>      Enable specific tools
#   --disabled-tools <list>     Disable specific tools
#   --list-tools                Print available tools and exit
#   --cwd <path>                Working directory
#   -w, --worktree [name]       Git worktree mode
#   -s, --session-id <id>       Continue existing session
#   --fork <id>                 Fork existing session
#   --append-system-prompt      Append to system prompt
#   --tag <spec>                Session tags

# Mission Mode (unique among CLI agents):
#   --mission enables built-in multi-agent orchestration
#   Workers spawned via factoryd, auto-approved
#   Only non-interactive CLI with native sub-agent support
```

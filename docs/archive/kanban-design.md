# Agent Kanban Communication Board

> Tachi v0.9 Feature — Inter-agent communication via shared memory kanban

## Overview

Agents in the Hapi Fleet (Hapi, Iris, Aegis, Ada) communicate by posting **cards** to a shared kanban board. Each card is a standard `MemoryEntry` with `category = "kanban"` and structured metadata. A small local model (27B, e.g. Qwen via Ollama) auto-classifies incoming cards in the background.

**Zero new database tables required** — kanban cards ARE memories.

## Architecture

```
┌──────────┐  post_card   ┌─────────────────────┐  classify   ┌───────────┐
│  Agent A │ ────────────→ │   Tachi Memory DB   │ ──────────→ │ 27B Model │
│  (Hapi)  │              │  category = "kanban" │ ←────────── │  (Ollama) │
└──────────┘              └─────────────────────┘  enriched   └───────────┘
                                    │
                           check_inbox("iris")
                                    │
                                    ▼
                            ┌──────────┐
                            │  Agent B │
                            │  (Iris)  │
                            └──────────┘
```

## Card Structure

A kanban card is a `MemoryEntry` where:

| Field | Value |
|---|---|
| `category` | `"kanban"` |
| `path` | `"/kanban/{from_agent}/{to_agent}"` |
| `text` | Card body / request content |
| `summary` | Card title |
| `source` | `"agent"` |
| `metadata` | Structured JSON (see below) |

### Metadata Schema

```json
{
  "from_agent": "hapi",
  "to_agent": "iris",
  "status": "open",
  "priority": "high",
  "card_type": "request",
  "thread_id": "uuid-for-conversation-threads"
}
```

- **status**: `open` → `acknowledged` → `resolved` | `expired`
- **priority**: `low` | `medium` | `high` | `critical`
- **card_type**: `request` | `report` | `alert` | `handoff`
- **to_agent**: specific agent ID, or `"*"` for broadcast

## MCP Tools

### 1. `post_card`

Post a card from one agent to another.

**Params:**
- `from_agent` (string, required)
- `to_agent` (string, required, use `"*"` for broadcast)
- `title` (string, required) → stored as `summary`
- `body` (string, required) → stored as `text`
- `priority` (string, default `"medium"`)
- `card_type` (string, default `"request"`)
- `thread_id` (string, optional)

**Behavior:**
1. Creates a `MemoryEntry` with `category="kanban"`
2. Saves immediately (non-blocking)
3. Fires background classification via small model (optional)

### 2. `check_inbox`

Query cards addressed to a specific agent.

**Params:**
- `agent_id` (string, required)
- `status_filter` (string, optional — e.g. `"open"`)
- `since` (ISO timestamp, optional)
- `include_broadcast` (bool, default `true`)

**Returns:** Cards sorted by priority (critical first), then by timestamp (newest first).

**SQL Strategy:**
```sql
SELECT * FROM memories
WHERE category = 'kanban'
  AND (path LIKE '/kanban/%/{agent_id}' OR path LIKE '/kanban/%/*')
  AND archived = 0
  -- optional status filter via JSON_EXTRACT on metadata
ORDER BY
  CASE JSON_EXTRACT(metadata, '$.priority')
    WHEN 'critical' THEN 0
    WHEN 'high' THEN 1
    WHEN 'medium' THEN 2
    ELSE 3
  END,
  timestamp DESC
```

### 3. `update_card`

Update the status of a kanban card.

**Params:**
- `card_id` (string, required)
- `new_status` (string, required)
- `response_text` (string, optional — appended as threaded reply)

**Behavior:** Uses revision-based optimistic locking (existing `update_with_revision()`).

## Small Model Classification

When a card is posted, Tachi optionally sends its title + body to a local 27B model for auto-enrichment:

**Prompt:**
```
Classify this inter-agent message. Return JSON only.
Title: {title}
Body: {body}
Output: {"topic": "...", "keywords": [...], "priority_suggestion": "..."}
```

**Config:** `KANBAN_MODEL_URL` env var (defaults to Ollama at `localhost:11434`).

Classification runs asynchronously — same pattern as existing embedding enrichment in `llm.rs`.

## Why This Design?

1. **Zero new tables** — Kanban cards are memories. All existing search (FTS, vector, graph) works on cards for free.
2. **Agent-as-a-Tool** — An agent calling `check_inbox` doesn't know it's talking to other AIs. It just sees "tool results". This is the simplest possible multi-agent communication pattern.
3. **Small model = cheap classification** — A 27B model on Ollama costs $0 and runs in ~200ms. Perfect for tagging priority, topic, and keywords without burning expensive API tokens.
4. **Scales to N agents** — No hardcoded routing. Any agent can post to any other agent. Broadcast cards (`to_agent="*"`) enable fleet-wide alerts.

## Prior Art & Inspiration

| Project | What we borrowed |
|---|---|
| **IronClaw** | Capability-based isolation — cards respect `sandbox_rules` |
| **NanoClaw** | Lightweight design — 3 tools, ~200 lines of new code |
| **Refly (Vibe Workflow)** | Visual kanban metaphor for workflow state |
| **OpenClaw** | MCP as the universal transport layer |

## Implementation Priority

1. **P0**: `post_card` + `check_inbox` + `update_card` (core loop)
2. **P1**: Small model auto-classification via `llm.rs`
3. **P2**: `sandbox_rules` integration (agent A can't read agent B's private cards)
4. **P3**: WebGate SSE streaming (push new cards to web frontend in real-time)

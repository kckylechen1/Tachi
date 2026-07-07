# Host Adapter Lifecycle V1

Status: draft canonical spec
Date: 2026-06-30

Related docs:

- [`kernel-surface-v1.md`](./kernel-surface-v1.md)
- [`project-cycle-memory-spine.md`](./project-cycle-memory-spine.md)
- [`subagent-eval-system.md`](./subagent-eval-system.md)
- [`tachi-continuity-memory-architecture.md`](./tachi-continuity-memory-architecture.md)

Tracking issues:

- [#445](https://github.com/kckylechen1/tachi/issues/445)
- [#792](https://github.com/kckylechen1/tachi/issues/792)

## Intent

Tachi should be the control plane for memory, project-cycle state, evidence,
dispatch, and profile projection. Codex, OpenCode, Claude, Gemini, IDE plugins,
and other agent hosts remain execution planes.

The host adapter lifecycle defines the neutral boundary between those two
layers:

```
host runtime event
  -> host adapter
  -> Tachi lifecycle event / read model / write path
  -> host-native prompt, continuation, feedback, or dispatch action
```

The goal is to absorb useful patterns observed in LazyCodex, OpenCode/OMO, and
other host systems without installing another control plane inside Tachi.

## Findings To Promote

The following host-system behaviors are useful and should become Tachi-native
contracts.

### Lifecycle Hooks

LazyCodex-style hooks are useful because they give the memory system exact
runtime timing. Tachi should support these neutral hook names:

| Hook | Direction | Purpose |
|---|---|---|
| `before_session` | host -> Tachi | Load briefing, active flow, profile, and capability bundle before work starts. |
| `before_prompt` | host -> Tachi | Attach task-specific memory, docs/spec refs, and profile guidance before a user turn. |
| `after_tool` | host -> Tachi | Record tool facts and run post-edit feedback providers when relevant. |
| `after_compact` | host -> Tachi | Preserve compacted session state and avoid losing active flow context. |
| `before_stop` | host -> Tachi | Check unfinished project-cycle criteria before the host ends the session. |
| `after_session` | host -> Tachi | Capture final outcome, evidence refs, distillation candidates, and checkpoints. |

These hooks are a contract. Each host can expose them through its own mechanism:
MCP calls, shell hooks, config snippets, agent markdown rules, or plugin APIs.

### Post-Edit Feedback

LazyCodex's LSP and comment-checker hooks are useful as a shape, not as a
dependency. Tachi should own a generic post-edit feedback contract:

```
after_tool(edit)
  -> detect touched files
  -> run enabled feedback providers
  -> return transient feedback to the host
  -> optionally record durable evidence when the feedback changed the outcome
```

The core should not depend on one JavaScript plugin, one editor, or one LSP
implementation. Feedback providers are capabilities selected by host, project,
and profile.

### Continuation Gate

LazyCodex's stop-hook continuation is useful because it prevents incomplete work
from being reported as finished. Tachi should implement the same idea using
Tachi state as the source of truth:

```
before_stop
  -> read active flow / dispatch / verification state
  -> derive unfinished required criteria
  -> emit a host-native continuation directive or allow stop
```

The continuation gate must read `.tachi/runs/<flow_id>/` artifacts, `cycle_plan`,
and verification evidence. It must not depend on `.omo/boulder.json` or any
host-specific state file.

### Evidence Criteria

`ulw-loop` is useful because it forces evidence for each acceptance criterion.
Tachi should make this first-class in the project-cycle spine:

```json
{
  "criterion_id": "verify-open-code-cli-fallback",
  "description": "OpenCode dispatch falls back to CLI when serve is unavailable.",
  "required": true,
  "evidence": [
    {
      "kind": "cargo_test",
      "command": "cargo test -p memory-server opencode_transport --locked",
      "artifact": ".tachi/runs/<flow_id>/verification.json",
      "status": "passed"
    }
  ]
}
```

`tachi_verify` should be able to record criterion-scoped evidence. `close_loop`
should either confirm required criteria coverage or record explicit gaps.

### Typed Host Adapters

OpenCode support already exists through `opencode_builder`, but it is still
represented as a custom backend in dispatch profile routing. OpenCode should be
promoted to a typed adapter:

| Field | Target |
|---|---|
| backend | `opencode` |
| transports | `opencode_cli`, `opencode_serve`, future ACP-compatible transports |
| credential profile | `opencode_shared` |
| artifacts | prompt, context, result, status, trajectory, transport metadata |
| default behavior | read-only unless profile policy grants write actions |

Typed adapters make routing, eval, credential policy, and failure diagnosis
clearer than a generic custom command.

### Agent Profile Projection

Tachi should project reviewed profile decisions into host-native instruction
surfaces:

- `AGENTS.md`
- `CLAUDE.md`
- `GEMINI.md`
- OpenCode config / agent prompts
- IDE-specific instruction surfaces where supported

Projection must stay review-gated. Tachi can draft and diff proposed changes,
but should not silently rewrite all host personalities.

### Session Inventory

Cross-agent session search is useful when multiple hosts work on the same
project. Tachi should ingest or index host session summaries as a neutral
session inventory:

```
host session transcript / summary
  -> compact session record
  -> flow refs, docs/spec refs, dispatch refs, evidence refs
  -> searchable project-cycle context
```

This should store compressed records and provenance, not raw private transcripts
by default.

## Findings Not To Promote

The following should not become Tachi defaults.

- Do not install LazyCodex or OMO as a required dependency of Tachi.
- Do not let `.omo` files become the source of truth for Tachi project-cycle
  state.
- Do not enable every host hook by default.
- Do not move host plugin management into the Tachi kernel.
- Do not hardcode project-specific model aliases, finance routes, or local
  commands into the generic package.
- Do not let child-agent output bypass leader verification.

An isolated lab profile is acceptable for comparison:

```bash
CODEX_HOME="$HOME/.codex-lazy" codex
```

That profile is a test surface, not the production Tachi control plane.

## Lifecycle Event Contract

Every adapter event should share a small envelope:

```json
{
  "event_id": "uuid",
  "event_type": "host.before_prompt",
  "host": "codex",
  "adapter": "codex-cli",
  "project": "Sigil",
  "cwd": "/Users/kckylechen/Desktop/Sigil",
  "flow_id": "flow_...",
  "session_id": "host-session-id",
  "turn_id": "host-turn-id",
  "refs": {
    "issue_ref": "kckylechen1/tachi#123",
    "pr_ref": null,
    "dispatch_id": null
  },
  "payload": {},
  "created_at": "2026-06-30T00:00:00Z"
}
```

The envelope should be append-only. Derived prompt context, continuation
directives, and feedback messages are read models over these events plus the
existing project-cycle artifacts.

## Hook Output Contracts

### `before_session`

Returns:

- Tachi runtime identity and health warnings.
- Project-scoped briefing.
- Active flow or issue/PR hints.
- Recommended capability bundle.
- Host profile guidance.

### `before_prompt`

Returns:

- compact memory context;
- linked docs/specs from the active flow;
- unresolved acceptance criteria;
- relevant patterns, wiki pages, and profile rules;
- host-specific instruction packet.

### `after_tool`

Records:

- tool name and safe summary;
- touched files where safe;
- feedback provider results;
- optional evidence refs.

Returns:

- transient warnings or recommended next checks;
- no durable facts unless the event is explicitly saved or linked to outcome
  evidence.

### `after_compact`

Records:

- compact summary;
- active flow and dispatch refs;
- open criteria;
- next-step directive.

### `before_stop`

Returns:

- `allow_stop: true` when no required work remains; or
- `allow_stop: false` with a bounded continuation directive.

Loop guard:

- continuation must be scoped to one active flow/session;
- repeated stop blocks must include the same blocker id;
- after a configured limit, Tachi should record a checkpoint instead of
  endlessly reinjecting continuation text.

### `after_session`

Records:

- outcome summary;
- verification refs;
- issue/PR/docs refs;
- durable memory or wiki candidates;
- subagent eval rows where applicable.

## Generic Chat-Agent Memory Adapter Contract

The host lifecycle hooks above are generic enough for coding agents, but chat
agents also need a smaller memory-specific contract that can be embedded in
zeroclaw, RomanBath-like harnesses, OpenClaw-style plugins, local CLIs, or any
other conversation host. This adapter is generic: RomanBath may be a fixture or
example consumer, but no field, policy, or prompt shape is RomanBath-specific.

The adapter must operate without Tachi GitHub, dispatch, ship, release, or
project-cycle surfaces. Those surfaces can enrich a coding workflow, but durable
chat memory only depends on the portable kernel primitives: retain, recall,
reflect when available, readiness, and optional direct read/edit operations.

This section is a target adapter contract. Current Tachi bindings are named
explicitly below; OMP/Hindsight-style retain/recall/reflect lifecycle event
names are proposed adapter vocabulary until a later implementation adds
executable event emission and projection tests.

### Context Assembly Input

The host calls the adapter before a model turn with a bounded request:

```json
{
  "host": "zeroclaw",
  "adapter": "generic-chat-agent",
  "project": "optional-project-name",
  "session_id": "host-session-id",
  "turn_id": "host-turn-id",
  "actor": {
    "user_id": "stable-user-or-profile-id",
    "agent_id": "character-or-agent-runtime-id"
  },
  "conversation": {
    "recent_messages": [],
    "summary_ref": "optional-host-summary-ref",
    "continue_memory_ref": "optional-host-continuation-ref"
  },
  "query": {
    "text": "current user turn or host-supplied recall query",
    "intent": "chat|roleplay|coding|support|other",
    "entities": [],
    "topics": []
  },
  "limits": {
    "max_context_tokens": 4000,
    "max_recall_items": 8
  },
  "policy": {
    "persona_profile": "adapter-policy-ref",
    "allow_memory_write": true,
    "allow_reflection": true,
    "allow_edit_by_id": false
  }
}
```

Required input semantics:

- `conversation.recent_messages` is transient host context, not durable memory.
- `summary_ref` and `continue_memory_ref` point to stable summaries or host
  continuation memory already accepted by the host.
- `query` is the immediate recall seed and may be rewritten by adapter policy
  before hitting the kernel.
- `policy.persona_profile` is an adapter policy reference. It is not a durable
  memory schema field.

### Context Assembly Output

The adapter returns typed sections instead of one untyped memory block:

```json
{
  "adapter": "generic-chat-agent",
  "context": {
    "stable": {
      "conversation_summary": [],
      "continue_memory": [],
      "profile_memory": []
    },
    "immediate_recall": [],
    "reflection": [],
    "readiness": {
      "status": "ready|degraded|unavailable",
      "warnings": []
    }
  },
  "provenance": {
    "memory_ids": [],
    "event_ids": [],
    "kernel": "tachi"
  }
}
```

The ordering is load-bearing:

1. Stable mental model first: accepted summaries, continue memory, profile
   memory, and other durable projections that define the conversation frame.
2. Immediate recall second: search results triggered by the current turn.
3. Reflection third: synthesized guidance when the kernel supports it and host
   policy allows it.

This follows the Hindsight/OMP mental-model-before-recall shape: the agent first
knows the stable conversation model, then reads fresh recall hits. The adapter
must not flatten summaries, continuation memory, search hits, and reflections
into one prompt bucket where authority and freshness are indistinguishable.

### Memory Tool Surface

The generic adapter exposes only a small memory surface to the host:

| Adapter operation | Kernel mapping | Required | Implementation status | Notes |
|---|---|---|---|---|
| `retain` / `save` | `tachi_memory(action="save")`, `save_memory`, or kernel save API | yes | current save path; `retain` is adapter vocabulary | Stores durable user/session facts with provenance and policy labels. |
| `recall` / `search` | `tachi_memory(action="search")`, `search_memory`, or kernel search API | yes | current search path | Returns ranked memory rows plus source, scope, and confidence metadata. |
| `reflect` / `synthesize` | `tachi_memory(action="ask")`, distill/read-model API, or no-op | optional | partially current through ask/distill; adapter reflection API is target | Produces synthesis only when the kernel and adapter policy support it. |
| `status` / `readiness` | `tachi_memory(action="readiness")`, `tachi_status`, or runtime info | yes | current | Reports degraded recall, locked vault, vector gaps, or unavailable kernel. |
| `read_by_id` | `tachi_memory(action="get")` or kernel get API | optional | current get path | Allowed only when the host policy permits direct memory reads. |
| `edit` | kernel update/edit API | optional | proposed/target | Allowed only where the kernel has reviewed edit semantics. |

The surface deliberately excludes GitHub, dispatch, ship, release notes, worker
spawning, and direct Hindsight HTTP calls. A host may have those tools for other
reasons; this adapter contract does not require or expose them.

### Event Projection

Chat adapters should emit neutral lifecycle events and project them into the
existing or planned Tachi event/read-model surfaces:

| Host action | Lifecycle event | Implementation status | Projection target |
|---|---|---|---|
| Session starts | `host.before_session` | proposed/target | runtime identity, readiness, stable profile context |
| Prompt assembled | `host.before_prompt` | proposed/target | context assembly receipt and memory refs |
| User or agent fact retained | `memory.retain_requested` -> `memory.saved` | `memory.saved` current; request event proposed | durable memory row plus continuity event when enabled |
| Recall performed | `memory.recall_requested` -> `memory.recall_returned` | proposed/target | recall telemetry, access history, optional recall-cache evidence |
| Reflection requested | `memory.reflect_requested` -> `memory.reflection_returned` | proposed/target | synthesis artifact or no-op reason |
| Host summary compacted | `host.after_compact` | proposed/target | continue memory, summary refs, open threads |
| Session ends | `host.after_session` | proposed/target | outcome summary, durable candidates, distillation candidates |

Projection rules:

- The append-only event ledger remains the evidence substrate.
- Durable memory rows and continuity projections remain kernel-owned.
- Adapter receipts may include prompt section ordering and memory ids, but not
  raw private transcripts by default.
- Host policy may suppress retention, reflection, or direct edit/read, but it
  must report that suppression in readiness or the operation receipt.

### Policy Boundary

Persona, character-card behavior, tone, roleplay style, safety narration, and
token-pressure choices are adapter policy. The portable kernel owns durable
memory schema, recall primitives, provenance, access history, and continuity
projection semantics.

That split keeps the same memory kernel reusable across coding agents, chat
agents, zeroclaw consumers, RomanBath fixtures, and future host adapters without
hardcoding one character system or one UI prompt format.

## Relationship To Existing Surfaces

### Kernel Surface

Host lifecycle belongs to the runtime layer of Kernel Surface V1. It should not
inflate the default model-facing tool list. Hosts call lifecycle hooks through
adapter wiring; agents see only the compact output that is relevant to the
current task.

### Project Cycle

Lifecycle hooks should enrich, not replace, Project Cycle:

```
issue/spec -> cycle_plan -> dispatch -> verify -> PR -> release_note -> close_loop
```

`before_prompt` reads this state. `before_stop` checks it. `after_session`
distills it.

### Subagent Eval

Host adapters may run downstream agents, but the leader remains accountable.
Typed host adapters should record transport, model, role, verification impact,
and failure mode so `tachi_task(action="recommend")` can learn from actual
outcomes.

### Continuity Memory

Lifecycle events become one more evidence source for pattern, timeline, bonding,
lorebook, affect, A2A, and profile projections. The source of truth remains the
append-only event ledger plus project-cycle artifacts.

## Implementation Phases

### Phase 1: Spec And Event Skeleton

- Add the lifecycle spec.
- Add a neutral event shape for host lifecycle events.
- Add read-only helpers that can resolve active project, flow, and runtime
  identity for a host call.
- Do not change host behavior yet.

### Phase 2: Typed OpenCode Adapter

- Promote `opencode_builder` from `backend="custom"` to an explicit OpenCode
  backend path.
- Preserve current CLI and serve behavior.
- Preserve `opencode_shared` credential policy.
- Record adapter and transport metadata in dispatch status artifacts.
- Keep compatibility for existing `opencode_builder` profile names.

### Phase 3: Evidence Criteria

- Extend verification artifacts with optional `criteria[]`.
- Allow `tachi_verify` to record evidence against a criterion id.
- Make `cycle_plan` and `close_loop` report missing required criteria.

### Phase 4: Continuation Gate

- Add a `before_stop` read model over active flow state.
- Emit bounded continuation directives when required criteria are incomplete.
- Add loop guards and checkpoint fallback.

### Phase 5: Post-Edit Feedback Providers

- Add provider interface for post-edit checks.
- Start with command-based providers that can run existing project checks.
- Store transient feedback separately from durable memory.

### Phase 6: Agent Profile Projection

- Use existing profile proposal machinery to draft host instruction updates.
- Render reviewed projections into host-specific files.
- Require explicit review/confirm before writes.

### Phase 7: Session Inventory

- Add a compact host-session index.
- Link session records to flows, docs/specs, dispatch ids, verification, and
  close-loop artifacts.
- Keep raw transcripts out of default recall.

## Acceptance Criteria

- `docs/engineering/architecture/kernel-surface-v1.md` names host lifecycle as a
  runtime-layer contract.
- `opencode_builder` still resolves and dispatches, but the routing code exposes
  an explicit OpenCode backend path.
- Dispatch status artifacts include host adapter and transport metadata for
  OpenCode runs.
- `tachi_verify` can record criterion-scoped evidence without breaking existing
  verification ledgers.
- `cycle_plan` surfaces missing required criteria as blockers.
- `close_loop` records required evidence coverage or explicit evidence gaps.
- A `before_stop` lifecycle read model can allow stop or return one bounded
  continuation directive.
- Post-edit feedback can run at least one configured provider and return
  transient feedback without writing ordinary memory.
- Agent profile projection can draft host instruction updates without applying
  them until confirmed.

## Test Plan

Run targeted Rust tests as the implementation lands:

- `cargo test -p memory-server opencode_transport --locked`
- `cargo test -p memory-server profile_resolution --locked`
- `cargo test -p memory-server cycle_plan --locked`
- `cargo test -p memory-server verify --locked`
- `cargo test -p memory-server agent_profile --locked`

Run product smokes after the slices are integrated:

- `tachi status`
- Tachi briefing / save / ask smoke
- OpenCode dispatch through the `opencode_builder` compatibility profile
- Verification ledger record + board
- Project-cycle close-loop preview with missing and satisfied criteria

## Open Questions

- Should lifecycle event storage reuse `tachi_events` immediately, or start with
  `.tachi/runs/<flow_id>/host_events.jsonl` and project into `tachi_events`
  after the shape stabilizes?
- Should post-edit feedback providers be configured in dispatch profiles,
  project profiles, or a new host adapter config?
- Should the first profile projection target only `AGENTS.md`, or also draft
  `CLAUDE.md` and `GEMINI.md` in the same slice?
- What is the stop-hook loop limit before Tachi should checkpoint instead of
  continuing?

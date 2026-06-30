# Host Adapter Lifecycle V1

Status: draft canonical spec
Date: 2026-06-30

Related docs:

- [`kernel-surface-v1.md`](./kernel-surface-v1.md)
- [`project-cycle-memory-spine.md`](./project-cycle-memory-spine.md)
- [`subagent-eval-system.md`](./subagent-eval-system.md)
- [`tachi-continuity-memory-architecture.md`](./tachi-continuity-memory-architecture.md)

Tracking issue: [#445](https://github.com/kckylechen1/tachi/issues/445)

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

# Tachikoma Backplane: Deck, Card, Move, and Upstream Skill Corpus

Status: draft canonical spec
Updated: 2026-06-15
Related: GitHub issue #381, GitHub issue #382, GitHub issue #383,
`docs/engineering/architecture/dispatch-policy-learning-spec.md`,
`docs/engineering/architecture/credentialed-dispatch-profiles.md`

This spec defines the vocabulary and boundaries for treating Tachi as a
Tachikoma-style agent work backplane: a shared layer that receives bounded work
from the primary IDE assistant or human, routes it to small role cards, records
evidence, and links outcomes back to GitHub, docs, and memory.

This document replaces the earlier broader "AgentD / Harness / Agent Party"
direction with the smaller MVP from #381: **Deck, Cards, Skill Slots, Poke
probes, and Card evolution**. It also incorporates the upstream-corpus boundary
from #382: Superpowers and Waza are upstream-managed sources that Tachi tracks,
pins, diffs, scans, and syncs through review; Tachi does not silently evolve
their source text.

This document also positions the acpx/ACP direction from #383: acpx can become
an optional dispatch execution backend, but it must remain below the Tachi
control plane. It transports a Tachi-built work packet to an ACP-compatible
agent and streams structured events back into Tachi artifacts. It does not
select Cards, mutate skill sources, own evidence, write GitHub state, or replace
Tachi's review gates.

## Vocabulary

Keep the public model small.

| Term | Meaning |
|---|---|
| **Tachikoma backplane** | The whole Tachi shared work/memory/evidence/skill system that sits behind the primary IDE assistant or human. |
| **Deck** | The set of available Cards for a project/runtime. |
| **Card** | One subagent evaluation/profile unit. It defines role, permissions, behavior, evidence contract, metrics, and skill loadout. A Card is currently implemented as a `DispatchProfileDef` with a Tachikoma-facing JSON view. |
| **Superpowers** | Development guidance / lifecycle doctrine. They govern when to plan, execute, review, verify, and ship. |
| **Waza** | Concrete tactical techniques. They are working methods such as `check`, `health`, `hunt`, `read`, `think`, `write`. |
| **External skill** | A discovered plugin/skill that must be inspected and approved before use. |
| **Move** | A concrete Waza or approved external skill available to a Card. Superpowers are guidance, not moves. |
| **Guidance** | The Superpowers doctrine attached to a Card. Guidance shapes decision gates, not executable moves. |
| **Evidence contract** | The artifacts and verification the Card must produce for a task to be considered complete. |
| **Evolution** | Reviewed changes to a Card's skill set, evidence contract, strengths/weaknesses, or permissions based on task outcomes. |
| **Execution backend** | The low-level adapter that runs a Tachi-assembled prompt/work packet through an agent transport such as a CLI subprocess or ACP client. It is transport, not planning or governance. |

## Product Loop

```text
Primary IDE assistant / human
  -> bounded work slot
  -> Card selection (Poke / SCV / Raven / Medic)
  -> Guidance injection (Superpowers)
  -> Move resolution (Waza + approved external skills)
  -> bounded worker dispatch
  -> execution backend (CLI / acpx / future transport)
  -> evidence artifact collection
  -> leader verification
  -> /eval completion row
  -> Card performance summary
  -> evolution proposal (review-required)
  -> GitHub issue/PR + docs + memory link-back
```

Tachi owns the control plane. Worker CLIs such as Codex, Claude Code, OpenCode,
Kimi, GLM, DeepSeek, and ACP clients keep their native execution loops. Tachi
passes bounded work packets to them and collects evaluated outcomes; it does not
replace their inner agents.

## ACP Execution Backends

Tachi now has two ACP-shaped execution paths:

| Backend | Transport | Tachi contract |
|---|---|---|
| `harness_transport="acpx"` | acpx CLI subprocess | Use upstream acpx as an external controller; Tachi preserves acpx JSON output and maps selected events into dispatch artifacts. |
| `harness_transport="acp-native"` | Tachi-owned Rust JSON-RPC controller | Spawn an ACP stdio adapter directly, persist a Tachi ACP session checkpoint, write raw ACP NDJSON, and map updates into `trajectory.jsonl` / `progress.jsonl`. |

The native Rust path is intentionally a controller, not a full acpx clone. The
MVP implements the session/prompt loop that Tachi needs:

- `initialize`
- `session/resume` or `session/load` when a stored `acp_session_id` exists
- `session/new` when no stored session exists or reconnect fails
- `session/prompt` with a Tachi-built text content block
- conservative `session/request_permission` handling: default maps to
  read/search approval only; write/shell permissions are denied/cancelled

Native ACP session identity follows the same useful acpx principle:

```text
(agent command, absolute cwd, optional session name) -> stored acp_session_id
```

The process is disposable; the session is not. Each dispatch can spawn a fresh
adapter process while reusing the ACP session id stored under
`~/.tachi/sessions/acp/*.json`. The raw dispatch stream is written to
`~/.tachi/runs/<dispatch_id>/acp.stream.ndjson` as unwrapped ACP JSON-RPC lines.
The session record and companion Markdown distill are the memory-friendly
checkpoint/index; they are derived artifacts, not a replacement event protocol.

Deferred until needed:

- warm queue-owner process and local IPC for serializing many prompts through
  one live ACP connection
- ACP terminal client methods
- broad file-system client methods
- acpx flows, compare, status/cancel parity, and GitHub writeback

## Existing Foundation

This model reuses what already exists instead of creating a parallel system.

1. **Dispatch profiles are Cards.**
- `DispatchProfileDef` in `crates/memory-server/src/dispatch_profile.rs`
  already carries `role`, `stage`, `common_skills`, `signature_skills`,
  `forbidden_skills`, `evidence_required`, `strong_against`, and
  `weak_against`.
- The runtime JSON view (`profile_json_for_server`) already emits an
  `mbit_card` object with a Poke/SCV/Raven `archetype`, `stats`,
  `skill_loadout`, `evidence_contract`, and an `evolution` projection.
   - `tachi_task(action="profiles" | "profile" | "card")` already exposes this
     JSON.

2. **Superpowers and Waza are builtin skills.**
   - Stable IDs live in `crates/memory-server/src/skill_policy.rs`.
   - Source content lives under `skill/superpowers/skills/` and
     `skill/waza/skills/`.
   - `crates/memory-server/src/builtins.rs` seeds them into the Hub on startup
     and registers skill tools.
   - `tachi_skill(action="discover" | "bundle" | "loadout" | "run")` exposes the
     skill surface.

3. **Dispatch already produces evidence artifacts.**
   - `crates/memory-server/src/dispatch_ops/` writes `prompt.md`, `context.md`,
     `capability_bundle.json`, `trajectory.jsonl`, `progress.jsonl`,
     `status.json`, and `result.md` under `~/.tachi/runs/<dispatch_id>/`.

4. **Card evolution pieces already exist.**
   - `dispatch_profile_card_overlays` namespace stores reviewed overlays.
   - Route policy proposals and rules live in
     `dispatch_route_policy_proposals` and `dispatch_route_policy_rules`.

## Upstream Corpus Boundary

Superpowers and Waza are **upstream-managed corpora**, not locally
self-evolving assets.

| Corpus | Upstream owner | Tachi role |
|---|---|---|
| Superpowers | `obra/superpowers` | lifecycle guidance / development doctrine source |
| Waza | `tw93/Waza` | tactical working-method / move source |

Tachi vendors selected skills into `skill/superpowers/...` and
`skill/waza/...` and registers them as builtin skills. Tachi may evolve its own
Card loadouts and local overlays, but the authoritative source text of an
upstream skill may change only through a reviewed upstream sync.

Hard rules:

- Tachi must not silently rewrite Superpowers/Waza source text based on local
  task outcomes.
- Local task evidence may propose an upstream issue/PR or a Tachi-local Card
  loadout change.
- Card loadouts can evolve inside Tachi.
- Upstream skill snapshots update only through reviewed sync.
- External skills discovered from the web/registry must go through inspect +
  scan + review before becoming approved moves.

Use this rule:

```text
Local evidence can change Tachi Card loadouts.
Upstream-managed skill sources change through upstream sync or upstream contribution.
```

### Source metadata

Each builtin skill copied from an upstream corpus should eventually carry source
metadata. The first PR only documents the schema and adds a manifest; it does
not yet fetch or update upstream automatically.

```yaml
source:
  kind: upstream_skill_repo
  repo: obra/superpowers
  path: skills/verification-before-completion/SKILL.md
  pinned_ref: <tag-or-commit>
  pinned_sha: <commit_sha>
  update_policy: reviewed_sync
  local_overlay: null
```

For Waza:

```yaml
source:
  kind: upstream_skill_repo
  repo: tw93/Waza
  path: skills/check/SKILL.md
  pinned_ref: <tag-or-commit>
  pinned_sha: <commit_sha>
  update_policy: reviewed_sync
  local_overlay: tachi-routing-only
```

Current pinned source metadata is exposed through:

```bash
tachi skill-surface sources
tachi skill-surface sources --json
```

This is a read-only status surface. It reports the vendored manifest refs and
metadata coverage; it does not fetch upstream, compute diffs, update skill text,
or write GitHub state.

Reviewed upstream sync planning is exposed through:

```bash
tachi skill-surface sync-plan
tachi skill-surface sync-plan --json
```

This is also read-only. It checks the pinned upstream repo/ref/sha against the
latest upstream ref, computes tracked vendored skill-file changes, classifies
the review risk, and lists affected Card loadouts. It does not update the local
snapshot; accepted updates still land through a reviewed PR.

## MVP Cards

Do not create an agent zoo. First version uses three Cards plus one mode.

### 1. Poke

Product probe / synthetic agent user.

- **Purpose**: Test Tachi through public agent-facing surfaces the way a real
  agent would use it. Produce reproducible evidence, not code fixes.
- **Guiding line**: *Poke tests Tachi as an agent would use it, not as Rust
  would compile it.*
- **Evidence contract**: `probe_result_json`, expected vs observed behavior,
  artifact paths, repro steps, cleanup status.
- **Forbidden by default**: broad Rust gates such as clippy/fmt as the primary
  purpose; direct code edits; direct merge.
- **Capability profile**: read-only; no GitHub writes; no merge.
- **Suggested backend**: long-context, low-cost, exploratory model.

### 2. SCV

Bounded builder / implementation unit.

- **Purpose**: Execute a clear, scoped task packet. Make bounded code/docs
  changes. Report changed files, tests run, known gaps, and handoff notes.
- **Evidence contract**: `changed_files`, `tests_run`, `result_summary`,
  `known_gaps`, `scope_guardrail`.
- **Forbidden by default**: unbounded redesign; silent scope expansion; direct
  merge; changing high-risk surfaces without explicit authorization.
- **Capability profile**: write-code; no merge; no GitHub writes.
- **Suggested backend**: coding-specialized model with strong style-following
  and test-iteration ability.

### 3. Raven

Verifier / reviewer / hidden-risk scanner.

- **Purpose**: Review outputs from SCV or external agents. Check evidence
  sufficiency. Flag missing tests, risk boundaries, stale verification, and
  merge blockers.
- **Evidence contract**: findings by severity, file references, verification
  result, merge blockers, confidence / uncertainty.
- **Forbidden by default**: implementing fixes; approving without reading
  artifacts; treating CI green as sufficient proof by itself.
- **Capability profile**: read-only; no code edits; no merge.
- **Suggested backend**: strong-reasoning model with risk awareness.

### 4. Medic mode

Do **not** make Medic a separate Card in MVP. Medic is an SCV mode for small,
local hotfix/self-heal loops.

Allowed examples:

- clear syntax/import/type failure
- small conflict repair
- failed probe with obvious one-file fix
- trivial test fixture update

Guardrails:

- max files changed: small fixed budget
- max attempts: 1 or 2
- must use existing failure evidence
- must rerun the exact failed probe/test
- cannot expand scope
- cannot merge

## Card Schema

A Card is exposed as a JSON object that extends the existing
`DispatchProfileDef` runtime view. New fields are additive only.

```yaml
id: poke
display_name: Poke
archetype: poke
role: product_probe
stage: probe

authority:
  write_code: false
  merge: false
  github_write: false
  can_dispatch_followup: false

guidance:
  superpowers:
    - skill:superpowers-verification-before-completion

moves:
  waza:
    - skill:waza-health
    - skill:waza-check
    - skill:waza-tachi
  external: []

strengths:
  - black_box_probe
  - workflow_smoke
  - artifact_check
  - agent_facing_ux_regression

weaknesses:
  - deep_code_fix
  - architecture_design
  - large_refactor

personality:
  curiosity: 95
  caution: 75
  speed: 80
  risk_control: 85

evidence_contract:
  required:
    - probe_result_json
    - expected_vs_observed
    - artifacts
    - repro_steps

metrics:
  task_count: 0
  success_rate: null
  false_positive_rate: null
  avg_latency_ms: null
  avg_cost_usd: null
  human_override_rate: null

evolution:
  status: starter
  proposals: []
```

## Backend Is a Capability Requirement, Not a Model Name

Card `backend` and `model` fields describe the **capability profile** the role
needs, not a hardcoded provider/model string. The dispatcher resolves the
profile against available providers, cost policy, and current health.

| Role need | Capability profile | Typical resolution |
|---|---|---|
| `product_probe` | long context, low cost, acceptable creativity | Gemini / fast explorer |
| `bounded_executor` | strong code style, test iteration, low scope drift | Codex / GLM-5.1 / kimi-for-coding |
| `reviewer` | strong reasoning, risk awareness, does not implement | Claude / GPT-4-class |
| `medic` | low latency, small-patch reliability | same pool as executor, tight guardrails |

This keeps Cards portable across model providers and prevents hardcoding a
single model for a role.

## Execution Backend Boundary

The execution backend is one layer below Card selection.

```text
Tachi task / issue
  -> Card authority + Guidance + Moves + evidence contract
  -> Tachi-generated prompt.md / context.md / capability_bundle.json
  -> execution_backend
  -> raw transport events
  -> Tachi-owned trajectory.jsonl / progress.jsonl / status.json / result.md
```

This boundary prevents acpx, OpenCode, Codex CLI, Claude CLI, or future
transports from becoming a second orchestration product inside Tachi.

Rules:

- Cards decide role posture, authority, skill loadout, and evidence contract.
- Execution backends only run the already-assembled work packet.
- Tachi run artifacts remain authoritative even when the backend has its own
  session history.
- Backend-specific raw logs may be saved as additive audit files, but they do
  not replace `trajectory.jsonl`, `progress.jsonl`, `status.json`, or
  `result.md`.
- Backend adapters must be isolated. Do not scatter command construction,
  permission mapping, or event parsing across dispatch code.
- Backend selection is opt-in while the backend is experimental.

### acpx / ACP adapter

Issue #383 proposes acpx as an optional ACP execution backend for dispatch.
That direction fits this spec if acpx stays below the Tachi control plane.

Tachi should still own:

- Card selection and evaluation;
- Superpowers/Waza and approved external skill loadouts;
- Card authority and permission policy;
- prompt/context/capability artifact generation;
- evidence ledger and run status;
- GitHub/docs/memory governance;
- leader or Raven verification before trust/ship.

acpx may provide:

- ACP transport;
- structured JSON/ACP event stream;
- one-shot execution from `prompt.md`;
- named sessions such as `poke`, `scv`, `raven`, or `scv-medic`;
- prompt queueing, status, cancel, and session diagnostics;
- adapter access to ACP-compatible agents.

The runtime metadata should be additive, for example:

```json
{
  "execution_backend": "acpx",
  "acpx": {
    "agent": "codex",
    "mode": "session",
    "session": "scv",
    "cwd": "<repo>",
    "format": "json",
    "permissions": "approve-reads",
    "controls": {
      "status": {"supported": true},
      "cancel": {"supported": true}
    }
  }
}
```

The first adapter pass is configured by environment variables rather than a
new public facade:

| Setting | Meaning |
|---|---|
| `harness_transport="acpx"` | Opt in to the ACP execution backend for dispatch. |
| `TACHI_ACPX_COMMAND` | Backend command, default `acpx`; use `npx` for a pinned package run. |
| `TACHI_ACPX_ARGS` | Extra argv before acpx global flags, e.g. `-y acpx@0.10.0` with `npx`. |
| `TACHI_ACPX_AGENT` | ACP agent override when Tachi's dispatch agent name is not enough. |
| `TACHI_ACPX_RUN_MODE` | `exec` by default; `session` enables named persistent sessions. |
| `TACHI_ACPX_SESSION` | Explicit acpx session name; otherwise derive `poke`, `scv`, `raven`, or `scv-medic` from Card hints. |

Reference baseline: `openclaw/acpx` 0.10.0 requires Node `>=22.13.0`. Tachi
should keep this isolated in the adapter because acpx is still alpha.

Default permission posture must be conservative:

| Card/mode | acpx default |
|---|---|
| Poke | read/probe oriented; prefer one-shot where possible |
| SCV | scoped write policy only when dispatch policy permits |
| Raven | read-only review/verification |
| Medic mode | scoped SCV write policy plus attempt/file budget |

Hard rules:

- Do not default to acpx `--approve-all`.
- Card authority must be applied before invoking acpx.
- acpx `flow run`, GitHub writeback, and auto-merge are out of scope for MVP.
- Missing acpx or unsupported runtime prerequisites must produce actionable
  errors.
- One-shot `exec` mode does not create an acpx session; status/cancel controls
  are only meaningful when `TACHI_ACPX_RUN_MODE=session`.
- acpx-generated results still require Tachi/Raven/leader verification before
  trust, ship, or close-loop.

Initial event mapping should stay small:

| acpx/ACP event | Tachi artifact |
|---|---|
| thinking/message | `progress.jsonl` event |
| tool call start/end | `trajectory.jsonl` event |
| diff/edit event | changed-files evidence |
| permission request/denial | policy evidence |
| final response/end_turn | `result.md` + `status.json` |
| cancel/status/dead session | dispatch lifecycle event |

Raw backend events can be persisted as `acpx_events.jsonl` for audit and
debugging.

## Skill Intake Path

External skills are plug-in moves. They must pass a gate before becoming
available to a Card.

```text
Discover via web/search/registry
  -> inspect README/SKILL.md/scripts/permissions
  -> scan with SkillSpector-style gate
  -> normalize into Tachi move metadata
  -> register disabled or review-required
  -> bind to a Card only after approval
  -> track outcomes and downgrade/promote over time
```

Do not directly install and execute arbitrary skills from the web.

## Card Evolution Loop

Cards evolve from evidence, but default Card changes must be reviewed before
taking effect.

```text
Task run
  -> card_id + skills_used + outcome + evidence
  -> eval row / evidence ledger
  -> card performance summary
  -> evolution proposal
  -> human/main-assistant review
  -> approved card update
```

Examples:

- SCV repeatedly succeeds on bugfixes when `waza/hunt` is present -> propose
  promoting `waza/hunt` to default.
- Raven misses UI regressions -> add `weak_against: ux_review` or propose a
  specialized future card.
- Poke produces too many false positives in one probe -> downgrade that probe
  or tighten its evidence contract.
- SCV expands scope too often -> tighten authority and required scope report.

Evolution rules:

- Observations may be automatic.
- Proposals may be automatic.
- Default card changes must be reviewed before taking effect.

## Public Facade Rule

Tachikoma features must stay inside existing domain facades. Do not add new
public facades such as `tachi_card` or `tachi_deck` while an existing facade can
carry the workflow.

| Capability | Public surface | Status |
|---|---|---|
| list/show Cards | `tachi_task(action="profiles" \| "profile" \| "card")` | implemented |
| read-only Card CLI convenience | `tachi card list` / `tachi card show <id>` | starter implemented |
| skill discovery / loadout | `tachi_skill(action="discover" \| "bundle" \| "loadout")` | implemented |
| upstream source status | `tachi skill-surface sources` | implemented |
| upstream source sync planning | `tachi skill-surface sync-plan` | starter implemented |
| execution backend selection | existing dispatch path via `harness_transport="acpx"` with additive backend metadata | starter implemented |
| dispatch backend status/cancel | `tachi_task(action="status" \| "cancel", dispatch_id=...)` | starter implemented |
| Poke smoke suite | `tachi poke run --suite smoke` | starter implemented |
| Card evolution proposals | `tachi_task(action="proposals" \| "apply_proposals")` | implemented |

The first native CLI convenience layer is read-only (`tachi card list` and
`tachi card show <id>`) and calls the same task facade. Future convenience
commands such as `tachi poke run` should follow the same rule. acpx support
should also stay inside the existing dispatch path; do not add a broad public
`tachi_acpx` facade for the MVP.

## Poke Local Smoke Suite (Phase 2)

First executable feature should be Poke, not a daemon.

Command shape:

```bash
tachi poke run --suite smoke
```

Initial probes:

1. **Memory probe**: save a `poke_` fact, search it, get it, verify exact
   content and cleanup/residue behavior.
2. **Skill surface probe**: discover/list builtin skills, verify
   Waza/Superpowers builtin presence, run or render a safe skill contract if
   callable.
3. **Shell artifact probe**: run a local shell/flow action in a safe temp
   project, verify `instruction.md`, `status.json`, and injected SOP artifact.
4. **Dispatch mock probe**: run no-op/mock dispatch path where possible,
   verify `prompt.md`, `context.md`, `capability_bundle.json`,
   `trajectory.jsonl`, and `status.json`.
5. **Verification ledger probe**: write/read a small verification item, ensure
   unrelated flow evidence is not treated as current proof.

Output:

```text
.tachi/runs/poke_<timestamp>/
  report.json
  report.md
  probes/
    memory_basic.json
    skill_surface.json
    shell_artifact.json
    dispatch_mock.json
    verify_ledger.json
```

Starter implementation status: `tachi poke run --suite smoke` runs these probes
against an isolated sandbox home under the Poke report directory. It writes
`report.json`, `report.md`, and one JSON artifact per probe. The dispatch probe
uses a no-op custom command rather than a real model, so the suite stays local,
cheap, and deterministic. The suite also sets
`TACHI_SEARCH_DISABLE_QUERY_EMBEDDING=1` inside the sandbox so the memory probe
uses lexical search instead of external embedding calls.

## GitHub / Docs / Memory Loop (Phase 6)

This stays strategic for now; do not make it the first implementation.

```text
Issue = intent / status
Docs = contract / current truth
Tachi memory = pointer / gotcha / negative lesson
PR = the nail that binds all three
```

Tachi should eventually use GitHub as an A2A coordination surface, but first
version should not add GitHub writes, daemon scheduling, or auto-merge behavior.

## Non-Goals for the First PR

- No `AgentD` daemon.
- No GitHub webhook/polling loop.
- No automatic comment-back.
- No auto-merge.
- No broad dispatch refactor.
- No planner/Ghost card; the IDE primary assistant remains the planner.
- No large agent zoo beyond Poke / SCV / Raven plus Medic mode.
- No automatic upstream skill update or GitHub writeback.
- No acpx `flow run`, GitHub writeback, or auto-merge.
- No acpx session history as Tachi's evidence authority.

## Suggested Implementation Phases

### Phase 1: Deck/Card model + upstream source manifest

- Add this vocabulary spec.
- Add static Card definitions for Poke, SCV, Raven, and Medic mode metadata.
- Add `skill/superpowers/manifest.yaml` and `skill/waza/manifest.yaml` with
  upstream repo/path/ref/sha mappings.
- Extend the existing `mbit_card` JSON view with `authority`, `guidance`,
  `moves`, and `personality` fields.
- Add a read-only `tachi card list` / `tachi card show` CLI convenience that
  calls the existing facade.
- Optionally add a read-only `tachi skill-sources status` report.

### Phase 2: Poke local smoke suite

- Implement `tachi poke run --suite smoke`.
- Produce report JSON + Markdown.
- Keep probes local and isolated.
- Do not run broad Rust gates by default.

### Phase 3: acpx execution backend skeleton

- Add an opt-in acpx execution backend through `harness_transport="acpx"`.
- Build acpx commands from dispatch params and Card authority in one adapter.
- Run one-shot execution from Tachi-generated `prompt.md`.
- Optionally run named sessions with `TACHI_ACPX_RUN_MODE=session` and store
  status/cancel control argv in run metadata.
- Route `tachi_task(action="status" | "cancel")` through dispatch-scoped acpx
  status/cancel controls when the run used acpx session mode.
- Persist raw JSON/ACP output as `acpx_events.jsonl`.
- Map basic events into existing trajectory/progress/status/result artifacts.
- Detect missing acpx/Node prerequisites with actionable errors.
- Keep acpx flows, GitHub writes, and auto-merge out of scope.

### Phase 4: Card evaluation summary

- Aggregate recent eval/dispatch rows by card.
- Show metrics and failure patterns.
- Generate review-required evolution proposals.

### Phase 5: Skill slot intake

- Define metadata for approved external moves.
- Add scan/review-required path for discovered skills.
- Bind approved moves to cards.

### Phase 6: GitHub A2A observe-only

- Normalize issue/PR/comment/check events into local evidence.
- Detect external bot ownership and duplicate dispatch risk.
- No comment-back or dispatch by default.

## Acceptance Criteria

- [ ] Issue direction is narrowed to Tachikoma backplane + Deck/Card evaluation
      + Skill Slots + Poke probes.
- [ ] First version has only Poke / SCV / Raven cards and Medic mode.
- [ ] Card schema can represent existing dispatch profile concepts.
- [ ] Superpowers are modeled as development guidance, not just ordinary moves.
- [ ] Waza and approved external skills are modeled as moves/techniques.
- [ ] Poke smoke suite has a local isolated starter implementation.
- [ ] Card evolution produces proposals, not silent automatic mutations.
- [ ] External skill intake requires inspection/scanning/review before approval.
- [ ] Upstream Superpowers/Waza sources are tracked, pinned, and documented as
      upstream-managed corpora.
- [ ] acpx is documented as an optional execution backend, not a Card, Move, or
      evidence authority.
- [ ] acpx defaults are conservative and do not imply `--approve-all`.
- [ ] acpx raw events are additive and map into existing Tachi run artifacts.
- [ ] GitHub A2A, daemon/harness, and automatic upstream sync are explicitly
      deferred from the first PR.

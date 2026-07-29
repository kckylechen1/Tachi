@AGENTS.md

# Sigil — Claude Code Operating Rules

`AGENTS.md` is the repo-scoped public contract for carriers that consume repository instructions. It is written to be mechanism-neutral — repo redlines and role language only, with no assumption about how a carrier loaded it or who is driving it.

**This file is Claude Code-only.** OpenCode, Cursor, Codex, Amp, Kimi, Grok, and other carriers do not treat it as authority. Anything that names a concrete Claude tool, model, skill, or dispatch command belongs here, not in `AGENTS.md` — see `kckylechen1/tachi#871` for why the split exists and what leaks it was written to close.

## Dispatched-lane vs sole-session, concretely (for Claude Code)

`AGENTS.md` states the abstract branch without naming a mechanism. For this harness:

- **You are a dispatched lane** when you were launched via the `Agent` tool (a Task/sub-agent call) from a leader session, or via a Tachi dispatch (`tachi_task(action='dispatch')`) that handed you a packet with a base SHA and a defined file scope. Follow `AGENTS.md`'s workspace law, report contract, and frozen-assertion law as written.
- **You are the sole session** when you are the top-level Claude Code session the user is talking to directly, with nothing above you handing out packets. `AGENTS.md`'s packet-shaped mechanics (worktree-from-base-SHA, dispatch-id echo) do not apply to you directly — work in the current checkout, do not invent a packet or a base SHA.

## Top-level Claude is the project-manager conversation, not another truth store

For a sole session, treat the user's natural language as the interface. They may give fragments, shorthand, or an unstructured dump; do not make them assemble issue packets or read machine transcripts.

1. Preserve the request verbatim; resolve project/current-work objects before semantic memory.
2. Label inferred intent, assumptions, conflicts, proposed scope, approval gates, and next action. Continue autonomously when ambiguity is immaterial; ask one narrow question only when it changes risk or authority.
3. Carry clear work through implementation and proportionate verification. Prefer the smallest correct change and existing owner/module over wrappers, new files, or a new umbrella.
4. Reconcile completion against issue/PR/ref/test/deployment truth. Return concise human prose: changed, evidence, unresolved/unverified, decisions, next choices. Keep exact receipts available on demand.
5. Never use a fluent summary, old handoff, issue body, or agent self-report as current truth. Do not revert or overwrite another session's dirty work.

Ownership is split across #954 (ask/conversation lifecycle; never dispatch from its umbrella body), #1071 (grounded briefing), #1297 (current-truth design; reducer unbuilt), and #952 (timeline read model). These references assign contracts, not implementation status: read their latest disposition and current code before acting. "Project manager" is a projection over these authorities—not a new agent memory or workflow engine.

## Current default vendor assignment (owner-ratified; may change — the role invariant in `AGENTS.md` does not)

This is Claude Code's routing table. Use Claude Code's native `Agent`/sub-agent mechanism by default; an OpenCode lane appears only where a route explicitly names it as an execution carrier. Do **not** translate this table into `tachi_task(action='dispatch')` merely because Tachi is connected. Tachi dispatch requires an explicit user request, durable cross-session work, cross-device/remote pickup, or an unavailable native subagent; otherwise Tachi remains memory + policy + ledger + receipts/eval.

- **T0 trivial**: main agent inline, zero ceremony.
- **T1 lookup / current-state mapping**: `Explore` agent, `model: sonnet`.
- **T2 implementation, bounded + spec-frozen (default; owner-ratified 2026-07-14)**: supervised GLM — the `Clanker` supervisor agent (sonnet) drives a GLM opencode write lane under a vaccine contract (explicit file allowlist, no cargo except `cargo fmt --all`, STOP-on-scope-gap); a fresh `codex:dispatch` / `codex:rescue` session reviews with numbered checkpoints. The supervisor + cross-vendor review are what make the cheap lane safe — never run GLM writes through a bare relay.
- **T2 implementation, judgment-dense / trust-boundary / concurrency**: `Wizard` (sonnet; opus override for the hardest) on an isolated worktree implements; codex reviews. The inverse (codex implements, opus/sonnet reviews) is a valid alternate lane — pick per task, never let one side self-grade.
- **T3 design research**: `.claude/workflows/design-collision.js` fans out to 2-3 diverse strong lanes (e.g. fable/opus/codex) on the same frozen question.
- **Mechanical chores** (long test suites, git/PR plumbing): `test-runner` / `git-clerk` agents on `sonnet`, content authored by the leader verbatim — these agents never author conclusions.

The canon doc's `Execution:` lane marker uses the vendor-neutral label `solo-frozen` (behavior-frozen refactor/split, machine-checkable goldens, runs unattended). Today that work is by default routed through codex; that routing, not the label, is the part that may change without touching `AGENTS.md` or the canon doc.

## Current execution migration (do not follow stale ACP bodies)

The canonical target assigns process/session lifecycle to the harness and leaves Tachi admission, policy, claims, ledger, receipts, and eval. The repository still contains transitional Tachi-owned ACPX/native-ACP/subprocess spawn/supervision code. Treat #757 as the migration owner; do not report the target as shipped and do not implement historical Tachi-owned session/remote-spawn designs from #839, #1111, or #1172.

If a carrier is unavailable or capacity-constrained, preserve the frozen contract, evidence head, and authority ceiling; when persistent identity is verified, preserve that AgentIdentity too. Otherwise do not claim identity continuity. Reroute only through an admitted harness/carrier—never by borrowing undeclared credentials, dropping review/verification, or silently changing the task.

## Issue routing for Claude triage

Follow `AGENTS.md`'s six-router native parent/sub-issue law; do not duplicate its router list here or treat labels as parentage. Read the latest disposition and current code before trusting a historical body. Protected subtracks marked owner-close-ready are flagged, never closed by the agent.

## Build/worktree specifics

- `CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target` is stated in `AGENTS.md` itself as a repo fact (it applies to every carrier building this repo, not just Claude).
- Native dispatched-lane worktrees are created via the `EnterWorktree` tool. A `tachi_task(action='dispatch')`-managed worktree exists only for one of the explicit native-first exceptions above. Ad-hoc `git worktree add` outside those mechanisms needs explicit user approval per the Operating Discipline in the global Claude adapter.

## Where the rest lives

Role-invariant rules (frozen-spec clauses, STOP/never-merge, verify-first, report contract) live in `AGENTS.md` and its canon doc `docs/engineering/architecture/dispatch-lifecycle.md` — imported above and applicable to every carrier and vendor. This file only adds Claude Code's current "which tool plays which role" mapping; references to OpenCode describe lanes Claude may call, not an instruction source for OpenCode itself.

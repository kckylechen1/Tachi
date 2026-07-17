# Sigil — Claude/OpenCode Operating Rules

`AGENTS.md` is the public contract: every carrier that reads instruction files in this repo (Claude, Cursor, codex, or any other backend agent) reads the same text, and it is written to be mechanism-neutral — repo redlines and role language only, zero vendor names, no assumption about how the reader got here or who is driving it.

**This file is Claude/OpenCode-only.** Cursor and other carriers do not load it. Anything that names a concrete tool, model, skill, or dispatch command belongs here, not in `AGENTS.md` — see `kckylechen1/tachi#871` for why the split exists and what leaks it was written to close (Cursor once mis-read `AGENTS.md`'s dispatched-lane framing as a literal instruction and went off spawning its own child job).

## Dispatched-lane vs sole-session, concretely (for Claude Code)

`AGENTS.md` states the abstract branch without naming a mechanism. For this harness:

- **You are a dispatched lane** when you were launched via the `Agent` tool (a Task/sub-agent call) from a leader session, or via a Tachi dispatch (`tachi_task(action='dispatch')`) that handed you a packet with a base SHA and a defined file scope. Follow `AGENTS.md`'s workspace law, report contract, and frozen-assertion law as written.
- **You are the sole session** when you are the top-level Claude Code session the user is talking to directly, with nothing above you handing out packets. `AGENTS.md`'s packet-shaped mechanics (worktree-from-base-SHA, dispatch-id echo) do not apply to you directly — work in the current checkout, do not invent a packet or a base SHA.

## Current default vendor assignment (owner-ratified; may change — the role invariant in `AGENTS.md` does not)

- **T0 trivial**: main agent inline, zero ceremony.
- **T1 lookup / current-state mapping**: `Explore` agent, `model: sonnet`.
- **T2 implementation, bounded + spec-frozen (default; owner-ratified 2026-07-14)**: supervised GLM — the `Clanker` supervisor agent (sonnet) drives a GLM opencode write lane under a vaccine contract (explicit file allowlist, no cargo except `cargo fmt --all`, STOP-on-scope-gap); a fresh `codex:dispatch` / `codex:rescue` session reviews with numbered checkpoints. The supervisor + cross-vendor review are what make the cheap lane safe — never run GLM writes through a bare relay.
- **T2 implementation, judgment-dense / trust-boundary / concurrency**: `Wizard` (sonnet; opus override for the hardest) on an isolated worktree implements; codex reviews. The inverse (codex implements, opus/sonnet reviews) is a valid alternate lane — pick per task, never let one side self-grade.
- **T3 design research**: `.claude/workflows/design-collision.js` fans out to 2-3 diverse strong lanes (e.g. fable/opus/codex) on the same frozen question.
- **Mechanical chores** (long test suites, git/PR plumbing): `test-runner` / `git-clerk` agents on `sonnet`, content authored by the leader verbatim — these agents never author conclusions.

The canon doc's `Execution:` lane marker uses the vendor-neutral label `solo-frozen` (behavior-frozen refactor/split, machine-checkable goldens, runs unattended). Today that work is by default routed through codex; that routing, not the label, is the part that may change without touching `AGENTS.md` or the canon doc.

## Dispatch process (owner-ratified 2026-07-17; supersedes the in-session review loop above where they conflict)

- **PR-first**: an implementation lane's FIRST act is branch + initial commit + **draft PR**; all increments push to the PR branch continuously. The worktree is a pure cache — a dead lane loses nothing. Finishing = flipping the draft to ready.
- **No in-session cross-review, no in-session merge**: lanes open PRs and STOP; the leader flips ready and does NOT merge. Review and merging belong to the owner's designated human reviewer.
- **Oz still runs, non-blocking**: after each PR opens, the Oz seat runs compile + targeted tests asynchronously and posts the verbatim results as a PR comment for the reviewer. It gates nothing and merges nothing.
- **Dead-lane finalizer**: on any lane-death notification, the leader pushes stranded work to a `salvage/<lane>` branch, then removes the worktree. Structural home for auto-salvage: #1184 (reaper-integrated).

## Build/worktree specifics

- `CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target` is stated in `AGENTS.md` itself as a repo fact (it applies to every carrier building this repo, not just Claude).
- Dispatched-lane worktrees are created via the `EnterWorktree` tool (for a session already inside this repo) or `tachi_task(action='dispatch')`-managed worktrees. Ad-hoc `git worktree add` outside those two mechanisms needs explicit user approval per the Operating Discipline in the global Claude adapter.

## Where the rest lives

Role-invariant rules (frozen-spec clauses, STOP/never-merge, verify-first, report contract) live in `AGENTS.md` and its canon doc `docs/engineering/architecture/dispatch-lifecycle.md` — read those first; they apply to every carrier and every vendor. This file only adds the Claude/OpenCode-specific "which tool plays which role, right now" mapping so a Claude session doesn't have to infer it from `AGENTS.md`'s deliberately abstract language.

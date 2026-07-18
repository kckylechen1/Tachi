# Tachi Security Model: the OS that holds the keys and spawns the workers

Status: threat model v1, drafted 2026-07-05 at owner request. This is the roof over
pieces that already exist (#458 vault broker, #476 seat scoping, #515 front door,
ToolProfile/card authority, hub review gate, audit log). Product gate: this document
must be true before Tachi ships as a product.

## The sentence that must have an answer

Tachi stores every API key the owner has AND dispatches arbitrary-vendor agents with
broad sandboxes to act on untrusted content. A product with that shape needs an
explicit answer to "why doesn't a prompt-injected worker walk away with the vault?"

## Assets, principals, trust

**Assets** (in descending blast radius): vault secrets (API keys, GH_TOKEN) · the
machine itself (full-access sandboxes exist in practice) · GitHub write authority
(repo integrity, secret-leak-via-issue) · memory stores (may contain quoted secrets,
PII, decision history) · eval/routing state (poisoning it steers future dispatches).

**Principals** (in descending trust):
1. Owner CLI (local shell) — fully trusted; the only principal allowed to mint trust.
2. Resident daemon — trusted executor of policy; never originates authority.
3. Leader agents (Claude Code / codex / droid sessions the owner drives) — trusted to
   orchestrate, but their CONTEXT ingests untrusted content (web, repo files, PR
   comments) → injectable.
4. Dispatched workers — least trusted: arbitrary vendor models, fed task packets
   containing repo content, often with write sandboxes.
5. External MCP upstreams and vendored skill corpora — supply chain, not principals.

**The core rule: authority flows down the card, never down the prompt.** A worker's
capabilities come from its DispatchProfile card (tool profile, write scope, key
leases), decided before spawn. Nothing a worker reads at runtime can widen them.

## Threats and stances

**T1 — Injected worker exfiltrates secrets.** A worker's packet includes repo/web
content; a hostile string says "print $OPENAI_API_KEY / read the vault".
Stance (strengthened 2026-07-05, worker-class doctrine): workers never mount ANY
Tachi surface — not just vault, the entire OS control plane. Contract workers speak
a file protocol: packet in (instruction.md with pre-baked briefing), artifacts out
(result.md / run-dir drops); the pipeline does all ledger writes. Capability READS
(codegraph, context7) are card-issued and seat-scoped, ≤2-3 per flash-tier worker
(#476). Keys reach workers only as **scoped leases**: the dispatch layer injects
exactly the whitelisted env vars the card names (#458 broker's job). With no recall
surface, memory-based exfiltration probing is structurally gone.
Residual risk: a leased key is exfiltratable by the worker that legitimately holds
it → leases are per-dispatch, audit-logged, and the card whitelist keeps them minimal.

**T2 — Worker escapes its write scope.** Prompt-stated scopes ("Owns crates/foo
only") are honor-system.
Stance today: worktree isolation + concurrent-tree discipline + review gate before
merge (safe_merge/verify). Stance later: sandbox-enforced scopes (macOS sandbox
profiles / container worktrees) — tracked as a gap, not promised.
The merge gate is the real wall: nothing a worker writes reaches main without a
different-vendor review and verification evidence (#516 pipeline hardens this).

**T3 — Malicious or compromised MCP upstream.** An upstream tool's OUTPUT is
injectable content aimed at whichever agent called it; a hostile server can also
lie in its tool descriptions.
Stance: hub review gate (`review_status=approved`) stays mandatory for anything not
registered by the owner's local CLI (#515 constraint: auto-approve is CLI-local
only, MCP-surface `hub_register` never bypasses); flatten exposure uses explicit
allow/deny lists; circuit breaker + pool caps bound availability abuse. Seat scoping
(#476) narrows which agents even see an upstream.

**T4 — Supply chain via vendored skills.** Skills drive backend agents' SOPs
(injection.rs); a poisoned upstream commit is an instruction injection into every
dispatched worker.
Stance: pins + `reviewed_sync` (never auto-apply), drift detection surfaces changes
(#497); diffs are read by the adjudicator before sync.

**T5 — Secret leakage through the world-facing pipe.** Workers/leaders post to
GitHub (issues, PR bodies, comments) — the exfil channel that needs no attacker,
just carelessness.
Stance: gitleaks runs as a required verify check pre-ship (#516 seeds it); ship's
git-log-generated bodies reduce freehand prose; audit log attributes every write.

**T6 — Local-at-rest and transport.** Vault is argon2id+AES-GCM at rest (exists).
Portability bundles re-encrypt the vault under a separate sync passphrase and never
place live DBs on iCloud (see state-portability.md). Memory DBs are NOT encrypted at
rest in v1 — acknowledged gap; FileVault is the assumed floor, own-encryption is a
product decision to revisit.

**T7 — Eval/routing poisoning.** Workers self-report; a lying worker inflates its
own route.
Stance: eval rows record verdicts from the REVIEW lane (different vendor), not the
worker's self-report (review discipline is law); route changes go through proposals
with `confirm=true` human application — the scheduler learns, but the owner ratifies.

**T8 — Recursive-dispatch resource exhaustion.** A worker that itself dispatches
children (self-dispatch, or a chain of workers each dispatching the next) can
exhaust the daemon with unbounded fan-out/depth.
Stance (#1251, v1): `enforce_dispatch_depth` (`session_identity.rs`) refuses a
dispatch once the caller's depth (`resolve_dispatch_depth`, carried per-call over
`X-Tachi-Dispatch-Depth` in the daemon-proxy topology, or `TACHI_DISPATCH_DEPTH` env
in the CLI in-process path) reaches `MAX_DISPATCH_DEPTH`. This is a **caller-asserted**
gate — it stops ACCIDENTAL unbounded recursion via the normal MCP-dispatch path,
which is the only path a well-behaved worker takes. It is explicitly the SAME trust
class as `TACHI_AGENT_SEAT` self-report (T7's "workers self-report" framing applies
here too): nothing binds the depth claim to an authenticated capability, so a
DELIBERATE worker can bypass it — invoking the raw `tachi task` CLI outside the
env-stamped path, or forging the `X-Tachi-Dispatch-Depth` header on a direct HTTP
connection. That residual is accepted for v1 (owner ruling, #1251) on the same basis
the rest of this document already accepts worker self-report as real-but-bounded
exposure; a server-side capability-token binding (depth minted into a
signed/opaque token by the parent, unforgeable by the child) is tracked as a
follow-up hardening, not part of this gate.

## Invariants (freeze these; violating = BUG in any review)

1. Workers mount no Tachi surface at all (packet in, artifacts out); secrets reach
   them only as card-whitelisted, per-dispatch, audit-logged leases.
2. Trust is minted only at the owner's local CLI; no MCP-surface path may
   auto-approve capabilities, keys, or route changes.
3. Nothing merges to a default branch without different-vendor review evidence and
   verification checks (goal/* included at campaign close).
4. Every GitHub write and every key lease is attributable in the audit log.
5. Review/verify gates fail CLOSED (missing/stale/unparseable evidence = blocked).
6. Open-web read and write authority never coexist on one worker: research
   retrievers read the world and write nothing; implementers hold the pen and read
   only the closed world (local index + curated docs) — the injected-instruction →
   malicious-patch path is severed structurally.

## Known gaps (tracked, not hidden)

- ToolProfile is facade-granular (#495) — delegate deny-by-tool is coarse; the
  action-level filtering fix is part of the security story, not just ergonomics.
- Write scopes are honor-system pending sandbox enforcement (T2).
- Memory-at-rest encryption undecided (T6).
- Key-lease mechanism (#458) is designed but not fully landed — until then, workers
  inherit whatever env the spawning harness passes: the current REAL exposure.
- Recursive-dispatch depth gate (T8, #1251) is caller-asserted, not capability-bound
  — a deliberate worker can bypass it via raw CLI or a forged header. Accepted for
  v1 (matches the existing worker self-report boundary); capability-token binding
  is the follow-up.

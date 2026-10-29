# AgentIdentity → WorkClaim holder-evidence spine v1

Status: frozen implementation contract for #1253
Design owners: #1171, #1239, #894
Depends on: #1170 only for a future `verified` admission adapter

## Purpose

This document defines the executable v1 relationship between a connecting
agent, the work it claims, and an execution environment. It is deliberately
one narrow spine: it makes those relationships durable and observable without
making an execution environment the owner of identity, claims, GitHub state,
or adjudication.

## Terms and authority

| Term | Stable key | Owns | Does not own |
| --- | --- | --- | --- |
| AgentIdentity | `agent_identity_id` | agent seat/capability and admission history | a particular connection or work item |
| Session | `session_id` | protocol session lifetime | stable agent identity |
| Connection | `connection_id` | one transport admission | a session or work claim |
| WorkClaim | `claim_id` | intent, role, lease, conflict semantics, and links to work artifacts | identity admission or ExecEnv lifecycle |
| ExecEnv | `exec_env_id` | an environment lease and its local destructive-reconciliation decision | identity or WorkClaim transitions |
| GitHub | issue/PR ids | live issue and PR state | cached terminal state in this service |

`agent_identity_id`, `session_id`, and `connection_id` are different keys.
The server generates `connection_id`; a reconnect may retain an identity while
receiving a new connection and, where protocol semantics require, a new
session.

## Admission

The stored admission state is exactly one of:

- `self_asserted`: a local stdio or loopback assertion accepted as local
  attribution, not remote proof.
- `verified`: only a trusted verification adapter may write this state.
- `rejected`: assertion is malformed or contradicts the authenticated
  transport evidence.
- `unavailable`: no adapter can prove the asserted remote identity.

There is no public request field that can set `verified`. Until #1170 provides
device proof, remote admission is `unavailable`; the absence of proof is not a
reason to manufacture a local identity or to label it verified. Display names
are presentation data only.

AgentIdentity owns seat/capability. WorkClaim owns its per-work role. Presence
of a connection and freshness of a WorkClaim lease are separate signals.

## WorkClaim ledger

The existing `session_claims` physical table is evolved in place; its Rust
domain name and public APIs are `WorkClaim`. It remains the durable ledger for
the fields actually shipped in v1:

```text
AgentIdentity → WorkClaim → Issue / flow / ExecEnv / worktree
```

Every new WorkClaim records its claimant identity, role, mode (`read_only` or
`writable`), declared file scope, expected HEAD, caller-declared lease metadata,
a monotonic transition version, and any ExecEnv/orphan binding. The
`lease_expires_at` value is advisory and reserved for future direct-expiry
enforcement; v1 orphaning uses `heartbeat_at` plus the configured GC TTL. Legacy
rows are identity-unavailable: migration must not invent an
`agent_identity_id` for them.

States are `active`, `orphaned`, and `released`.

- GC marks an `active` claim `orphaned` when its `heartbeat_at` is older than
  the configured claim TTL.
- An orphan blocks silent takeover.
- Only explicit, versioned release or handoff changes ownership.
- A failed compare-and-swap transition reports conflict rather than silently
  selecting a winner.

Same-issue claims are not inherently exclusive. Read-only claims and disjoint
writable claims may coexist. The server rejects overlapping writable tree
paths, overlapping writable scopes, and incompatible expected HEAD values
loudly. Existing advisory auto-hooks can remain best-effort only; the
authoritative claim API never swallows persistence failure.

## Local canonical project ownership

The local conservative policy adds nullable `canonical_project` to the existing
WorkClaim table. Only an explicitly requested claim by an already admitted local
identity receives the server's canonical bound project. Caller project, repo,
worktree and runtime-routing receipt strings do not establish this authority.
Existing claims, legacy presence hooks and remote admissions without project
scope stay unbound; transitions cannot rebind a claim's project.

A project read additionally requires that the current identity hold an active
canonical claim in that project with an unexpired lease. Identity admission plus
routing alone does not qualify. This read eligibility uses lease expiry without
changing v1 heartbeat/GC state transitions or inventing a new grant ledger.
Released, orphaned or expired last claims make scoped reads unavailable; their
historical data is not deleted by that decision. Explicit versioned handoff and
release retain their existing transition rules.

The creation sequence is explicit `claim(flow_id)` followed by `intake(flow_id)`.
Intake requires one current writable holder, takes the admission/claim SQLite
writer boundary, creates a new run directory, and atomically writes its immutable
ownership seal before private payload. It never adopts an existing unsealed
folder. Directory descriptors, no-follow IO and the status mutation fence keep
replacement directories from inheriting authority. A partial creation is
incomplete/unavailable. Claim GC retains its existing policy; after the referenced
claim is deleted, the remaining seal alone grants no access or re-adoption.

Project board/status/brief use the existing WorkClaim source, projection index
and views. They filter canonical rows before opening flow receipts and share a
semantic revision including state, heartbeat and admitted flow facts, not merely
`transition_version` (GC can orphan without bumping it). Missing CurrentTruth,
run, verification, adjudication and delivery adapters remain unavailable in this
project slice; legacy exact-ref CurrentTruth reads preserve their retired-writer
posture. They never backfill project ownership.

Only claimed intake and scoped reads support new sealed flows. Staff/direct
launch, completion/adjudication, verification and progress producers reject them
before their normal mutations; this slice adds no execution permission. Legacy
read paths classify ownership metadata before loading payload and omit sealed
receipts, including after claim GC. The advisory global filesystem scan retains
its physical inspection cap and honest incomplete posture: a denied physical
entry may displace a later legacy row within that budget. It does not promise
project-level noninterference. New scoped views never fall back to that scan.
The legacy event-before-status recovery order is retained, including an existing
event file in a read-only unsealed root; the fixed recovery exception creates
neither a root lock nor an event file and cannot write a seal.

The fixed legacy `verification.json` advisory remains readable without a status
receipt; no other artifact gets this exception. A present malformed or sealed
status always refuses it, and its caller-asserted/unverified posture is unchanged.
New sealing rejects concurrent legacy entries before writing private bytes.
Bound intake validates the explicit claim before upstream issue reads and checks
the same captured identity again at sealing. Its plan reports execution integration
unavailable, even when the unchanged legacy planner would recommend dispatch.
Scoped snapshots stop before receipt IO above 64 project claims or 8 selected
receipts. Receipts are capped at 1 MiB each; metadata plus payload reads total at
most 16 MiB plus 16 overflow-probe bytes. Over-limit results are explicitly
bounded/incomplete. Selectors do not alter the canonical WorkClaim source or each
work revision; selected flow receipt revisions are reported separately.
Run-state filters are unavailable in scoped views until the run-receipt source
exists; a zero limit yields no rows or flow references.

Descriptor-bound ownership IO requires Unix; new sealing currently supports
macOS and Linux and fails closed elsewhere. Windows retains plain unsealed
legacy IO through no-reparse handles, same-handle bounded reads and mandatory
mutation fences. Directory sharing guards replacement of the directory itself;
child contents need their own retained handles. Existing descriptor-based board
reads remain unsupported on Windows. Unknown non-Unix platforms stay unavailable.
Tool field inventories and profile permissions are unchanged. Active-lease-dependent reads
are not a durable operation/result read binding: reconnect and post-completion
Tasks retention/authorization require a separate frozen contract, not an upgrade
of expired claims or a second handle ledger in this slice.

## ExecEnv holder evidence

ExecEnv stores only opaque `agent_identity_id` and `claim_id` links. WorkClaim
performs their atomic binding and is the only component that transitions claim
state. On a destructive close or reclaim, the cleaner queries holder evidence:

| Evidence | Meaning | Destructive action |
| --- | --- | --- |
| `clear` | explicitly released bound claim | may continue through existing safety gates |
| `not_applicable` | legacy unbound environment | may continue through existing safety gates |
| `held` | active or orphaned claim | refuse |
| `contradictory` | links do not agree | refuse |
| `unavailable` | required ledger read failed | refuse |
| `unverifiable` | ledger cannot establish a safe answer | refuse |

Refusal is observable to the caller. It must not appear as a successful no-op.
ExecEnv never creates, releases, or hands off an identity or claim.

## Lifecycle and board

The canonical `tachi_task` surface exposes `claim`, `release`, `heartbeat`,
`handoff`, and `board`. The retired `tachi_memory` claim/release tokens are
typed-rejected; callers use `tachi_task` for the WorkClaim lifecycle. Board
composition presents independent facts. It
reads GitHub live where available; otherwise it reports
`github_state=unavailable` and never infers a terminal state from a cache.

PR, outcome, and adjudication references remain in their existing lifecycle
ledgers; v1 does not add those columns to `session_claims`.

## Migration and compatibility

Migration v21 is additive. It adds identity/admission records, augments
`session_claims`, and augments `exec_envs`. Older binaries ignore the added
fields; reverting code does not delete durable rows. The former Memory
claim/release aliases are retired in #1688; historical rows remain readable,
and the additive schema remains backward-readable.

## Required proof

Tests must be RED against the v20 base and GREEN after implementation for:

1. stale GC changing an active claim to `released` rather than `orphaned`;
2. same-issue collision treating all roles/modes as equivalent;
3. authoritative claim persistence failure silently degrading;
4. destructive cleanup ignoring claim holder evidence.

Additional tests cover identity reconnect/admission, migration preservation,
CAS handoff/release, conflict classes, atomic binding rollback, live-GitHub
unavailability, and each holder-evidence branch.

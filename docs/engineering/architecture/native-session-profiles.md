# Native Session profile boundary

Staff resolves an admitted profile before creating a managed worktree or run.
The profile fixes its provider, requested model, transport, permissions, credential
policy and turn limits. A worker selector may repeat that provider; it cannot
replace it. Worker-only starts and retired profile names return a repair message.
Stage is fixed to review or execute by the profile; the legacy V2 planner toggle
does not insert a separate provider call before native Session startup. The
separately admitted `dsh_executor` headless path remains distinct.

## Requested configuration

| Profile | Provider | Requested model | Requested effort | Workspace writes | Turns | Seconds per turn |
| --- | --- | --- | --- | --- | --- | --- |
| `codex_sol_review` | Codex | `gpt-6.1-sol` | `high` | No | 4 | 300 |
| `codex_astra_review` | Codex | `gpt-6-astra` | `xhigh` | No | 4 | 600 |
| `codex_terra_write` | Codex | `gpt-6.1-terra` | `high` | Managed worktree | 8 | 600 |
| `opencode_sol_review` | OpenCode | `openai/gpt-6.1-sol` | Unspecified | No | 4 | 300 |
| `opencode_astra_review` | OpenCode | `openai/gpt-6-astra` | Unspecified | No | 4 | 600 |
| `opencode_terra_write` | OpenCode | `openai/gpt-6.1-terra` | Unspecified | Managed worktree | 8 | 600 |

All six profiles use supervised native ACP Session transport. Each run also has a
fixed total lifetime of `turn ceiling × seconds per turn`, including time spent
waiting for supervision; each active turn independently has the listed timeout. These are requested
pins, not assertions about adapter installation, account access or model
availability. OpenCode has no claimed effort control in these profiles. Its
provider-qualified model ID remains exact and case-sensitive.

The connection must select the exact advertised model and, when specified, effort
option; receive the matching configuration acknowledgement; and only then prompt.
No fallback, prefix rewriting or inferred model observation is permitted. Requested,
planned and observed identity stay separate in the existing dispatch receipt.
Every in-turn configuration update must still match the fixed profile before any
later tool approval or result; restoring a mismatched setting cannot erase the failure.
The resolved profile contract is frozen in the run's native metadata. Native
profiles use the fixed `server_confined_typed_tools` permission policy; the legacy
`TACHI_ACP_APPROVAL_MODE` toggle affects only non-managed legacy sessions.

The retired `codex_53_fast`, `codex_55_review` and `opencode_builder` names never
silently route to a new model. Their errors name a current profile for explicit
selection. Unrelated operator-only legacy dispatch entries are not promoted into
native Staff profiles.

## Adapter and host discovery

Tachi drives the locally installed harness binary through that binary's own
session protocol; it never bundles or downloads a second copy. Each profile
records its `dialect`:

| Backend | Command | Dialect |
| --- | --- | --- |
| Codex | `codex app-server` | `codex_app_server` (JSON-RPC without a `jsonrpc` field; thread/turn model) |
| OpenCode | `opencode acp` | `acp` (ACP v1) |

Operators may select an installed trusted executable through
`TACHI_CODEX_APP_SERVER_COMMAND` or `TACHI_OPENCODE_ACP_COMMAND`; the matching
`_ARGS` settings contain a JSON array of argument strings. A managed run whose
profile names no supported dialect fails before spawn. No runtime package
installation, legacy global ACP command override or serve-to-CLI fallback
participates in these profiles.

The supervisor (turn ceilings, mailbox intake, projections, artifacts) is shared;
a dialect driver owns the wire protocol and identity evidence. Codex pins the
model and effort at `thread/start` (`model`, `config.model_reasoning_effort`,
`sandbox=danger-full-access`, `approvalPolicy=never`, run-private
`config.mcp_servers`) and fails unless the local roster offers the model and the
response echoes both. A turn ends only at `turn/completed`; a non-`completed`
status, `model/rerouted`, or any identity drift fails the run without retry.
Server-to-client requests are refused fail-closed with a receipt (approvals are
declined). Codex has no in-turn effort notification, so effort is evidenced once,
at thread start.

The public Staff profile list filters profiles whose adapter or host filesystem
boundary is unavailable. Advisory recommendations and shared briefings retain
operator-only legacy profiles, including `claude_plan`; native candidates still
require adapter and host availability. Advisory candidates, ledger decisions and
persisted recommendations use the same risk-gated available set. An advisory
recommendation never grants native Staff admission. Preflight reports discovery and the bounded
confinement probe separately from unknown authentication, negotiated model support,
tool execution and cancellation evidence. Preflight never grants a launch.

## Filesystem and credential policy

An admitted native run has a server-owned managed worktree and a distinct
`<run>/runtime` directory. Supervised native writes use a whole-worktree boundary; a narrower explicit
`declared_file_scope` is refused until scoped postflight can complete before each
public turn result. The run receipt, status and control artifacts remain
outside the writable runtime. Both read and write profiles may write private
runtime files; only write profiles may write the managed worktree.

On Linux, bubblewrap mounts the host filesystem read-only, creates private PID,
IPC and UTS namespaces, drops capabilities and prevents nested user namespaces.
The worktree bind comes from the child's already-anchored current directory.
Writable runtime and worktree trees reject shared hard-linked regular files to
prevent writes through an inode also reachable outside the grant. Symlinks do not
expand the writable mount boundary. A bounded probe must succeed; an installed
binary alone is insufficient.

On macOS, sandbox-exec denies filesystem writes except the canonical runtime and
the optional managed worktree, and denies process-group control escape. Before
exec the runner closes inheritance of non-stdio descriptors after applying its
anchored-directory hooks. Linux uses close_range with CLOEXEC where supported,
or enumerates the actual descriptors through procfs on older kernels. macOS
uses a bounded proc_pidinfo descriptor snapshot and refuses a full 4096-entry
buffer rather than accept possible truncation. Both paths cover descriptors
above a subsequently lowered hard limit and preserve the spawn-error pipe
until exec. The macOS path still requires the repository's real owner-Mac
verification gate.

This boundary confines filesystem writes. Network access and host file reads are
not isolated. Git metadata outside the managed worktree remains read-only; workers
must report changes for the leader's integration path rather than gaining write
access to the original repository through a shared Git directory.

`HOME`, temporary files and XDG config/cache/data directories are private to the
run. The credential policy is `provider_auth_snapshot`: the server requires and copies only
the selected provider's admitted authentication file, with bounded owner-only
materialization. It does not inherit host configuration, history, MCP settings or
a shared writable `CODEX_HOME`. Environment credentials require explicit
admission. A missing local account snapshot fails before adapter spawn. Present files do not
prove valid credentials; rejected or revoked credentials remain authentication
failures during adapter negotiation. Snapshot
cleanup is an explicit terminal obligation; failure prevents a clean cancellation
confirmation and remains visible in the canonical receipt.

OpenCode v2 keeps credentials in `opencode/opencode.db` (`credential` table). When
that store exists it is authoritative over a leftover `auth.json`: the server opens
it read-only, selects the single active row whose `integration_id` equals the
profile model's provider prefix (`openai` for `openai/gpt-6.1-sol`), and writes only
that credential in the legacy `auth.json` shape, which OpenCode imports into the
run-private store. Several active rows, an unknown credential type, or an OAuth
access token that expires within the run lifetime plus ten minutes fail before
spawn; the last avoids a sandbox refresh rotating the host's refresh token. Cleanup
removes both the snapshot and the imported `opencode.db` with its WAL/SHM files.

## Supervision contract

Use `tachi_staff(start)` with an explicit admitted profile and a durable
`submission_id` when a lost response must be recoverable. Read `status` and use its
`status_revision` for `continue` or `finish`. These controls accept only the
original owner on the owning daemon while `native_session.phase` is
`awaiting_instruction`. `continue` accepts the next instruction in `task`;
`finish` accepts no task and closes the same session. A fourth review turn or
eighth write turn finishes automatically. Cancellation is valid during a running
turn or at a boundary and uses the existing owned-child termination finalizer.

The launch-bound mailbox recipient is `staff:<dispatch_id>`. Its existing global
mailbox admission is scoped to the bound project. Intake occurs only between
terminal turns and counts against the same turn ceiling. The consumed mailbox
receipt proves runner intake for a next-turn prompt, not execution or model comprehension. Incoming
text is advisory and grants no new filesystem, profile or dispatch authority.

A stale revision can be re-read and adjudicated. A failed instruction whose
outcome is unknown must not be automatically replayed. Restart exposes the
original durable run with unavailable control; it never recreates a session or
signals a process from persisted PID fields.

`events.jsonl` retains the owner-only raw protocol stream. `telemetry.json` is an
atomic projection of a canonical status revision, not a second lifecycle store.
`result.md` retains the complete terminal response; the existing MCP result
reader still refuses files above its 64 KiB transport limit. Required publication
or credential-cleanup failure remains a failed run, not accepted work.

## Lifecycle witness disposition

The numbered obligations in #1937 map to the native implementation as follows.
The obsolete independent CLI/acpx canary is replaced; its safety obligations
remain. Test names describe conformance responsibilities, not authenticated
provider or owner-Mac acceptance.

| #1937 witness | Disposition | Native counterpart |
| --- | --- | --- |
| 1. Semantic input and server policy | Retained | Fixed profile/start admission, rejected backend and stage overrides, server-owned adapter/auth/cwd, typed permission policy and actual filesystem confinement. |
| 2. One canonical identity, revision and receipt | Retained | Native turns extend `ManagedRunControlRegistry`; submission recovery reuses the existing MCP Tasks binding. Deterministic HTTP peers and retained kernel fixtures reach the canonical launcher. |
| 3. Execution evidence is not acceptance | Retained | Exact model acknowledgement, terminal stop reason and required nonempty result; raw events, atomic projection and full result; no synthesized exit zero or semantic review verdict. |
| 4. Confirmed termination before terminal cancel | Replaced with native equivalent | Protocol cancel followed by owned process termination proof and credential cleanup; native HTTP cancellation plus retained kernel race/failure discriminators. A status-only mutation cannot confirm. |
| 5. Truthful controller loss | Retained | Restart test retains run identity while control is unavailable; no automatic resume-to-new, retry or persisted-PID signaling. |
| 6. Pre-spawn refusal and bound provenance | Retained | Missing adapter/confinement is refused; absent local auth snapshot produces zero adapter spawn. Existing request, assignment/grant, WorkClaim, host and artifact receipts remain canonical. Present auth is not a claim of account validity. |
| 7. Exact real candidate evidence | Retained; real-host gate pending | Each supported cohort must record candidate SHA, OS/architecture, actual adapter/version, account/model acknowledgement, correction, finish, cancellation and cleanup. Deterministic peer conformance does not satisfy this gate. |
| 8. Independent attached sessions | Retained | External harness roster visibility and mailbox access do not install a control entry. Host-owned sessions remain host-owned. |

Phase 5 requires the separately agreed authenticated real-run cohort and explicit
owner retirement decision. Source implementation and simulated adapter tests do
not authorize deployment, legacy controller shutdown, or closing #2252/#1937.

# HTTP Direct-Connect Cookbook (#732)

> **Status:** current capability + migration guide (2026-11-04).
> **Parent:** [#732](https://github.com/kckylechen1/tachi/issues/732) under [#746](https://github.com/kckylechen1/tachi/issues/746).  
> **Companion:** [`library-identity-runtime.md`](./library-identity-runtime.md).

## Goal

Capable MCP clients speak **Streamable HTTP** straight to the resident daemon:

```text
http://127.0.0.1:6919/mcp
```

First-party Tachi MCP serves only over Streamable HTTP. Generated configs and ACP attachments require native HTTP support. Bare `tachi`, `tachi serve`, and explicit stdio selections refuse before startup or configuration writes; set `--daemon` to start the service. The former proxy/debug escape hatch does not restore stdio. Third-party Hub MCP transports, ACP harness subprocess pipes and the standalone portable-server retain their separate stdio paths.

Target shape for Claude Code / Codex-class hosts:

```text
1 tachi daemon process
0 mandatory per-session tachi serve subprocesses
```

## Resident service and stable endpoint

Run the shared service under the existing host supervisor on an explicitly
chosen fixed port, for example:

```sh
tachi --daemon --no-project-db --port 6919 serve
```

The daemon now stays resident by default (`TACHI_DAEMON_IDLE_TIMEOUT_SECS=0`).
A positive value explicitly opts into idle shutdown. Native HTTP clients do
not start a compatibility process to wake a stopped service: the supervisor
owns restart, and clients reconnect to the same endpoint. `--no-project-db`
keeps the listener independent of its launch directory; each client declares
its own workspace or project.

The `6919` above is an operator choice, not client-side discovery. Persistent
client files must use a fixed endpoint whose port the supervisor preserves.
Do not copy an automatically allocated `--port 0` URL into a permanent config:
it can change after restart. Temporary Staff configs use the actual listener
URL published after bind, or the existing discovery receipt scoped to the same
global DB. A missing daemon fails before injection rather than inventing a
port or creating another database owner. A task interrupted by an ephemeral
listener restart needs fresh admission/config; it must not replay a possibly
committed write.

## Generate a client configuration

With the supervised daemon running, preview a workspace-bound Claude Code
configuration from the repository:

```sh
python3 scripts/setup_agent_mcp.py --agent claude-code \
  --url http://127.0.0.1:6919/mcp --workspace-root /absolute/workspace
```

The installer defaults to Streamable HTTP, project scope and a dry run. Review
the displayed configuration, then repeat the command with `--apply` to write
it. `--config-scope user` does not implicitly bind the user-wide configuration
to the installation command's current directory; supply a workspace only when
that persistent binding is intentional. `--transport stdio` is rejected. Hosts without a verified HTTP schema are skipped or refused; there is no transport fallback.

## Staff and host-owned ACP context

When a resolved Staff profile requests Tachi MCP injection, the generated entry
contains `type: "http"`, the published `url`, and a `headers` map. Its default
tool profile is `delegate`. The headers carry the caller's bound canonical
project, a separate child seat as `X-Tachi-Client`, and the parent's dispatch
depth plus one. They never derive a project from the daemon's cwd. An explicit
Staff project must resolve to the caller's bound store before a child config
is created; an unbound caller must bind its workspace first. Worktree paths do
not create a new logical project.

A seat is an attribution label, not an AgentIdentity. Dispatch does not invent
a principal from the dispatch id or reuse the parent's principal. Host-owned
ACP attachment carries its separately authorized `agent_identity_id` as
`X-Tachi-Agent-Identity`. The private internal proxy capability never enters
either public config. Privileged HTTP profiles such as `ops` and `admin` remain
refused; changing transport does not grant authorization.

The actual host launch paths consume the generated config:

| Host path | Delivery |
|---|---|
| Claude Code / Grok | Existing `--mcp-config` JSON file, now with the HTTP Tachi entry |
| Codex | New MCP entries use per-run names through `-c` overrides; an existing same-name entry causes an explicit refusal until its complete policy can be preserved. HTTP header values travel through `env_http_headers` and child environment variables. Existing sandbox flags remain intact |
| Native ACP | ACP descriptor array on `session/new` or `session/load`, after `initialize` advertises `agentCapabilities.mcpCapabilities.http=true` |
| Unsupported backend | Explicit injection is rejected rather than silently omitted |

An injected native ACP session uses `session/load` on reconnect so the adapter
receives current project/seat headers. A resume-only path is not assumed to
apply replacement MCP configuration. A profile with injection disabled remains
disabled. Third-party Hub transports retain their definitions; they cannot
replace the first-party `tachi` entry.

Codex recursively merges configuration tables. Before injection Tachi runs the
read-only `codex mcp list --json` command after the admitted per-run credential
environment and Cargo target policy have been applied. Command construction
does not run the probe. Explicit environment overrides and removals are copied
to the probe, while both commands retain the server's inherited-env contract.
Managed workers and probes use the same already-open workspace directory;
neither reopens a replaceable lease pathname to choose its cwd.

For an unmanaged `-C` directory that differs from the worker's launch directory,
the probe must enter that configuration directory. Relative or empty `PATH`
entries, or nonempty relative `HOME`/`CODEX_HOME`, are refused in that combination:
use absolute values or a managed workspace. Tachi does not silently change the
worker executable or reinterpret a relative configuration source. A refusal
after materialization cleans only the current run's ephemeral credentials and
records a failed task before flow dispatch-slot reservation or worker start.

The 15-second probe must succeed;
otherwise requested injection fails with repair guidance. The inventory does
not expose the complete `enabled_tools`, `disabled_tools`, `tools` and approval
policy for an existing server. Tachi therefore refuses injection when the
same logical name already exists, whether that entry uses stdio or HTTP and
whether it is enabled or disabled. It never disables that entry and creates a
new alias to bypass its policy. A Staff profile with MCP injection explicitly
disabled can continue using the existing Codex configuration; replacement
requires an explicit configuration and policy migration.

When there is no same-name entry, each injected server receives a
`tachi_dispatch_<UUID>_<index>` name for the run. The user's files,
`CODEX_HOME`, authentication and unrelated MCP servers stay in the normal
Codex configuration layers. The probe
does not start an agent or an MCP server, though Codex may inspect HTTP OAuth
availability while listing authentication status.

This injection probe is supported on Unix, where Tachi gives it an owned
process group and keeps its leader unreaped until cleanup finishes. It uses
bounded nonblocking output reads and terminates ordinary wrapper descendants
on timeout, failure, or a pipe that remains open after the wrapper exits.
This does not contain descendants that deliberately leave the process group.
On other platforms requested Codex MCP injection is refused because no
supported probe ownership boundary is available. A Staff profile with
injection disabled continues to use its existing Codex configuration without
running this probe.

HTTP header values are kept out of command-line arguments. A stdio descriptor's
literal environment values can be projected as Codex `env_vars` only when the
same values already exist in the admitted worker environment. An explicit
environment removal is not replaced by the daemon's value. Otherwise injection refuses
with an error; it does not change the carrier's authentication environment or
put those values into command-line arguments.

For host-owned attachment, `attach_session` and `get_attachment` default to
`mcp_transport: "http"` and require `mcp_http_capable: true`, supplied by the
host from the actual ACP initialize result. This boolean reports transport
support, not identity authorization. An agent without HTTP capability cannot receive a Tachi MCP attachment. The HTTP descriptor carries the canonical project, profile, seat, authorized identity and dispatch depth. Descriptors remain digest-checked on replay; changes to the
endpoint, scope or policy refuse instead of silently rewriting an attachment.
`get_attachment` redacts header/environment values in its projection.

Attachments admitted with the earlier unpinned stdio descriptor retain their
original immutable digest. They cannot be projected as HTTP in place. Keep the original receipt for audit. The host
must create a new ACP remote session and call `attach_session` with a fresh
`remote_session_id` and `idempotency_key` under current admission, declaring
HTTP capability. Explicit stdio selections refuse without rewriting the historical receipt.

The schemas follow the [ACP transport capability contract](https://agentclientprotocol.com/protocol/v1/session-setup),
[Codex MCP configuration](https://developers.openai.com/codex/mcp/), and
[Claude Code HTTP MCP configuration](https://code.claude.com/docs/en/mcp).

## Landed substrate (do not re-implement)

| Piece | Where |
|---|---|
| Loopback HTTP listener + `/mcp` | `bootstrap/serve/daemon.rs` (`127.0.0.1`) |
| `/health` (status, transport, auth posture, reconnect) | same |
| Legacy-only session identity at `initialize` | headers `x-tachi-*` **or** meta `tachi*` |
| Profile filter (admin refused without #495 policy) | `server_handler::parse_http_tool_profile` |
| Read/write asymmetry | `session_identity` |
| E2E tests | `bootstrap/serve/http_tests.rs` (`http_direct_connect_*`) |

## Protocol compatibility (RMCP 3.8, #1891)

The HTTP daemon implements two explicit peer modes. Legacy peers
retain `initialize`, `notifications/initialized`, and HTTP session behavior
through `2025-11-25`; legacy tool responses omit `resultType`. Modern peers use
RMCP 3.8's `2026-07-28` `server/discover` lifecycle, typed capabilities,
per-request client metadata, `resultType`, and stateless HTTP routing. A peer
that explicitly sends `2026-07-28` through the removed `initialize` lifecycle
gets `UNSUPPORTED_PROTOCOL_VERSION` rather than a successful downgrade to
`2025-11-25`. The portable server remains intentionally legacy-only and fails
the same modern initialize explicitly.

Modern HTTP requests carry matching `Mcp-Protocol-Version`, `Mcp-Method`, and,
where applicable, `Mcp-Name` / `Mcp-Param-*` routing headers. RMCP rejects
missing or conflicting standard headers and body metadata before handler
dispatch. Tachi additionally rejects conflicts between an `X-Tachi-*` identity
header and its request `_meta` twin before tool dispatch. Modern identity and
admission are applied to a request-local server clone; they never replace the
legacy session binding or become protocol-session authority. Inline metadata
that selects a legacy version is still rejected because it cannot bypass the
legacy initialize/session adapter. Modern `clientInfo` is optional, but when
present it must be a valid typed MCP `Implementation`; malformed values fail
before discovery or tool dispatch. Validation decodes the current request
metadata directly and never substitutes an initialized peer's `clientInfo`.

RMCP 3.x delivers wire initialize `_meta` through `RequestContext.meta`.
The adapters read it there, retaining typed initialize params only for direct
in-process calls. Existing header precedence, project binding, self-asserted
identity, profile filtering and retry rules remain in force.

Unix and local NTFS Windows daemons also expose a bounded MCP Tasks consumer for an existing run's
result, described below. Protocol negotiation alone grants no execution
authority. The compatibility adapter requires no database migration or live
configuration change; rollback is reverting it before deployment.

## MCP Tasks: wait for an existing run's result

The retained consumer is `tachi mcp task`, using direct loopback HTTP and the
`2026-07-28` Tasks extension. First-party stdio forwarding is retired.
It is available on Unix and local Windows NTFS and applies to
`tachi_staff(action="result", format="json")`. It leaves Staff's start response
and the existing dispatch lifecycle intact. It does not launch a daemon or a
worker; the resident daemon must already be listening on the selected port.

Start a run through the existing Staff path with an admitted AgentIdentity and
a bound named project. The start owner persists that principal, canonical
project and verified project-source identity beside the canonical run receipt
before execution is published. A Tasks request must pass the current profile,
identity and project admission checks and match that durable owner. Historical
runs without this binding remain available through their existing Staff APIs,
but cannot acquire a Tasks handle by asserting ownership later. `clientInfo` and
the client label never substitute for the admitted principal.

### CLI consumer

Use the same AgentIdentity and named project that started the run. The optional
`--port` defaults to `6919`; the endpoint is always `127.0.0.1`.

```sh
tachi mcp task --project PROJECT --agent-identity AGENT_ID result DISPATCH_ID
tachi mcp task --project PROJECT --agent-identity AGENT_ID get TASK_ID
tachi mcp task --project PROJECT --agent-identity AGENT_ID cancel TASK_ID
tachi mcp task --project PROJECT --agent-identity AGENT_ID --port 6919 \
  update TASK_ID --input-responses '{}'
```

Each command sends one request with the `coordinate` profile and matching
Tachi headers and metadata, then prints the complete JSON-RPC envelope. It
handles JSON and SSE response envelopes and reports protocol errors with a
failing exit status. Polling is explicit: use the returned `taskId` with `get`
after the advertised `pollIntervalMs`. A completed run can return a normal
`resultType: "complete"` tool response immediately instead of a task handle.

### Wire and lifecycle boundaries

Modern `server/discover` advertises
`capabilities.extensions["io.modelcontextprotocol/tasks"] = {}` on supported platforms.
Legacy `initialize` omits this extension. Every Tasks-capable request supplies
the following protocol metadata alongside its Tachi identity metadata:

```json
{
  "_meta": {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientCapabilities": {
      "extensions": {"io.modelcontextprotocol/tasks": {}}
    },
    "tachiProject": "PROJECT",
    "tachiAgentIdentity": "AGENT_ID",
    "tachiProfile": "coordinate"
  }
}
```

`Mcp-Protocol-Version` matches that version and `Mcp-Method` matches the JSON-RPC
method. `Mcp-Name` is `tachi_staff` for the initial `tools/call`, then the exact
`taskId` for `tasks/get`, `tasks/update` and `tasks/cancel`. The initial tool
arguments are `action`, `dispatch_id` and `format`; no task-creation preference
or caller-supplied revision is needed.

| Operation | Result and meaning |
|---|---|
| `tools/call` with the Tasks capability | A pending owned run returns flat `resultType: "task"`, `taskId`, `status`, timestamps, TTL and polling interval. It does not create another run. |
| `tasks/get` with `taskId` | Flat `resultType: "complete"` plus the task state. A finished result request includes the canonical Staff tool response in `result`; business failure does not itself mean the protocol request failed. |
| `tasks/update` with `taskId` and `inputResponses` | Empty `resultType: "complete"` acknowledgement. A valid outstanding Native ACP permission reply is recorded and delivered once to its current owner. Unknown, duplicate and superseded response keys produce no writes or resume. |
| `tasks/cancel` with `taskId` | Empty acknowledgement after the existing managed controller accepts cancellation, or an error when control is unavailable. Poll `get` for the terminal state; acknowledgement alone does not prove execution stopped. |
| Tool calls without the Tasks capability | The existing normal tool result; legacy peers also retain their original response shape without `resultType`. |

Task state is projected from the canonical run receipt, with a durable result
binding in the same run directory. Repeated result requests reuse the current
handle; final task results are retained without restarting work or rewriting
the dispatch outcome. The handle has a bounded TTL. Run completion, artifact
acceptance, merge and deployment remain separate facts.

### Reconnect and Windows

A modern HTTP peer reconnects through the stable endpoint and reuses durable Task handles with the same admitted identity, project, profile and client capabilities. Capability discovery never downgrades Tasks into legacy execution. A failed request after dispatch is not replayed automatically; the operation may already have committed. The portable server does not expose this Tasks consumer.

Windows uses retained directory handles and relative NT file operations for
status, result and binding files. It requires local NTFS, rejects reparse
aliases, and checks private owner/DACL and single-link binding/fence files.
UNC and other filesystems are refused. Status mutations share a physical
directory mutex and a bounded cross-process fence. Windows Tasks do not add
process cancellation authority: cancellation of active work reports
`cancellation unavailable: no current controller`; an already terminal Task
still acknowledges cancellation without changing work.

### Native ACP permission input

Set `TACHI_ACP_APPROVAL_MODE=interactive` in the resident daemon's environment
before starting Native ACP work. The default is `automatic`, preserving the
existing typed read/search permission policy. Interactive mode adds a human or
host confirmation to a permission that the existing execution grant already
allows. It only handles the exact `session/request_permission` method with a
canonical read/search kind and an unambiguous `allow_once` option. Writes,
execution, conflicting kinds and `allow_always` remain denied.

When that live process asks for permission, `tasks/get` exposes
`status: "input_required"` and one `elicitation/create` request in
`inputRequests`. The client must advertise
`clientCapabilities.elicitation.form = {}`. The CLI advertises this capability
and prints the form; it does not answer on the user's behalf. The form asks for
one boolean, `approve`, and labels the operation title as agent-supplied text.
Copy the actual input key from `get`, then answer:

```sh
tachi mcp task --project PROJECT --agent-identity AGENT_ID \
  update TASK_ID --input-responses \
  '{"INPUT_KEY":{"resultType":"complete","action":"accept","content":{"approve":true}}}'
```

Use `approve: false`, `action: "decline"`, or `action: "cancel"` to refuse;
decline/cancel omit `content`. A reply requires the same run owner and current
project/source binding, plus a profile that can perform Staff `start`. Both the
launch admission and the replying connection's admission are checked at the
decision commit, serialized with revocation. The reply cannot change the
execution grant, credentials, tools, cwd, sandbox or permission profile.

The pending request binds its unique key, schema, request/grant digests,
principal, project, physical source, controller epoch, status revision and
expiry. A recorded decision resumes only the existing waiting ACP process.
The canonical receipt retains at most 64 typed permission decision/withdrawal
events per run; after this limit, further interactive permission is denied.
Each wait lasts at most five minutes and is also bounded by the runner's
original deadline. Disconnecting the MCP client does not cancel the waiter.
If unrelated route/identity metadata advances the receipt revision, the live
runner reissues the same permission under a fresh input key; replies to the
superseded key are ignored. Fetch the latest `inputRequests` before answering.
Restarting the daemon never reconstructs a live input channel from disk.

Required postflight runs deny interactive permission without exposing pending
worker text before artifact release. Plan-review continuation, arbitrary
question input, attached external harness intervention and OpenClaw migration
remain separate lifecycle work under
[#1531](https://github.com/kckylechen1/tachi/issues/1531) and
[#1677](https://github.com/kckylechen1/tachi/issues/1677).

## Auth posture (v1 decision)

| Decision | Value |
|---|---|
| Bind | **127.0.0.1 only** — no remote multi-host |
| Auth | **`loopback-trust-v1`** — single-user workstation; no bearer token |
| Multi-tenant header ACL | **#495** (out of scope for v1) |
| Advertise | `/health` → `auth_posture: "loopback-trust-v1"`, `bind: "127.0.0.1"` |

## Client identity (not env)

HTTP has no per-process env. Legacy peers send identity at **initialize**;
modern peers send it in each request:

| Field | Header | MCP `_meta` |
|---|---|---|
| Project binding | `X-Tachi-Project` | `tachiProject` (alias `tachi.project`) |
| Tool profile | `X-Tachi-Profile` | `tachiProfile` |
| Client label | `X-Tachi-Client` | `tachiClient` |
| Agent identity | `X-Tachi-Agent-Identity` | `tachiAgentIdentity` (alias `tachi.agentIdentity`) |

For compatibility, headers win over initialize metadata in legacy mode. Modern
header and request-metadata identities must agree. Project must resolve via
`resolve_named_project_db_path`.

Standard MCP protocol/routing header conflicts are rejected by RMCP at the
HTTP transport boundary with status 400. A conflict between otherwise valid
`X-Tachi-*` and request `_meta` identity reaches Tachi's application boundary
and returns JSON-RPC `HEADER_MISMATCH` (`-32020`) with HTTP 400 as well.
Modern `INVALID_PARAMS` (`-32602`) caused by missing or malformed required
protocol metadata maps to HTTP 400. With complete required metadata,
application validation of optional `clientInfo`, identity aliases or tool
profile admission returns HTTP 200 with a JSON-RPC error. Clients must inspect
the JSON-RPC envelope in both cases. Legacy application JSON-RPC errors retain
HTTP 200.

For direct HTTP, a valid, explicit AgentIdentity assertion on this loopback-only
transport is recorded as `self_asserted`, never `verified`. An absent assertion
stays identity-less and rejected; the daemon process environment is not an
identity fallback for HTTP clients. This is the local attribution posture
frozen in `identity-workclaim-spine-v1.md`, not remote identity proof.

### Rate-limit bucket (`X-Tachi-Rate-Limit-Session`)

The daemon's loop/stuck detection (`RateLimiter` burst and RPM windows) is
keyed by a per-session `rate_limit_session_id`. Modern HTTP uses request-local server clones, so identified callers share an identity-derived bucket across requests.

| Peer | Bucket key |
|---|---|
| Modern HTTP with an AgentIdentity | Digest of the resolved AgentIdentity, client label and canonical bound project (peers asserting the same AgentIdentity, client and project share one bucket). This takes precedence over the bucket header: a modern request runs on a fresh clone, so honoring a rotated key would let one identified peer shed its own burst window |
| Any HTTP peer sending a valid `X-Tachi-Rate-Limit-Session`, with no AgentIdentity | That key (legacy: for the session; modern: per request) |
| Modern HTTP without the header or an AgentIdentity | Per-request key (unchanged fallback); a client label or project alone is shared by every instance of that client, so it never anchors a bucket |
| Legacy HTTP without the header | Per-session key (unchanged) |

The header only selects a rate-limit bucket. It never grants identity,
profile, project, or authority, and it is applied only after every identity
check has passed. It is header-only: there is no `_meta` twin, and
`initialize` metadata cannot set it. Values must be 16 to 128 characters of
`[A-Za-z0-9_-]`; anything else is ignored rather than failing the request, and
the daemon keeps its own per-session or identity-derived key. For a legacy
session a caller could choose a fresh key per call, but it could already open
a fresh session, so this adds no new way around the limiter; for a modern
identity-asserting request the identity-derived bucket wins regardless of the
header, so rotating keys no longer sheds the burst window.

Canonical local `tachi_staff(action='status')` calls are status polling: the
native router, canonical read-only action-effect metadata, and strictly decoded
`TachiStaffParams` with a required valid `dispatch_id` allow identical polls
without loop warnings or burst rejection. Validation uses the existing dispatch
ID allowlist and does not look up the run; a well-formed unknown ID remains a
read-only probe. Every poll still counts toward the configured RPM limit in
the same session bucket, including calls sharing
that bucket through server clones. A configured RPM of zero retains the
operator's existing unlimited setting. This exception does not apply to Staff
start, cancel or preflight, unknown actions or malformed arguments,
proxy-qualified routes, or other read-only tools. Generic limiter entry points retain strict loop
detection; a caller cannot select a polling policy through request arguments.
Staff retains the existing whole-tool cache invalidation, including on status
reads. The polling exception changes only identical-call loop detection.

### Claude Code / host config sketch

```json
{
  "mcpServers": {
    "tachi": {
      "type": "http",
      "url": "http://127.0.0.1:6919/mcp",
      "headers": {
        "X-Tachi-Profile": "standard",
        "X-Tachi-Client": "claude-code",
        "X-Tachi-Workspace-Root": "/absolute/path/to/workspace"
      }
    }
  }
}
```

Store a workspace-bound config in that workspace's host config (for example,
Claude Code's `.mcp.json`). `X-Tachi-Workspace-Root` is the actual local caller
workspace; the daemon resolves its existing project identity. An explicit
`X-Tachi-Project` may instead use the registered key Tachi reports. Do not guess
a hash or persist one workspace's root into a general user-wide configuration.
A user-wide unbound configuration stays global-only until its host supplies a
workspace per connection. See INSTALL.md.

A modern (`2026-07-28`) client that sends none of `X-Tachi-Agent-Identity`
or `X-Tachi-Rate-Limit-Session` gets a per-request rate-limit bucket, so
loop/stuck detection cannot fire for it. Send an `X-Tachi-Agent-Identity` to
anchor a stable bucket (it wins even when a bucket header is also sent), or
an `X-Tachi-Rate-Limit-Session` if the client has no identity to assert; see
[Rate-limit bucket](#rate-limit-bucket-x-tachi-rate-limit-session).

### Process inventory check

With HTTP direct-connect configured and the session idle:

```bash
# Expect: one resident daemon, no extra `tachi serve` children
pgrep -fl 'tachi|tachi-server' || true
curl -sS http://127.0.0.1:6919/health | jq '{status,bind,auth_posture,transport,reconnect}'
```

A client still configured to spawn Tachi over stdio will fail immediately.
Replace its command/args entry with native HTTP configuration; no Tachi stdio
adapter remains. Third-party Hub MCP child processes are separate transports.

## Reconnect after daemon restart

| Actor | Behavior |
|---|---|
| **Legacy HTTP client (through 2025-11-25)** | Connection drops / JSON-RPC **`-32000`** (or transport error). Wait until `/health` is `ok`, then **re-run `initialize`** to get a new `mcp-session-id`. |
| **Modern HTTP client (2026-07-28)** | The request fails with a transport error. Wait until `/health` is `ok`, then run `server/discover` again when capability refresh is needed. Retry a stateless request with the same validated per-request metadata and routing headers only when the failure is known to be `BeforeDispatch`; after a timeout or other ambiguous post-dispatch failure, do not replay automatically because a mutation may already have committed. Do **not** call `initialize`: explicit modern initialize is rejected by design and modern requests never carry an `mcp-session-id`. |
| **Idle shutdown (explicit opt-in)** | A positive `TACHI_DAEMON_IDLE_TIMEOUT_SECS` allows shutdown. A supervisor must restart it; an HTTP client does not spawn the service. The default is resident. |

`/health` advertises:

```json
{
  "reconnect": {
    "on_disconnect": "re-initialize MCP session (new mcp-session-id)",
    "client_hint": "expect brief JSON-RPC -32000 ..."
  }
}
```

The `/health.reconnect.on_disconnect` string above describes the retained
legacy session adapter. It is not an instruction for a `2026-07-28` peer.

## Migration policy

1. **HTTP only for Tachi clients.** Refuse explicit stdio selections. Existing live client files are migrated only by an operator-approved config update.
2. **Parity requirement for a client:** same profile surface + project-scoped
   save lands in the bound project DB + global scope still reachable.
3. **Privileged profiles** over public HTTP are refused until authorization is provided. The private local proxy capability is not a generic HTTP credential.
4. Cross-library **reads** with explicit `project=` remain allowed; **writes** stay bound.

## Discrimination tests (CI)

| Test | Asserts |
|---|---|
| `http_direct_connect_header_identity_binds_profile_and_project` | Headers bind; save+search global+project |
| `initialize_meta_binds_profile_client_and_project` | `_meta` key extraction (headers still preferred on the wire) |
| `http_direct_connect_initialize_advertises_http_guidance` | initialize instructions mention HTTP reconnect |
| `http_loopback_explicit_agent_identity_is_self_asserted_for_a2a` | explicit loopback identity is local self-asserted attribution; A2A sees the exact actor |
| `http_direct_connect_does_not_inherit_daemon_process_env_identity` | absent HTTP identity stays rejected even when daemon env is set |
| `http_direct_connect_rejects_admin_profile_without_authorization_policy` | admin refused |
| `http_direct_connect_unbound_session_rejects_explicit_cross_project_write` | C1 unbound write |
| `http_direct_connect_bound_session_rejects_cross_project_write` | Bound write isolation |
| `daemon_health_payload` unit | loopback + reconnect fields |

## Acceptance checklist (#732)

- [x] Identity over protocol (headers + meta), not env.
- [x] Loopback bind + documented loopback-trust posture.
- [x] Reconnect semantics documented + `/health` hints.
- [x] admin profile refused without authorization policy.
- [x] Stdio remains compatibility layer (not deprecated).
- [x] Automated HTTP identity / write-isolation / save-landing tests.
- [ ] Live host dogfood note (optional ops): Claude Code HTTP session + `pgrep` inventory on a workstation — manual, not CI.

## Related

- #731 multi-project daemon substrate  
- #746 library identity track  
- #495 profile/authorization  
- #737 cross-library read asymmetry  

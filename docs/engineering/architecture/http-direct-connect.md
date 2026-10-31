# HTTP Direct-Connect Cookbook (#732)

> **Status:** current capability + migration guide (2026-10-30).
> **Parent:** [#732](https://github.com/kckylechen1/tachi/issues/732) under [#746](https://github.com/kckylechen1/tachi/issues/746).  
> **Companion:** [`library-identity-runtime.md`](./library-identity-runtime.md).

## Goal

Capable MCP clients speak **Streamable HTTP** straight to the resident daemon:

```text
http://127.0.0.1:6919/mcp
```

Stdio adapters remain a **permanent compatibility pipe** for clients that cannot
do HTTP. Target shape for Claude Code / Codex-class hosts:

```text
1 tachi daemon process
0 mandatory per-session tachi serve subprocesses
```

## Landed substrate (do not re-implement)

| Piece | Where |
|---|---|
| Loopback HTTP listener + `/mcp` | `bootstrap/serve/daemon.rs` (`127.0.0.1`) |
| `/health` (status, transport, auth posture, reconnect) | same |
| Legacy-only session identity at `initialize` | headers `x-tachi-*` **or** meta `tachi*` |
| Profile filter (admin refused without #495 policy) | `server_handler::parse_http_tool_profile` |
| Read/write asymmetry | `session_identity` |
| E2E tests | `bootstrap/serve/stdio/tests.rs` (`http_direct_connect_*`) |

## Protocol compatibility (RMCP 3.8, #1891)

The daemon and stdio proxy implement two explicit peer modes. Legacy peers
retain `initialize`, `notifications/initialized`, and HTTP session behavior
through `2025-11-25`; legacy tool responses omit `resultType`. Modern peers use
RMCP 3.8's `2026-07-28` `server/discover` lifecycle, typed capabilities,
per-request client metadata, `resultType`, and stateless HTTP routing. A peer
that explicitly sends `2026-07-28` through the removed `initialize` lifecycle
gets `UNSUPPORTED_PROTOCOL_VERSION` rather than a successful downgrade to
`2025-11-25`. The portable server remains intentionally legacy-only and fails
the same modern initialize explicitly.

The normal stdio adapter is the proxy above. The debugging-only direct route
selected by `TACHI_DISABLE_STDIO_PROXY=1` remains legacy-only: because it has
neither the proxy's process-bound admission gate nor HTTP transport headers, it
returns typed `UNSUPPORTED_PROTOCOL_VERSION` for modern discovery, listing,
completion, and tool calls rather than accepting request metadata as authority.

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
`2026-07-28` Tasks extension. The stdio daemon proxy also forwards this extension.
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

### Stdio proxy and Windows

A Tasks-capable modern stdio peer uses a modern discovery hop to the resident
daemon for result requests and `tasks/get`, `tasks/update`, and `tasks/cancel`.
The proxy advertises Tasks only after the daemon confirms support. It forwards
the request's admitted identity, fixed project/profile, and form-elicitation
capability. Reconnecting the client retains access to the same durable Task.
An initialization failure may use the existing daemon refresh path; an error
after dispatch is returned without replaying the operation. A missing Tasks
capability never triggers a downgrade to legacy execution. Legacy stdio peers
keep their normal tool responses. Direct embedded/debug stdio and the portable
server do not expose this Tasks consumer.

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
`resolve_named_project_db_path` (same as stdio).

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

For modern stdio, per-request project and profile metadata may only repeat the
project and profile admitted when the adapter process started; omission keeps
those process bindings, while any different declaration is rejected before a
daemon call. Client labels and AgentIdentity assertions are validated and
forwarded only for that request. Canonical and dotted aliases must agree, and
an omitted later request never inherits an earlier modern request's identity.
An omitted modern stdio AgentIdentity does retain the longstanding process
binding: the adapter resolves `TACHI_AGENT_IDENTITY` afresh for that outbound
call. Thus “request-local” forbids previous-request stickiness; it does not
discard process configuration. A present malformed assertion is rejected and
never falls through to that environment value.

For direct HTTP, a valid, explicit AgentIdentity assertion on this loopback-only
transport is recorded as `self_asserted`, never `verified`. An absent assertion
stays identity-less and rejected; the daemon process environment is not an
identity fallback for HTTP clients. This is the local attribution posture
frozen in `identity-workclaim-spine-v1.md`, not remote identity proof.

### Rate-limit bucket (`X-Tachi-Rate-Limit-Session`)

The daemon's loop/stuck detection (`RateLimiter` burst and RPM windows) is
keyed by a per-session `rate_limit_session_id`. Two peers used to get a fresh
key on every call: the stdio proxy opens a new short-lived daemon MCP session
for each `tools/call` (there is deliberately no session pool), and a modern
`2026-07-28` request runs on a request-local server clone. Either way the burst
window started empty, so repeat-call warnings and loop blocks never fired.

| Peer | Bucket key |
|---|---|
| stdio proxy | One random key minted per proxy connection and sent as `X-Tachi-Rate-Limit-Session` on every daemon tool call (legacy: read at `initialize`) |
| Modern HTTP with an AgentIdentity | Digest of the resolved AgentIdentity, client label and canonical bound project (peers asserting the same AgentIdentity, client and project share one bucket). This takes precedence over the bucket header: a modern request runs on a fresh clone, so honoring a rotated key would let one identified peer shed its own burst window |
| Any HTTP peer sending a valid `X-Tachi-Rate-Limit-Session`, with no AgentIdentity | That key (legacy: for the session; modern: per request) |
| Modern HTTP without the header or an AgentIdentity | Per-request key (unchanged fallback); a client label or project alone is shared by every instance of that client, so it never anchors a bucket |
| Legacy HTTP without the header, local stdio | Per-session key (unchanged) |

The header only selects a rate-limit bucket. It never grants identity,
profile, project, or authority, and it is applied only after every identity
check has passed. It is header-only: there is no `_meta` twin, and
`initialize` metadata cannot set it. Values must be 16 to 128 characters of
`[A-Za-z0-9_-]`; anything else is ignored rather than failing the request, and
the daemon keeps its own per-session or identity-derived key. For a legacy
session a caller could choose a fresh key per call, but it could already open
a fresh session, so this adds no new way around the limiter; for a modern
identity-asserting request the identity-derived bucket wins regardless of the
header, so rotating keys no longer sheds the burst window. The stdio proxy's
retry split is unchanged: only a `BeforeDispatch` failure is retried, with
the same key.

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
      "url": "http://127.0.0.1:6919/mcp",
      "headers": {
        "X-Tachi-Profile": "standard",
        "X-Tachi-Client": "claude-code",
        "X-Tachi-Project": "Sigil-433b921b"
      }
    }
  }
}
```

Use the **named project key** Tachi reports for the repo (often
`<Basename>-<hash8>`), not a free-form display name. See INSTALL.md.

A modern (`2026-07-28`) client that sends none of `X-Tachi-Agent-Identity`
or `X-Tachi-Rate-Limit-Session` gets a per-request rate-limit bucket, so
loop/stuck detection cannot fire for it. Send an `X-Tachi-Agent-Identity` to
anchor a stable bucket (it wins even when a bucket header is also sent), or
an `X-Tachi-Rate-Limit-Session` if the client has no identity to assert; see
[Rate-limit bucket](#rate-limit-bucket-x-tachi-rate-limit-session).

### Process inventory check

With HTTP direct-connect configured and the session idle:

```bash
# Expect: one daemon (or zero if idle-reaped), no extra `tachi serve` children
pgrep -fl 'tachi|tachi-server' || true
curl -sS http://127.0.0.1:6919/health | jq '{status,bind,auth_posture,transport,reconnect}'
```

Stdio-configured clients will still show a short-lived adapter process — that is
the compatibility path, not a failure of HTTP migration.

## Reconnect after daemon restart

| Actor | Behavior |
|---|---|
| **Legacy HTTP client (through 2025-11-25)** | Connection drops / JSON-RPC **`-32000`** (or transport error). Wait until `/health` is `ok`, then **re-run `initialize`** to get a new `mcp-session-id`. |
| **Modern HTTP client (2026-07-28)** | The request fails with a transport error. Wait until `/health` is `ok`, then run `server/discover` again when capability refresh is needed. Retry a stateless request with the same validated per-request metadata and routing headers only when the failure is known to be `BeforeDispatch`; after a timeout or other ambiguous post-dispatch failure, do not replay automatically because a mutation may already have committed. Do **not** call `initialize`: explicit modern initialize is rejected by design and modern requests never carry an `mcp-session-id`. |
| **stdio adapter** | Host respawns the pipe; adapter re-attaches to (or re-spawns) the daemon. Unrelated to HTTP session ids. |
| **Idle reaper** | Daemon may exit after idle timeout and auto-respawn on next need; use the matching legacy-session or modern-stateless reconnect row above. |

`/health` advertises:

```json
{
  "reconnect": {
    "on_disconnect": "re-initialize MCP session (new mcp-session-id)",
    "client_hint": "expect brief JSON-RPC -32000 ...",
    "stdio_unaffected": true
  }
}
```

The `/health.reconnect.on_disconnect` string above describes the retained
legacy session adapter. It is not an instruction for a `2026-07-28` peer.

## Migration policy

1. **Opt-in per client.** Never force HTTP; stdio stays supported forever.
2. **Parity requirement for a client:** same profile surface + project-scoped
   save lands in the bound project DB + global scope still reachable.
3. **admin profile** over HTTP is refused until #495 wires authorization.
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

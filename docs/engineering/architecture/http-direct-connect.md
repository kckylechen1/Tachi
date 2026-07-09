# HTTP Direct-Connect Cookbook (#732)

> **Status:** current capability + migration guide (2026-07-09).  
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
| Session identity at `initialize` | headers `x-tachi-*` **or** meta `tachi*` |
| Profile filter (admin refused without #495 policy) | `server_handler::parse_http_tool_profile` |
| Read/write asymmetry | `session_identity` |
| E2E tests | `bootstrap/serve/stdio/tests.rs` (`http_direct_connect_*`) |

## Auth posture (v1 decision)

| Decision | Value |
|---|---|
| Bind | **127.0.0.1 only** — no remote multi-host |
| Auth | **`loopback-trust-v1`** — single-user workstation; no bearer token |
| Multi-tenant header ACL | **#495** (out of scope for v1) |
| Advertise | `/health` → `auth_posture: "loopback-trust-v1"`, `bind: "127.0.0.1"` |

## Client identity (not env)

HTTP has no per-process env. Send identity at **initialize**:

| Field | Header | Initialize meta |
|---|---|---|
| Project binding | `X-Tachi-Project` | `tachiProject` (alias `tachi.project`) |
| Tool profile | `X-Tachi-Profile` | `tachiProfile` |
| Client label | `X-Tachi-Client` | `tachiClient` |

Headers win over meta when both are present. Project must resolve via
`resolve_named_project_db_path` (same as stdio).

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

### Process inventory check

With HTTP direct-connect configured and the session idle:

```bash
# Expect: one daemon (or zero if idle-reaped), no extra `tachi serve` children
pgrep -fl 'tachi|memory-server' || true
curl -sS http://127.0.0.1:6919/health | jq '{status,bind,auth_posture,transport,reconnect}'
```

Stdio-configured clients will still show a short-lived adapter process — that is
the compatibility path, not a failure of HTTP migration.

## Reconnect after daemon restart

| Actor | Behavior |
|---|---|
| **HTTP client** | Connection drops / JSON-RPC **`-32000`** (or transport error). **Re-run `initialize`** to get a new `mcp-session-id`. Wait until `/health` is `ok`. |
| **stdio adapter** | Host respawns the pipe; adapter re-attaches to (or re-spawns) the daemon. Unrelated to HTTP session ids. |
| **Idle reaper** | Daemon may exit after idle timeout and auto-respawn on next need — same reconnect for HTTP. |

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

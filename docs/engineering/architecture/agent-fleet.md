# Tachi Staff execution backends

**Status:** Active  
**Date:** 2026-11-04
**Issues:** [#155](https://github.com/kckylechen1/tachi/issues/155) Agent Router, [#158](https://github.com/kckylechen1/tachi/issues/158) Eval harness  
**Historical design:** [`agent-router-spec.md`](agent-router-spec.md); current execution uses explicit Staff profiles.

> **Scope correction (2026-07-20, #1312):** this fleet is available only for explicit durable/cross-session, cross-device/remote, native-unavailable, or owner-requested Tachi dispatch. It is not the default replacement for a host harness's native subagents.

---

## Fleet policy

Tachi **dispatch** targets the four Phase 1 CLI workers and the explicitly
selected DeepSeek Harness headless adapter:

| Agent | CLI | Non-interactive | JSON | MCP inject (Tachi-generated config) |
|-------|-----|-----------------|------|-------------------------------------|
| `claude` | `claude` | `-p` | `--output-format json` | `--mcp-config <file>` |
| `codex` | `codex` | `exec` | `--json` | Per-run `-c mcp_servers.<name>=<TOML>` overrides |
| `grok` | `grok` | `-p` / `--single` | `--output-format json` | Best-effort `--mcp-config` (Claude-compatible) |
| `kimi` | `kimi` | `-p` | `--output-format json` | No |
| `dsh` | `dsh` | `--profile headless` | `--json` | No |

Aliases accepted at dispatch time (normalized to canonical names):

- **claude:** `claude-code`, `claude-cli`
- **codex:** `codex-cli`, `openai`
- **grok:** `grok-cli`, `xai`
- **kimi:** `kimi-cli`, `moonshot`
- **dsh:** `deepseek-harness`

DSH uses the admitted `dsh_executor` profile. Native execution supports macOS/Linux only;
canonical admission rejects other host platforms before worker spawn. Its
workspace authority is advisory. See the
[headless adapter contract](deepseek-harness-adapter.md) for grant restrictions,
native configuration ownership and completion evidence.

`agent=custom` remains for one-off commands (trusted allowlist).

## Worker selection

The host chooses an explicit Staff dispatch profile for the requested work.
`tachi_agents(action="list")` and `tachi_agents(action="profiles")` expose
backend metadata and the current projected profile rows. The old
`tachi_agents(action="select")` heuristic is retired; task keywords no longer
choose a CLI through this registry.

The backend registry still owns alias normalization, binary names, timeouts
and MCP injection compatibility. Typed OpenCode profiles and admitted custom
commands retain their existing launch paths. Profile resolution and its
fallback metadata remain intact; retiring the independent selector does not
change Staff admission, launch, cancellation, result or delivery semantics.

Historical model names in evaluation or memory records remain historical
facts. The registry does not rewrite them to a different worker.

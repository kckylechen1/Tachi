# Tachi dispatch fleet (Phase 1)

**Status:** Active  
**Date:** 2026-06-03  
**Issues:** [#155](https://github.com/kckylechen1/tachi/issues/155) Agent Router, [#158](https://github.com/kckylechen1/tachi/issues/158) Eval harness  
**Implementation spec:** [`agent-router-spec.md`](agent-router-spec.md) (architecture detail)

> **Scope correction (2026-07-20, #1312):** this fleet is available only for explicit durable/cross-session, cross-device/remote, native-unavailable, or owner-requested Tachi dispatch. It is not the default replacement for a host harness's native subagents.

---

## Fleet policy

Tachi **dispatch** targets exactly **four** CLI workers:

| Agent | CLI | Non-interactive | JSON | MCP inject (Tachi-generated config) |
|-------|-----|-----------------|------|-------------------------------------|
| `claude` | `claude` | `-p` | `--output-format json` | `--mcp-config <file>` |
| `codex` | `codex` | `exec` | `--json` | No — use `~/.codex/config.toml` |
| `grok` | `grok` | `-p` / `--single` | `--output-format json` | Best-effort `--mcp-config` (Claude-compatible) |
| `kimi` | `kimi` | `-p` | `--output-format json` | No |

Aliases accepted at dispatch time (normalized to canonical names):

- **claude:** `claude-code`, `claude-cli`
- **codex:** `codex-cli`, `openai`
- **grok:** `grok-cli`, `xai`
- **kimi:** `kimi-cli`, `moonshot`

`agent=custom` remains for one-off commands (trusted allowlist).

**Out of fleet (Phase 1):** gemini, qwen, copilot, droid, and other CLIs from research reports. They may appear in SFT labels or eval history and should be **remapped** to the four agents above.

**Not dispatch agents:** Qwen / SiliconFlow / local ollama lanes used for extract, summary, kanban, and secretary tasks ([#151](https://github.com/kckylechen1/tachi/issues/151)).

---

## Routing heuristics (manual / classifier)

| Task class | Preferred | Fallback |
|------------|-----------|----------|
| Implementation / fix / plan | `claude` | `grok` → `codex` |
| Bulk refactor / patch execution | `codex` | `claude` |
| Long-context review / explain (ZH) | `kimi` | `claude` |
| Interactive Build-style sessions | `grok` | `claude` |

Empirical scores from [#158](https://github.com/kckylechen1/tachi/issues/158) replace these defaults over time.

---

## SFT label remapping

Historical samples may name `gemini`, `qwen`, `copilot`, etc. When training the router ([#153](https://github.com/kckylechen1/tachi/issues/153)):

| Legacy label | Map to |
|--------------|--------|
| `gemini` (review, long context) | `kimi` or `claude` |
| `qwen` (explain, cheap) | `kimi` |
| `copilot` (test, standard) | `codex` or `claude` |
| `droid` | `codex` |

---

## Acceptance (Phase 1 code)

- [ ] `tachi_dispatch` (retired route — now `tachi_staff`) accepted `claude`, `codex`, `grok`, `kimi`, `custom`
- [ ] Static `agent_registry` lists only the four workers (+ documents `custom`)
- [ ] `inject_tachi_mcp` / `inject_hub_mcps` rejected for `codex` and `kimi`
- [ ] Unit tests cover alias normalization and command argv for each agent

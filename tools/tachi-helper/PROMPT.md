# Tachi Helper Wizard — Code Review & Bug Fix Request

## What is this project?

Tachi is a Rust-based MCP (Model Context Protocol) server that provides memory, vault (encrypted secret storage), and Hub services for AI agents. The project lives at `/Users/kckylechen/Desktop/Sigil/`.

We built a Go TUI setup wizard (`tools/tachi-helper/`) that helps users configure Tachi interactively. It uses the [bubbletea](https://github.com/charmbracelet/bubbletea) TUI framework with [lipgloss](https://github.com/charmbracelet/lipgloss) for styling.

## What the wizard does (9 steps)

1. **Welcome** — Shows what will be configured, press Enter to start
2. **Health Check** — Async check: tachi binary exists? global DB? vault? .env files? shell type?
3. **Vault Setup** — Creates master password → initializes encrypted vault via MCP
4. **Providers** — Selects embedding, reasoning/chat, and dispatch providers/endpoints
5. **Import Keys** — Scans .env files, preselects provider-required keys, imports selected keys into vault
6. **MCP Servers** — Multi-select common MCP servers (Exa, Tavily, etc.) → registers in Hub
7. **Backend** — Tunes model lanes for embedding, rerank, frontend LLM, and Foundry LLM
8. **Shell Integration** — Adds `tachi_env()` helper function to shell rc
9. **Done** — Summary of everything configured

## File-by-file architecture

```
tools/tachi-helper/
├── main.go                 — Entry point. Parses version/help args, runs bubbletea program
├── wizard.go               — Orchestrator. Holds State + step slice. Handles stepDone/stepBack msgs
│                             Controls step transitions (stepID enum: welcome→doctor→vault→providers→keys→mcp→foundry→shell→summary)
│                             Renders header (step progress bar) + footer (key hints)
├── state.go                — Shared State struct: TachiPath, TachiVersion, FoundKeys,
│                             ProviderSelections, SelectedKeys, SelectedMCPs, ShellType, ShellRC, RCModified
├── styles.go               — lipgloss color palette + style definitions. OpenCode-inspired.
│                             Cyan accent, zinc grayscale. Rounded border box.
├── select.go               — Custom multi-select component (bubbletea doesn't ship one)
│                             Supports: ↑/↓ navigate, space toggle, 'a' select/deselect all
├── mcp.go                  — MCP JSON-RPC client over stdio. Spawns `tachi` subprocess.
│                             CRITICAL: sets TACHI_PROFILE=admin + clears TACHI_EXPOSED_TOOLS
│                             Uses bufio.Writer with explicit Flush() after each JSON-RPC message
├── tachi.go                 — Utility functions:
│                             findTachi()          — lookPath for tachi binary
│                             tachiVersion()       — runs tachi --version
│                             parseDotEnv()        — reads .env → map[string]string
│                             scanDotEnvFiles()    — searches CWD (walk up 3 levels) + home dirs
│                             maskSecret()         — shows first4...last4 for long values
│                             detectShell()        — $SHELL → (shellType, rcPath)
│                             appendToRC(rcPath)   — appends tachi_env() function to rc file
│                             mcpServerDefs        — hardcoded list of 5 MCP servers to offer
│                             parseJSONString()    — JSON field extractor (unused?)
├── step_welcome.go         — Simple welcome screen with numbered step list
├── step_doctor.go          — Async health check. Returns checkResult[] via doctorResultsMsg
│                             Checks: tachi binary, global DB, vault status, .env files, shell
├── step_vault.go           — Auto-generates / reads the vault password from macOS Keychain
│                             Calls vault_init or vault_unlock via MCP without manual password prompts
├── step_providers.go       — Provider/endpoint selection for embedding, reasoning/chat LLM,
│                             and dispatch backend. Defaults to Voyage embeddings and Claude Code;
│                             GLM 5.1 is offered for review/analysis. Writes ~/.tachi/config.env.
├── step_keys.go            — Scans .env files, builds multi-select of all keys
│                             Selected provider keys are preselected → MCP vault_set calls (batch)
├── step_mcp.go             — Multi-select of MCP servers → `tachi hub register ...`
├── step_shell.go           — Writes tachi_env() helper to shell rc
│                             Phase: confirm → writing → done/skip
├── step_done.go            — Summary screen showing all configured items + next steps
└── tachi_helper_test.go    — unit tests for env parsing, selection, provider config, and setup utilities
```

## Critical design pattern: MCP communication

The wizard doesn't call tachi CLI commands directly for vault operations. Instead, it spawns a `tachi` subprocess and communicates via MCP JSON-RPC protocol over stdin/stdout. This is implemented in `mcp.go`:

```go
// Spawning with admin privileges
cmd.Env = append(os.Environ(),
    "TACHI_PROFILE=admin",
    "TACHI_EXPOSED_TOOLS=",  // clear whitelist so all tools are visible
)

// Protocol handshake
1. Send: {"jsonrpc":"2.0","id":1,"method":"initialize","params":{...}}
2. Receive: {"jsonrpc":"2.0","id":1,"result":{...}}
3. Send: {"jsonrpc":"2.0","method":"notifications/initialized"}  // no ID = notification
4. Ready to call tools via: {"jsonrpc":"2.0","id":N,"method":"tools/call","params":{"name":"vault_init","arguments":{...}}}
```

The Rust side (`crates/memory-server/src/bootstrap.rs`) is where the tachi binary handles CLI commands. The `Env` command (line 1465-1564) opens the global DB read-only, prompts for password, derives Argon2id key, verifies against stored verifier, then decrypts and emits all vault entries as `export KEY='VALUE'` lines.

## The Rust `tachi env` command

File: `crates/memory-server/src/cli.rs` (line 360-373)
```rust
Env {
    #[arg(long)]
    filter: Option<String>,      // glob filter on secret names
    #[arg(long)]
    env_only: bool,              // only UPPER_SNAKE_CASE names
    #[arg(long)]
    stdin_password: bool,        // read password from stdin instead of terminal
}
```

File: `crates/memory-server/src/bootstrap.rs` (line 1465-1564)
- Opens global DB read-only
- Prompts for password via `rpassword::prompt_password("Vault password: ")`
- Derives key via Argon2id, verifies against vault verifier
- Lists all vault entries, decrypts each, outputs `export KEY='escaped_value'`
- Supports `--filter` (glob), `--env-only` (uppercase only), `--stdin-password`

---

## Current Implementation Notes

- Vault setup uses a generated password stored in macOS Keychain (`tachi-vault/default`) and `tachi_env()` calls `tachi env --keychain`.
- MCP registration shells out to `tachi hub register <id> --cap-type mcp --name <name> --definition <json>`.
- Provider selection writes non-secret provider settings to `~/.tachi/config.env` and leaves real API keys in Vault or `.env` imports.
- Default provider choices are Voyage embeddings, Claude Code reasoning/dispatch, and GLM 5.1 as the recommended review/analysis option.
- If `tachi` is missing, the doctor step prevents continuing into MCP-backed setup steps.

---

## Testing

Run all tests from `tools/tachi-helper/`:
```bash
cd tools/tachi-helper
go test -v ./...
```

All 9 tests should pass:
1. `TestScanDotEnvFiles` — Creates temp .env, scans, verifies found
2. `TestParseDotEnv` — Parses various .env formats (quoted, empty, comments)
3. `TestMaskSecret` — Boundary cases for secret masking
4. `TestDetectShell` — Detects current shell type and rc path
5. `TestShortPath` — Converts home paths to ~ notation
6. `TestMultiSelect` — Select all, deselect, verify selected list
7. `TestFindTachi` — Checks tachi is in PATH
8. `TestMCPClientStart` — Spawns real tachi subprocess, calls vault_status
9. `TestAppendToRC` — Verifies idempotent append of helper function to rc file

After making changes, ensure:
- `go build` succeeds with no warnings
- All 9 tests pass
- The wizard runs interactively: `go run .`

## DO NOT

- Do NOT write `eval "$(tachi env)"` without `--keychain` to any shell rc file — it blocks the terminal with a password prompt
- Do NOT prompt for passwords in non-interactive contexts (shell startup, scripts, `eval` in rc files)
- Do NOT change the MCP client protocol in `mcp.go` — it works correctly
- Do NOT break existing test cases
- Do NOT add dependencies without checking if they're really needed (prefer `os/exec` to call system tools over CGo bindings)

# Tachi Helper Wizard — Code Review & Bug Fix Request

## What is this project?

Tachi is a Rust-based MCP (Model Context Protocol) server that provides memory, vault (encrypted secret storage), and Hub services for AI agents. The project lives at `/Users/kckylechen/Desktop/Sigil/`.

We built a Go TUI setup wizard (`tools/tachi-helper/`) that helps users configure Tachi interactively. It uses the [bubbletea](https://github.com/charmbracelet/bubbletea) TUI framework with [lipgloss](https://github.com/charmbracelet/lipgloss) for styling.

## What the wizard does (7 steps)

1. **Welcome** — Shows what will be configured, press Enter to start
2. **Health Check** — Async check: tachi binary exists? global DB? vault? .env files? shell type?
3. **Vault Setup** — Creates master password → initializes encrypted vault via MCP
4. **Import Keys** — Scans .env files, shows multi-select, imports selected keys into vault
5. **MCP Servers** — Multi-select common MCP servers (Exa, Tavily, etc.) → registers in Hub
6. **Shell Integration** — Adds `tachi_env()` helper function to shell rc
7. **Done** — Summary of everything configured

## File-by-file architecture

```
tools/tachi-helper/
├── main.go                 — Entry point. Parses version/help args, runs bubbletea program
├── wizard.go               — Orchestrator. Holds State + step slice. Handles stepDone/stepBack msgs
│                             Controls step transitions (stepID enum: welcome→doctor→vault→keys→mcp→shell→summary)
│                             Renders header (step progress bar) + footer (key hints)
├── state.go                — Shared State struct: TachiPath, TachiVersion, Password, FoundKeys,
│                             SelectedKeys, SelectedMCPs, ShellType, ShellRC, RCModified
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
├── step_vault.go           — Password input via bubbles/textinput (EchoPassword mode)
│                             Phase machine: password → confirm → creating → done → error
│                             Calls vault_init or vault_unlock via MCP
├── step_keys.go            — Scans .env files, builds multi-select of all keys
│                             Selected keys → MCP vault_set calls (batch)
├── step_mcp.go             — Multi-select of MCP servers → registerHubMCP() (STUB!)
│                             registerHubMCP() is currently: return nil // TODO
├── step_shell.go           — Writes tachi_env() helper to shell rc
│                             Phase: confirm → writing → done/skip
├── step_done.go            — Summary screen showing all configured items + next steps
└── tachi_helper_test.go    — 9 test cases, all passing
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

## Issues that need to be fixed

### Issue 1: Remove the manual password prompt — use macOS Keychain

**Severity: Critical — this is the worst UX bug**

**Current behavior:**
- `step_vault.go` has a password input UI using `bubbles/textinput` with `EchoPassword` mode
- The user types a password, confirms it, and it's stored in `State.Password` (in-memory only)
- This password is then used to call `vault_init` and `vault_unlock` via MCP
- But when the user later runs `tachi_env` in their shell, `tachi env` prompts for the password AGAIN via `rpassword::prompt_password()`
- This means `tachi_env()` blocks the terminal waiting for password input every time it's called

**What should happen instead:**
1. The wizard should auto-generate a strong random password (e.g., 32 bytes hex-encoded)
2. Store it in macOS Keychain using the `security` CLI:
   ```
   security add-generic-password -s "tachi-vault" -a "default" -w "<password>"
   ```
3. Use that password to call `vault_init` via MCP
4. The `tachi_env()` shell function should call `tachi env --keychain` which reads the password from keychain automatically
5. No manual password prompt anywhere in the flow

**Files to change:**

**Go side (`tools/tachi-helper/`):**
- `step_vault.go` — Remove the entire password input UI (textinput models, password/confirm phases). Replace with a simple "Creating vault with auto-generated key stored in macOS Keychain..." message. Generate password, store in keychain, call vault_init.
- `tachi.go` — Add two new helper functions:
  ```go
  func generatePassword() string {
      // 32 random bytes, hex-encoded
      b := make([]byte, 32)
      rand.Read(b)
      return hex.EncodeToString(b)
  }

  func storeInKeychain(password string) error {
      cmd := exec.Command("security", "add-generic-password",
          "-s", "tachi-vault",
          "-a", "default",
          "-w", password,
      )
      // Use -U flag to update if already exists
      return cmd.Run()
  }
  ```
- `state.go` — Remove `Password string` field (no longer needed since keychain handles it)
- `step_keys.go` — Remove the `vault_unlock` call that sends password. Instead, the vault should already be unlocked after init, or unlock should happen via keychain password.
- `step_shell.go` — Update `tachi_env()` function to use `tachi env --keychain`:
  ```go
  block := "\n# Tachi — run `tachi_env` to load vault secrets into shell\ntachi_env() { eval \"$(tachi env --keychain)\"; }\n"
  ```

**Rust side (`crates/memory-server/`):**
- `src/cli.rs` — Add `--keychain` flag to `Env` command:
  ```rust
  Env {
      // ... existing flags ...
      /// Read master password from macOS Keychain instead of prompting.
      /// Uses service name "tachi-vault", account "default".
      #[arg(long)]
      keychain: bool,
  }
  ```
- `src/bootstrap.rs` — In `run_env_command`, add keychain password retrieval:
  ```rust
  let password = if keychain {
      // Read from macOS Keychain
      let output = std::process::Command::new("security")
          .args(["find-generic-password", "-s", "tachi-vault", "-a", "default", "-w"])
          .output()?;
      if !output.status.success() {
          return Err(format!("Failed to read from Keychain: {}",
              String::from_utf8_lossy(&output.stderr)).into());
      }
      String::from_utf8(output.stdout)?.trim().to_string()
  } else if stdin_password {
      // ... existing stdin logic ...
  ```
- `Cargo.toml` — No new dependencies needed, we're calling the `security` CLI via `std::process::Command`

### Issue 2: MCP server registration is a stub

**Severity: High — feature doesn't actually work**

**Current code in `step_mcp.go` (line 161-165):**
```go
func registerHubMCP(id, name, definition string) error {
    // Use tachi hub register CLI command
    // tachi hub register --id mcp:exa --cap-type mcp --name "Exa" --definition '{...}'
    return nil // TODO: exec tachi hub register
}
```

This function returns nil without doing anything. The user sees "✓ Registered N MCP server(s)" but nothing was actually registered.

**Fix:**
1. First, check what the actual `tachi hub register` CLI interface looks like:
   ```bash
   tachi hub register --help
   ```
2. Implement the function to call the real CLI command. The expected signature:
   ```go
   func registerHubMCP(id, name, definition string) error {
       cmd := exec.Command("tachi", "hub", "register",
           "--id", id,
           "--cap-type", "mcp",
           "--name", name,
           "--definition", definition,
       )
       output, err := cmd.CombinedOutput()
       if err != nil {
           return fmt.Errorf("%s: %s", err, strings.TrimSpace(string(output)))
       }
       return nil
   }
   ```
3. Verify the flag names match the actual CLI by reading the Rust clap definition in `cli.rs` or running `--help`.

### Issue 3: Color scheme and UI polish

**Severity: Low — cosmetic but impacts first impression**

The current color scheme was inspired by OpenCode but needs refinement. The styles have already been improved from the initial version (added rounded borders, badge, dividers, code backgrounds). Remaining issues:

1. **Welcome screen** (`step_welcome.go`) — The ASCII banner is basic. Consider:
   - A more distinctive logo (ASCII art of the Tachi mark, or a cleaner typographic treatment)
   - Better spacing and visual hierarchy

2. **Step transitions feel abrupt** — No animation or visual feedback when moving between steps. Consider adding a brief transition state.

3. **Progress bar spacing** — The header step indicators (`✓ Welcome ─ ▶ Health ─ · Vault ─ ...`) may wrap on narrow terminals. Consider hiding labels and showing just icons on small widths.

4. **Code block readability** — The `codeStyle` uses `#A1A1AA` (zinc-400) on `#18181B` (zinc-900). This should be readable on most dark terminals but test with a light terminal theme.

5. **Done screen** — The "Next steps" section could use more visual weight. The numbered steps blend together.

### Issue 4: Error handling for missing tachi binary

**Severity: Medium — poor UX if tachi isn't installed**

If the user runs the wizard without tachi installed:
- `step_doctor.go` will show "✗ tachi binary — not found in PATH"
- But the wizard continues to the vault step, which spawns a tachi subprocess that fails
- The vault step shows a cryptic MCP error: "Failed to start tachi: exec: \"tachi\": executable file not found in $PATH"

**Fix:**
- If `findTachi()` fails in the doctor step, the wizard should stop and show instructions to install tachi
- Add a guard in `wizard.go` or `step_vault.go` that checks `state.TachiPath != ""` before attempting MCP operations
- Show a clear message: "Tachi is not installed. Install it first: curl -fsSL https://tachi.dev/install | sh"

### Issue 5: The `parseJSONString` function is unused

**Severity: Trivial**

`tachi.go` has a `parseJSONString()` function that's never called. Either:
- Remove it (it's dead code)
- Or use it somewhere (it was likely intended for parsing MCP responses)

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

# Credential Adapters And Cleanup

## Goal

P1 credential work extends the P0 broker without widening the default tool
surface. New adapters and cleanup commands must stay opt-in, redacted, and
reversible.

## Manual Cleanup

`tachi vault cleanup` is the operator-facing lifecycle command for managed
credential materializations.

Rules:

- It is dry-run by default; `--apply` is required to mutate files or metadata.
- At least one scope is required: `--run-dir`, `--profile`, or `--consumer`.
- Reports expose target paths and statuses, never secret values.
- `--mark-only` marks matching metadata as cleaned without deleting target
  files. This is the preferred unapply path for patched user config files.
- Non-`--mark-only` cleanup deletes only files whose current content hash still
  matches the last Tachi-managed materialization.
- `config_patch` targets outside `--run-dir` are not deleted automatically;
  use `--mark-only` after reviewing the downstream config.
- Targets inside `--run-dir` are treated as ephemeral dispatch artifacts,
  including `config_patch` targets, and may be removed during run cleanup.

Examples:

```bash
tachi vault cleanup --profile codex_shared --consumer codex_cli
tachi vault cleanup --profile codex_shared --consumer codex_cli --apply
tachi vault cleanup --profile opencode_shared --consumer opencode --mark-only --apply
```

## `config_patch`

`config_patch` is the first P1 adapter. It renders the materializer template with
the selected Vault secret, then recursively merges that JSON object into the
target JSON object.

Rules:

- The rendered template must be a JSON object.
- Existing target files require `--allow-existing`; Tachi creates a backup
  before atomic replacement.
- The written target defaults to `0600` unless the materializer declares a
  narrower chmod.
- Broad session/auth targets such as `.claude.json`, `.claude/*`, and
  `.claude-code-router/*` are refused.
- Reports and managed metadata stay redacted; content hashes are stored, not
  secret values.

Example:

```json
{
  "type": "config_patch",
  "source": "api_key",
  "target": "~/.config/opencode/opencode.json",
  "chmod": "0600",
  "template": {
    "provider": {
      "openai": {
        "options": {
          "apiKey": "{{secret}}"
        }
      }
    }
  }
}
```

## Deferred Adapters

These remain intentionally deferred until their semantics are narrowed:

- `file_symlink`: needs explicit rules for symlink ownership, link-target
  leakage, and cleanup behavior.
- `command_helper`: needs a stable wrapper format and expiry/lifecycle model.
- `oauth_refresh`: should be provider-specific and should not pretend OAuth
  session ownership is the same as API-key injection.

## Agent Presets

Agent-specific credential presets are opt-in. Dispatch profiles may declare
credential profiles only when the downstream harness path is confirmed and the
profile remains safe for projects without `.tachi/credentials`.

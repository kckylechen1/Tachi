# Credential adapters and cleanup

## Supported writes

Credential profiles have three supported adapters. Selected credentials remain
subject to profile/consumer authorization and Vault ACL before delivery.

| Adapter | Output | Boundary |
| --- | --- | --- |
| `env` | A named child environment variable | No config file write |
| `config_content_env` | Rendered configuration in a named child variable | No config file write |
| `config_overlay` | A temporary rendered file | Requires a caller-authorized `run_dir`; target stays inside it |

File overlays use owner-only permissions (`0400` or `0600`, default `0600`).
They reject path traversal and symlinks in target components. Unix writes use
held directory descriptors with no-follow traversal and atomic publication.
An explicit `--allow-existing` may replace only a leaf inside the authorized
run directory; it does not overwrite a hardlink's external backing file.
Run-directory aliases are resolved to the authorized canonical directory.
File overlays are unavailable on non-Unix systems; env adapters remain usable.

A manual file overlay must supply `--run-dir`. Dispatch supplies its own run
directory. Reports and managed metadata contain names, paths, status and hashes,
never rendered secrets.

## Retired adapters and migration

`file_copy` and `config_patch` are retired for new writes. They remain parseable
so doctor and cleanup can diagnose existing profiles and historical records.
Plans mark their steps `retired`, non-ready and `would_write=false`; apply fails
before executing any earlier step in the same profile.

The old `config_overlay` environment target is also refused with a migration
message. Change its type to `config_content_env`. For persistent auth/config
copies or patches, use environment indirection or a file overlay under
`{credentials_dir}` within the caller's run directory. Vault no longer merges
JSON into persistent user config or creates backups to replace it.

```json
{
  "type": "config_content_env",
  "source": "api_key",
  "target": "OPENCODE_CONFIG_CONTENT",
  "template": {"apiKey": "{{secret}}"}
}
```

Use `tachi vault doctor --profile NAME --consumer CONSUMER --run-dir /path/to/run`
to diagnose temporary overlays against the same directory used for materialization.
Doctor resolves the original template target and checks its managed metadata and
content without decrypting credentials or changing files. Historical profile
diagnostics remain available without `--run-dir`.

## Historical and temporary-file cleanup

`tachi vault cleanup` manages the lifecycle of new temporary overlays and
historical managed files. Its compatibility support does not re-enable retired
writers.

- Dry-run is the default; `--apply` is required to mutate files or metadata.
- At least one scope is required: `--run-dir`, `--profile`, or `--consumer`.
- Reports expose target paths and statuses, never secret values.
- `--mark-only` marks metadata as cleaned without deleting target files. Use it
  for historical patches after reviewing the downstream user config.
- Without `--run-dir`, historical managed-file deletion requires the current
  content hash to match the recorded materialization; changed files remain intact.
- With `--run-dir`, matching managed targets are scoped temporary artifacts.
  Cleanup may remove them even if their content changed after materialization;
  this mode does not require a matching content hash.
- Historical `config_patch` targets outside `--run-dir` are not deleted
  automatically. Matching targets inside a scoped run directory are ephemeral
  run artifacts and may be removed during cleanup.
- TTL, hash checks, canonical run-directory matching and old metadata formats
  remain supported by doctor and cleanup.

```sh
tachi vault cleanup --profile codex_shared --consumer codex_cli
tachi vault cleanup --profile codex_shared --consumer codex_cli --apply
tachi vault cleanup --profile opencode_shared --consumer opencode --mark-only --apply
```

Credentialed dispatch profiles stay opt-in. Projects without
`.tachi/credentials` do not acquire credential requirements from ordinary
built-in profiles.

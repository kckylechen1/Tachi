# Native Skill directory links

Keep each local Skill in one source directory, then link that whole directory
into each host's native skills directory. `SKILL.md`, `references/`, `scripts/`,
and other supporting files stay together. Editing the source immediately changes
what the links expose; a host may need to refresh its own Skill cache or session.
Tachi neither executes the Skill nor copies its contents.

## Explicit local installation

Both `--source` and `--target-dir` are explicit paths. The target skills directory
must already exist. Tachi does not discover a home directory, select a host, scan
other sources, or change host settings for these commands.

```sh
mkdir -p "$HOME/.agents/skills" "$HOME/.claude/skills"

tachi skill-surface link \
  --source /path/to/central/example-skill \
  --target-dir "$HOME/.agents/skills"

tachi skill-surface link \
  --source /path/to/central/example-skill \
  --target-dir "$HOME/.claude/skills"

tachi skill-surface links --target-dir "$HOME/.agents/skills" --json
tachi skill-surface unlink example-skill --target-dir "$HOME/.claude/skills"
```

The source must contain `SKILL.md`. Its final path component supplies the native
directory name: 1–128 ASCII letters, digits, hyphens, or underscores, excluding
Windows device names. Source and target paths must be UTF-8 for the JSON ownership
receipt. Tachi resolves the source to an absolute canonical path.
Choose a stable source location; moving it leaves a broken link. The same source
can serve multiple native targets, and removing one target does not affect the
source or other targets. Host-specific Skill names, frontmatter, and trust rules
still apply; linking does not validate or grant permission to execute scripts.

## Host locations

These are documented native locations, verified against the official host docs
on 2026-11-07. Pick the scope you intend; the CLI does not infer one.

| Host | User scope | Project scope | Official documentation |
| --- | --- | --- | --- |
| Codex | `$HOME/.agents/skills` | `.agents/skills` in the repository hierarchy | [Codex Skills](https://developers.openai.com/codex/skills/) explicitly supports symlinked Skill folders. |
| Claude Code | `$HOME/.claude/skills` | `.claude/skills` | [Claude Code Skills](https://code.claude.com/docs/en/skills) supports symlinked Skill folders. Its reserved names include `synced` and, outside plugins, `anthropic-skills`. |
| Gemini CLI | `$HOME/.gemini/skills` or `$HOME/.agents/skills` | `.gemini/skills` or `.agents/skills` | [Gemini CLI Skills](https://geminicli.com/docs/cli/skills/) documents the `.agents/skills` aliases and native Skill linking. |

Codex and Gemini can share the same `.agents/skills` target; no second link is
needed for the same name in that directory. This command makes no claim about
desktop/cloud applications or about other hosts' discovery rules. Select another
host's target only when that host supports directory links and `SKILL.md`.

## Read-only status inventory

`tachi skill-surface status --host antigravity,codex,factory --json` inventories
the selected hosts and canonical stores. `--home /path/to/copied-home` selects
an isolated home for diagnosis. Status never installs skills or changes host
settings; it can inspect entries managed by another projection tool.

| Surface | Status discovery roots | Report behavior |
| --- | --- | --- |
| Antigravity | `~/.gemini/config/skills` | Reads `SKILL.md` in directories and per-skill symlinks. |
| Factory user skills | `~/.factory/skills`, `~/.agents/skills`, `~/.agent/skills` | Reports each store once; the shared `agents` store lists `factory` in `hosts`. |
| Factory plugin cache | `~/.factory/plugins/cache/**/skills` | Separate `plugin` store; plugin names do not participate in canonical same-name drift groups. |
| Codex projection | `~/.codex/skills` with `.agents-codex-projection.json` | Verifies the marker's source against the same-name canonical `~/.agents/skills` directory before comparing projected content. |

Factory `plugin.enabled` is the boolean from user `~/.factory/settings.json`
`enabledPlugins`; `enablement_scope` is `user_settings`. Missing, invalid, or
unbound settings produce `null` (human output: `unknown`). Cache presence does
not establish effective runtime activation: organization and project policy are
outside this home scan. The cache layouts `marketplace/plugin/revision/skills`
and `marketplace/@scope/plugin/revision/skills` identify the corresponding
`plugin@marketplace` setting. Other discovered layouts retain their paths with
unknown identity. The scan does not recurse through cache directory symlinks.
See [Factory plugin settings](https://docs.factory.com/harness/plugins).

Codex projection normalization follows
[agents-sync at af0b943](https://github.com/kckylechen1/agent-control-plane/blob/af0b943a3bd40836d4b9aa2caa28f7c8abba32ec/shared/bin/agents-sync):
retain canonical `name` and `description` frontmatter (including continuation
lines), remove other frontmatter, and preserve the body. A matching result is
`projected`; body or retained-frontmatter changes are `drift`; an invalid marker
or mismatched source is `unverified`. Raw `hash` remains available. Only verified
projections get a canonical `comparison_hash` for drift comparisons. This
reports existing projections and does not replace `agents-sync`.

## Ownership and conflicts

The explicit target contains a private `.tachi-skill-links/` metadata directory.
Each installed Skill has an ownership receipt containing its canonical source,
the link target spelling read back from the filesystem, the target directory,
and the filesystem identity of the link. Keep this metadata;
links without receipts are deliberately treated as unmanaged. Do not copy these
receipts between directories or machines.

| Existing state | `link` | `unlink` |
| --- | --- | --- |
| No native entry or receipt | Create a directory symlink and receipt | Report `already_removed` |
| Tachi link with the same source and identity | Report `unchanged` | Remove only that link and its receipt |
| Real directory/file or unmanaged symlink, including one to the same source | Refuse and preserve it | Refuse and preserve it |
| Receipt exists but the native entry was replaced | Refuse and preserve the replacement | Refuse and preserve the replacement |
| Source moved or disappeared, but the owned link is intact | Fail source validation | Remove the owned dangling link; do not resolve or touch its old source |
| Receipt exists but the link is absent | Require explicit `unlink` before reinstalling | Clear the stale receipt |

`links` is read-only and reports `linked`, `broken`, `invalid_source`, `staged`,
`missing`, or `changed`. It inventories only Tachi ownership receipts in the
chosen target, not unrelated native entries. Existing `status`, `sources`, and
`sync-plan` remain separate read-only inventory commands.

One stable lock per target serializes Tachi mutations; a competing operation
fails promptly with a busy error. Installation stages a link before publishing
it with an exclusive rename. Unlink moves the entry into private staging,
rechecks its identity, and only then removes the symlink. A replacement detected
after the move is restored if its original name is free, otherwise preserved in
staging with its location in the error. No recursive deletion is used.

## Interrupted operations and platform boundaries

A matching staged entry with a receipt can be completed by rerunning `link`, or
removed by explicit `unlink`. An absent native and staged entry can be cleared by
`unlink`. Invalid receipts, unknown staging content, or simultaneous native and
staged entries fail closed. Inspect those files manually; no `--force`, automatic
adoption, or cleanup of unknown content is provided. A crash before a receipt is
durable can leave an incomplete private staging directory requiring inspection.
The source contents are unaffected. Target parents and metadata must remain
owner-controlled throughout an operation; this is not a sandbox against another
process deliberately modifying the private metadata directory.

Linux and macOS use directory symlinks and exclusive rename primitives. On Unix,
the target and metadata must belong to the current user and must not be writable
by group or others. Metadata directories are created with mode `0700` and files
with mode `0600`.

Windows uses real directory symbolic links (`symlink_dir`), reparse-point-aware
link identity, exclusive `MoveFileExW`, and nonrecursive symlink removal. It
requires a filesystem with stable file IDs and permission to create symbolic
links (for example Developer Mode or appropriate account privileges). Failure is
reported; there is no junction or copy fallback. Windows metadata inherits the
chosen target's ACL, so choose an owner-controlled directory. Linux validation
does not establish Windows runtime acceptance; the same behavioral tests must
run on a Windows host with symlink creation enabled. Other platforms are refused.

## Legacy script migration

`scripts/tachi-skill.sh` now exits with migration guidance before any writes.
Its old copy, direct SQLite registration, and directory deletion paths are gone.
Legacy links have no new ownership receipt, so the CLI will not adopt or remove
them. Inspect each old native entry, preserve any directory contents, and move
that entry aside before using `link`. Hub registration is a separate owner
operation; native installation does not write a database or add an MCP surface.

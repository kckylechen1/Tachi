# Legacy local entrypoints

These scripts are manual operator tools. They do not deploy or restart services.

## Hub registration and client setup

`scripts/register_mcps_to_hub.py` and `scripts/load_skills_to_hub.py` retain their legacy names and planning options. First-party Tachi MCP stdio is retired, so their write modes refuse before starting a subprocess, changing Hub state or synchronizing client files. `--dry-run` prints definitions and visibility/scope policy for manual review. Third-party Hub MCP definitions may still use stdio.

Migrate supported operations with the owner CLI (`tachi hub` / `tachi mcp`). Those commands do not reproduce every batch scope/policy option; review and migrate such settings manually using supported capabilities. Registration, review and enablement remain separate decisions. Do not replace scoped registration evidence with an unscoped enable/disable write, which can target a same-ID project shadow.

Generate ordinary HTTP client configs separately:

```sh
python3 scripts/setup_agent_mcp.py --agent cursor \
  --url http://127.0.0.1:6919/mcp --apply
```

The setup helper defaults to project scope and the calling workspace; `--workspace-root` selects an explicit directory and `--config-scope user` selects a user config without inferring a workspace. It preserves unrelated direct MCP entries, normalizes legacy Tachi names and creates per-file backups. Only hosts with verified native HTTP configuration are supported. Explicit stdio selection refuses before writes; there is no proxy fallback or automatic consolidation/removal of direct entries.

## Root build installer

`./build.sh` remains the compatibility installer for `tachi-server` and
`tachi-clean`. `tachi-server` and the product/PATH alias `tachi` compile the same
`src/main.rs`; the default `~/bin/tachi` link points to the installed server
binary. `scripts/dev-install.sh` remains the development product installer with
its existing release literal gates and real-file `~/bin/tachi` installation.

The root installer uses Python 3 and the executable paths in the successful
Cargo build's JSON artifact messages. Cargo resolves `CARGO_TARGET_DIR`, Cargo
configuration and target triples; stale `repo/target/release` binaries are never
a fallback. Cross-compiled binaries that cannot execute locally fail their staged
version/help check before replacing installed files.

`INSTALL_BIN`, `INSTALL_CLEAN_BIN`, `TACHI_LINK`, `CLEAN_LINK`, and `SKIP_LINK=1`
retain their existing meanings. All active destinations must be distinct files
or symlinks, with no directory or ancestor/alias overlap. Names that differ
only by case or canonical Unicode composition are conservatively ambiguous,
including absent entries and parent/ancestor spellings, and are rejected on all
hosts. Parent symlinks are resolved before comparison. Existing symlinks are
backed up as symlinks and their referents are not overwritten. Both executables
and optional links are staged before replacements start. A detected replacement
or installed-binary check failure restores earlier entries; successful installs
retain named backups and print their paths. Replacing a pre-existing real
`~/bin/tachi` retains that file as a backup before installing the compatibility
link.

Each individual replacement is atomic. The four entries are not a filesystem
transaction: power loss, process termination, concurrent installers or unrelated
writers are outside the rollback guarantee. Use one installer at a time. A
rollback filesystem error is reported with the retained backup path. No service
activation or running-process identity is established by this installer.

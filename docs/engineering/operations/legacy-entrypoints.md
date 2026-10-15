# Legacy local entrypoints

These scripts are manual operator tools. They do not deploy or restart services.

## Hub registration and client setup

`scripts/register_mcps_to_hub.py` preserves its legacy name and options. It never
selects a privileged profile for you. An explicitly authorized operator can use
`TACHI_PROFILE=admin python3 scripts/register_mcps_to_hub.py --no-sync-agent-config`
to register the three built-in MCP definitions. Registration leaves MCPs pending
review; it does not approve, enable, or establish that they are callable.
`--disable-hidden` retains its compatibility role by requiring a hidden
registration's scoped response to explicitly confirm `enabled=false`. It never
calls the unscoped `hub_set_enabled` route, which could target a same-ID project
entry. Missing/contradictory disabled evidence stops immediately before later
registrations or client setup; previously completed Hub registrations remain.

After separately reviewing and enabling the definitions through the existing Hub
operator tools, use
`TACHI_PROFILE=ops python3 scripts/register_mcps_to_hub.py --sync-only --agent cursor`
(or another existing `--agent` selection) to add/sync the ordinary Tachi client
entry. `--sync-only` avoids registering them again and resetting their review state. It requires a
successful discovery with exactly one approved, enabled, explicitly callable MCP
record in the global scope for each of context7, exa, and longbridge. This proves
Hub internal readiness only: ordinary `standard` clients do not expose
`hub_call`, and capability readiness does not establish equivalent client routing.
Automatic consolidation/removal of direct MCP entries is therefore retired.
Existing context7, exa, longbridge and unrelated entries are preserved; the script
never requests `--remove-server`. The client Tachi entry keeps its ordinary
`standard` profile. The underlying setup helper retains its existing legacy
Tachi-name normalization and backups.

Registration errors, unavailable operator tools, malformed/empty discovery,
missing fields, pending review, disabled entries, and project shadows stop before
client synchronization. There is no fallback removal list. Project registration
requires `--no-sync-agent-config`: project evidence cannot authorize changing
shared client configuration. Partial registrations may remain pending in Hub if
a later request fails. Review them explicitly; the script does not roll back Hub
writes or automatically authorize them. The existing client setup helper retains
its per-file backups and failure reporting; setup across multiple clients is
not a single atomic transaction.

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

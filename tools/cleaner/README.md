# tachi-clean

Safe cleanup utility for Tachi-managed worktrees and build artifacts.

Current scope:

```bash
tachi-clean sweep                   # dry-run stale marked worktrees
tachi-clean sweep --root /tmp --json
tachi-clean sweep --force           # remove candidates with git worktree remove
tachi-clean tachi                   # dry-run by default
tachi-clean tachi --force           # remove old Tachi self artifacts
tachi-clean tachi --home /tmp/tachi --json
tachi-clean target [path]            # dry-run by default
tachi-clean target [path] --force    # remove non-release build artifacts
tachi-clean target [path] --json
tachi-clean wt-register <path> --repo <repo-root> --branch <branch>
tachi-clean wt-remove <path>          # dry-run by default
tachi-clean wt-remove <path> --force  # remove after safety checks
tachi-clean wt-remove <path> --json
```

`wt-register` records a Tachi-managed worktree in `~/.tachi/worktrees.json`
and writes a `.tachi-worktree.json` marker inside the worktree. `wt-remove`
refuses to remove the repository root, checks for active processes with `lsof`
when available, and prefers `git worktree remove` over direct file deletion.

`sweep` scans temporary roots for stale Tachi-managed worktrees that contain a
`.tachi-worktree.json` marker and are older than seven days. It does not query
GitHub; merge state must be decided upstream. With `--force`, candidates are
removed through `git worktree remove --force`.

`target` removes Cargo build intermediates while keeping top-level release
outputs. It deletes `target/debug` and `target/release/{deps,build,incremental,examples,.fingerprint}`
only when `--force` is passed.

`tachi` cleans Tachi self-maintenance artifacts under `TACHI_HOME` or
`~/.tachi`. It keeps the latest two `cleanup-backups` entries by name and
removes old `logs`, `runs`, and `.agent/claude-code-runs` entries after seven
days.

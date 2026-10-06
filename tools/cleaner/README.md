# tachi-clean

Safe cleanup utility for Tachi-managed worktrees and build artifacts.

Preferred Tachi CLI entrypoint:

```bash
tachi clean target [path]             # dry-run by default
tachi clean target [path] --force     # remove non-release build artifacts
tachi clean target [path] --json
tachi clean worktree <path>           # dry-run by default
tachi clean worktree <path> --force   # remove after safety checks
tachi clean sweep                     # dry-run stale marked worktrees
tachi clean sweep --root /tmp --json
tachi clean sweep --force             # remove candidates with git worktree remove
tachi clean tachi                     # dry-run by default
tachi clean tachi --force             # remove eligible old log files after safety checks
tachi clean tachi --home /tmp/tachi --json
```

Standalone maintenance binary:

```bash
tachi-clean sweep                   # dry-run stale marked worktrees
tachi-clean sweep --root /tmp --json
tachi-clean sweep --force           # remove candidates with git worktree remove
tachi-clean tachi                   # dry-run by default
tachi-clean tachi --force           # remove eligible old log files after safety checks
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
`~/.tachi`. It retains `runs`, `.agent/claude-code-runs`, and every
`cleanup-backups` entry: their age or terminal status does not establish that
identity receipts, evaluation evidence, or database rollback images are disposable.

Only ordinary files directly inside `logs` can be removed after seven days.
Log directories, symlinks, special entries and future-dated files are retained.
The logs directory must have clear OS holder evidence from `lsof`; missing or
inconclusive evidence prevents cleanup. The home, logs directory and each file's
identity, modification time and size are checked again before unlinking.
`--force` enables execution without bypassing those checks. On platforms without
the supported stable filesystem identity, this mode retains all files.

These checks narrow the same-user check/unlink race; they do not atomically
exclude a writer starting afterward. Holder visibility is limited to the
current OS user and namespace, as for the existing worktree cleanup guards.

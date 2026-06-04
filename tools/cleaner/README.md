# tachi-clean

Safe cleanup utility for Tachi-managed worktrees and build artifacts.

Current scope:

```bash
tachi-clean wt-register <path> --repo <repo-root> --branch <branch>
tachi-clean wt-remove <path>          # dry-run by default
tachi-clean wt-remove <path> --force  # remove after safety checks
tachi-clean wt-remove <path> --json
```

`wt-register` records a Tachi-managed worktree in `~/.tachi/worktrees.json`
and writes a `.tachi-worktree.json` marker inside the worktree. `wt-remove`
refuses to remove the repository root, checks for active processes with `lsof`
when available, and prefers `git worktree remove` over direct file deletion.

# State Portability: same OS on every machine

Status: design ratified by owner 2026-07-05 ("基本上都是用 Mac 的，放在 iCloud 下就好了" —
iCloud Drive is the v1 transport; a sync server is the future transport, same bundle).

## The promise being implemented

The product thesis: an agent's skills, MCP registrations, vault keys, dispatch cards,
memory, and eval history live in Tachi — so the owner's multi-agent workflow is
identical on every machine. Today all of that state is machine-local
(`~/.Tachi/**` SQLite + files). This doc defines how it travels.

## The one hard constraint: live SQLite never touches iCloud Drive

Putting `memory.db` (WAL mode) directly under `~/Library/Mobile Documents/` is a known
corruption class, not a style preference:

- iCloud syncs files independently — `memory.db` / `-wal` / `-shm` arrive on the other
  machine from different points in time → torn database.
- Conflict handling produces "memory 2.db" duplicate files, silently forking state.
- "Optimize Mac Storage" can evict the file to a stub while a daemon holds it open.

Therefore: **live state stays local; only immutable snapshot bundles cross machines.**
The bundle is the primitive; iCloud Drive is merely transport v1. The future sync
server consumes the exact same bundle format (transport-agnostic by construction).

## Design: `tachi pack` / `tachi restore` + iCloud as dumb transport

### Bundle format (`tachi-state-v1.bundle`, a tar.zst)

| Member | Source | Notes |
| --- | --- | --- |
| `global/memory.db` | `VACUUM INTO` snapshot | consistent single-file copy, no wal/shm |
| `projects/<name>/memory.db` | same | per named project |
| `hub.db`, ledger, eval DBs | same | every SQLite via `VACUUM INTO`, never `cp` |
| `vault.enc` | vault export | re-encrypted with a **sync passphrase** (argon2id), independent of the machine-local key |
| `cards/`, `manifest.json`, hub pack registry | file copy | reviewed overlays included |
| `meta.json` | generated | schema_version, tachi version, machine_id, generation counter, created_at, per-member checksums |

Explicit NON-members: `.tachi/runs/**` artifacts (machine-scale run documents),
worktrees, caches, logs. Portable = decisions and assets, not debris.

### Commands

- `tachi pack [--to <dir>]` — default target
  `~/Library/Mobile Documents/com~apple~CloudDocs/Tachi/bundles/<hostname>-<generation>.bundle`.
  Written atomically (temp file + rename), keeps last N=5 per machine, prunes older.
- `tachi restore [--from <bundle>]` — picks the newest bundle across machines by
  (generation, created_at); refuses if local generation is AHEAD (no silent rollback);
  requires the sync passphrase for vault; runs `tachi doctor` after unpack.
- Autopack: daemon triggers `pack` daily and after "asset" mutations (vault_set,
  hub_register, card overlay apply) with a 10-minute debounce. Memory-only churn does
  not trigger (daily is enough); WHY: asset changes are what break the "same OS
  everywhere" promise mid-week, memories degrade gracefully.

### Conflict policy: newest-wins, never merge

v1 is single-owner, multi-machine, serial use (the owner is one person). Policy:
generation counter + machine_id in `meta.json`; restore takes the newest, refuses
ambiguity (two bundles with same generation from different machines → surface both,
owner picks). **No DB-level merge in v1** — merging two SQLite memory stores is the
sync server's job (future), and pretending to merge by file-copy is how state forks.

### Failure honesty

Pack/restore failures are loud (Ops-incident pattern): a stale bundle in iCloud with
no error is worse than no bundle. `tachi status` surfaces bundle age; a bundle older
than 7 days with autopack enabled is a warning.

## Future: sync server

Same bundle, pushed to an owner-run server instead of (or alongside) iCloud; adds
cross-machine locking and real memory merge. Nothing in v1 needs rework for this —
that is the point of making the bundle the primitive.

## Sequencing

After facade trim (#495) per owner priority; independent of ship (#516). One leaf
issue when dispatched: pack/restore + autopack + status surfacing, goldens on
round-trip byte-equivalence of every SQLite member and on the refuse paths.

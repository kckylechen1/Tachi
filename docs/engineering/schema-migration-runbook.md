---
title: "Schema Migration Runbook"
summary: "Reusable operator runbook for the next EXPECTED_SCHEMA_VERSION bump, distilled from the v26/v24→v28 rehearsal (#1560) and live migration (#1581)."
category: "engineering/architecture"
organize: true
---
# Schema Migration Runbook

Reusable procedure for the next `EXPECTED_SCHEMA_VERSION` bump. Distilled from
what actually happened during the v26/v24→v28 cycle: the copy rehearsal
against all real stores (#1560) and the live production migration executed
from that rehearsal (#1581). Where those threads are silent on a step this
runbook needs, that is marked `GAP:` rather than invented — fill it in and
delete the marker the next time this runbook is used for real, don't guess
now.

Cross-references: #1119 (the typed `DbOpenContext` migration-authority gate
this whole procedure is built on top of), #1560 (rehearsal acceptance and the
owner ruling on kill-mid-migration), #1581 (the live v28 migration record this
runbook is transcribed from).

## 1. Pre-flight

**Enumerate every store before touching anything.** Two independent sources,
because neither alone is complete:

- `tachi migrate` (plan-only, no `--apply`) walks the global DB (hardcoded,
  not manifest-derived — `enumerate_known_libraries` in
  `crates/tachi-server/src/bootstrap/migrate_cli.rs`), the current workspace's
  project DB, and every manifest-addressed named project, reporting each
  one's `GapStatus` against `EXPECTED_SCHEMA_VERSION` with zero writes (a
  read-only immutable-URI probe — see the module doc at the top of
  `migrate_cli.rs`).
- A manual audit of `~/.tachi/manifest.json` against what is actually running.
  **Known blind spot (#1574, open at the time of writing): the global store
  is absent from `manifest.json` on at least one production machine** — of 20
  manifest entries, zero point under `~/.tachi/global/`. `tachi migrate`
  survives this because it enumerates the global DB by hardcoded convention,
  not by reading the manifest, but any other manifest-driven surface built
  later will silently skip the global store the same way `tachi status` does
  today. Do not trust manifest enumeration alone to be the full store list;
  cross-check against every daemon actually observed running (`tachi status
  --all-dbs`, `ps`) before calling the pre-flight enumeration complete.
- GAP: no documented disk-space preflight check (threshold, command, or
  which volume) exists in either source thread. Before backing up N stores,
  confirm free space on the backup target covers at least the sum of the live
  store sizes — the exact command/threshold is not recorded from the v28
  round and needs to be decided fresh.

**Backup naming.** The v28 live migration wrote backups to
`~/.tachi/pre-v28-backup-20260803/` — i.e. `~/.tachi/pre-v<NEW_SCHEMA_VERSION>-backup-<YYYYMMDD>/`. Use that same
`pre-v<N>-backup-<date>` shape for the next bump so the directory name alone
tells you which migration it predates and when it was taken.

**Binary preservation.** The v28 round kept the previous daemon binary as
`tachi.pre-v28` in the Homebrew Cellar (i.e. `tachi.pre-v<OLD_SCHEMA_VERSION>`)
before swapping in the new one. Do this before the live swap, not after —
the whole point is having a known-good binary on hand that still speaks the
pre-migration schema if rollback is needed (see §5).

GAP: the exact preservation mechanism (a `cp`/`brew` step, and whether it was
scripted or manual) is not spelled out in either thread — only the resulting
artifact name and location. Confirm the current Homebrew Cellar layout before
relying on the same path shape.

## 2. Rehearsal on copies

Rehearse against copies of every real store enumerated in §1, never the live
files. The v28 rehearsal covered ten copies (global, sigil, quant, clanker,
zeroclaw, yaya, antigravity, hapi, split_brain_repo, tachi-selfhost — #1560)
before the live round narrowed to the real production set (§4).

Per-copy acceptance, both required:

1. **Row-count parity.** `memories` and `memory_edges` counts must match
   pre- and post-migration, with the *only* permitted delta being boot-time
   idempotent seeding on a copy whose `serve` path had never run before (i.e.
   a copy that gets first-run seed rows it wouldn't have gotten from a copy
   of a store that was already live). Any other delta is a rehearsal
   failure — stop and diagnose before touching a real store.
2. **Spot-id byte-compare.** Pick sample row ids from before the migration and
   byte-compare their stored content after. The live v28 round did this with
   three sample ids per store (#1581); the rehearsal spot-checked ids across
   all ten copies (#1560).

**Old-binary refusal check.** After migrating a copy forward, confirm the
*previous* (pre-migration) binary refuses to open it, with the literal error
shape from `check_schema_version_gate`
(`crates/memcore/src/db/migrations.rs:281-289`):

```
Invalid argument: db schema version <NEW> newer than supported <OLD_BINARY_EXPECTED>
```

The v28 rehearsal confirmed this exact shape: `Invalid argument: db schema
version 28 newer than supported 17` against the pre-migration binary
(#1560). If the old binary opens a migrated copy without refusing, the
`check_schema_version_gate` hard-fail (`stored > EXPECTED_SCHEMA_VERSION`) is
not doing its job — do not proceed to a live migration until that's
understood.

**No source database is ever written to during rehearsal.** Every operation
in this section runs against a copy; the real store is only touched in §4.

## 3. Controlled kill-mid-migration — STANDING GATE for the next bump

**Owner ruling, 2026-08-04 (#1560), both halves:**

1. *For v28: won't-do, retroactively.* Production was already migrated by
   the time this was flagged (all nine stores, row counts preserved, backups
   retained) — a controlled kill-mid-migration rehearsal against an upgrade
   that already happened would verify nothing that matters now.
2. *For the future: standing requirement, not optional.* **The next schema
   bump's rehearsal must include a controlled kill-mid-migration test as a
   gate item.** The v28 round only has accidental evidence for this
   property — an early SIGTERM landed on the three largest copies mid-run
   and demonstrated the gate held (logged, no version bump, no row-count
   drift, no partial state) — but that is the reason to believe the
   controlled test would pass, not a substitute for running it.

**For the next bump, this means:** before the live swap, deliberately kill
the migrating process mid-transaction against a rehearsal copy (not a real
store) and confirm the same three properties the accidental SIGTERM
demonstrated: the gate/log line fires, `PRAGMA user_version` does not
advance, and row counts do not drift. `run_data_migrations` and
`check_db_open_context_gate` are transactional (`BEGIN IMMEDIATE`, commit
only after every migration and the version stamp succeed —
`crates/memcore/src/db/migrations.rs:438-448`), so a mid-transaction kill
should leave the copy exactly at its pre-migration stamp; this test is what
confirms that design intent under a real kill signal instead of only under
test-harness assumptions.

GAP: no documented mechanism exists yet for deterministically hitting the
migration window with a kill signal (the v28 note explicitly says "the
window is not deterministically hittable" — #1560). Whoever runs this gate
next needs to either find or build a way to pause/slow the migration long
enough to land the kill inside the transaction, rather than relying on
another accidental SIGTERM.

## 4. Live-swap sequence

Once §§1-3 pass on copies, on the real stores:

1. **Take backups per §1's naming convention**, one per real store, before
   any write.
2. **Stop the daemon via launchd, never a bare kill.** The daemon is
   launchd-managed with `KeepAlive` (`~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist`,
   `RunAtLoad` + `KeepAlive` — `docs/INSTALL.md`). A bare `kill` gets
   respawned by launchd (rate-limited by its ~10s default `ThrottleInterval`)
   before you've done anything, and a respawn racing your migration write is
   exactly the kind of concurrent-writer scenario `tachi migrate --apply`'s
   liveness pre-check exists to prevent (`crates/tachi-server/src/bootstrap/migrate_cli.rs`,
   module doc: "a live daemon's `FoundryScheduler` polls (and can write to)
   every manifest-listed DB"). Stop it properly:

   ```bash
   launchctl bootout "gui/$(id -u)" ~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist
   ```

3. **Grant migration authority explicitly, per #1119 — never via process env.**
   Schema-migration authority is a typed `DbOpenContext { intent, migration:
   MigrationAuthority::Allow { approved_by } }` constructed at the call site
   from the `--allow-schema-migration` CLI flag
   (`crates/tachi-server/src/bootstrap/serve.rs:315-317`), fail-closed
   (`Deny`) by default. This was the #1119 redesign's whole point: migration
   capability must never be ambient process-env state that every spawned
   child inherits — it is threaded per-invocation. Concretely:
   - For the daemon itself: add `--allow-schema-migration` to the plist's
     `ProgramArguments` for this migration window only, then
     `launchctl bootstrap` it back in (see step 5). Do **not** leave the flag
     in the plist after the migration completes — #1119's closing note
     records that an earlier deploy left the flag standing in the plist and
     it had to be found and removed; the gate should only be open during the
     ritual.
   - For `tachi migrate --apply` run out-of-band instead of via the daemon:
     it constructs its own independent `MigrationAuthority::Allow` with its
     own `approved_by` provenance string, separate from the daemon's
     `--allow-schema-migration` — see the trust-boundary discipline note at
     the top of `migrate_cli.rs`. Its own liveness pre-check refuses to touch
     any library a running daemon still holds, so run it only while the
     daemon from step 2 is stopped.
4. **Per-store verification**, same acceptance as §2's rehearsal, now against
   the real stores: row counts preserved, sample ids byte-compared before and
   after. The v28 live round did this on nine real stores with three
   byte-compared sample ids each (#1581). GAP: the enumerated real-store list
   in #1581's record names eight stores (global, Sigil, Quant, hapi,
   antigravity, tachi, yaya, Split_Brain_Repo) against a stated count of
   "nine real stores migrated" — the ninth is not separately named in that
   text (two additional zero-byte unstamped stores are called out as
   explicitly out of scope, not as the missing ninth). Reconcile the full
   nine-store list against `tachi migrate`'s plan output before trusting a
   verification pass as complete next time.
5. **Restart the daemon** (flag removed from the plist per step 3) and run
   post-checks:

   ```bash
   launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist
   tachi status
   ```

   Confirm `tachi status` / `/health` report the new `EXPECTED_SCHEMA_VERSION`
   and that no store still logs a `refusing to migrate` gate warning (the
   v28 round's operational finding was exactly this: a schema-28 daemon
   binary running against a still-schema-26 global store logs `refusing to
   migrate db schema 26 -> 28 ... without explicit authority` on every vault
   key refresh until the store is migrated — #1560).

## 5. Rollback

Two artifacts make rollback possible, both captured in §1 before the swap:

- **The preserved previous binary** (`tachi.pre-v<OLD>` in the Cellar).
  Swapping it back in gives you a binary whose `EXPECTED_SCHEMA_VERSION`
  matches the pre-migration stores.
- **The backups** (`~/.tachi/pre-v<NEW>-backup-<date>/`). Restore from these
  if a migrated store itself needs to be reverted, not only the binary.

**The `schema_version_behind_binary` doctor signal is what tells you a
rollback is incomplete, not what tells you to start one.** `tachi doctor`
probes every findable store's `PRAGMA user_version` against the running
binary's `EXPECTED_SCHEMA_VERSION`
(`crates/tachi-server/src/doctor/schema_skew.rs`):

- `schema_version_ahead_of_binary` — the store is stamped *newer* than this
  binary supports. This is the rollback failure mode: you swapped the binary
  back but a store already got migrated forward and this binary will refuse
  every open of it from here on. Remediation is either deploy a binary built
  at or after that store's schema version everywhere that polls it, or
  restore the pre-migration backup.
- `schema_version_behind_binary` — the store is stamped *older* than this
  binary. Informational, not fatal on its own: the store "stays readable but
  this binary will refuse to auto-migrate it... unless the process that
  opens it was started with migration authority." Seeing this on a store you
  believed you already migrated means the migration for that specific store
  did not actually complete — treat it as a signal to re-run §4 for that
  store, not as evidence anything is broken.
- A `stored == 0` (unstamped) store is never flagged by either code — the
  gate treats an unstamped file as a build, not a migration, so this
  tripwire mirrors that boundary rather than re-litigating it.

`tachi doctor`'s probe is read-only (immutable URI open, same as
`doctor::classify`) and informational — it never blocks a scan and never
touches the DB, so it is safe to run at any point during rollback to check
progress.

## 6. Abort criteria

Stop the live migration (§4) and fall back to §5 rollback if, on any real
store:

- Row counts do not match pre-migration counts after the migration completes
  (§2/§4's parity check fails on a real store, not just a rehearsal copy).
- A sample id's byte content differs before vs. after.
- The old (pre-migration) binary does *not* refuse to open the migrated
  store with the `check_schema_version_gate` shape (§2) — if that gate isn't
  holding, a real deployed daemon elsewhere in the fleet running the old
  binary can silently corrupt its view of the store, not merely display it
  wrong.
- `check_db_open_context_gate` logs anything other than the expected
  authorized-migration success line (`crates/memcore/src/db/migrations.rs:400-407`)
  during the live run — an unexpected refusal or an unauthorized-open log
  line both mean the typed authority chain from §4 step 3 did not reach the
  process that actually touched the store.
- Any store's migration is interrupted (crash, kill, power loss) and,
  post-restart, `PRAGMA user_version` does not match either the pre- or the
  post-migration stamp cleanly (i.e. the §3 gate's "no partial state"
  property did not hold for real, not just in the accidental-SIGTERM
  evidence this runbook was written from).

GAP: no documented decision authority (who calls the abort, and whether it
can be made by the operator running the ritual alone vs. needs the owner) is
recorded in either source thread. Confirm before starting the next live
migration who is empowered to invoke this section.

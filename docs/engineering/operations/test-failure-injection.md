# Test failure injection and the SQLite trigger-DDL wall

Status: operational constraint, agent-facing; resolves the sole survivor of #1443 (owner ruling 2026-08-03). Enforcement lives in `crates/tachi-contract-tests/src/tests/docs_tests/store_trigger_ddl_census.rs` (#1456); this page is the prose form agents should read before writing tests.

## The constraint

Every `MemoryStore` connection installs `install_reserved_reference_authorizer` (`crates/memcore/src/db/open.rs`), which denies **all schema mutation** — including `CREATE TRIGGER` and `CREATE TEMP TRIGGER`. The only DDL it ever admits is a small set of byte-exact internal shapes behind tokens a fixture cannot arm (see `memcore::db::open`'s module header for the exact allowlist, `is_exact_ingest_owner_fence_temp_ddl` among them). This wall is production behavior, not test scaffolding: it is what makes reserved-reference corruption impossible through a store connection.

## Why it bites test authors

A fixture that installs a failure trigger through `store.connection()` dies at prepare time with SQLite's generic `not authorized` — **before the code under test ever runs**. The test still compiles, still runs, and asserts nothing. `cargo clippy -D warnings` and fmt do not catch this; only actually running the test does, and authors usually verify with a filter that skips it.

This exact shape landed four times in two PRs on one day (#1411 ×1, #1431 ×3), by the same author, independently. It is not a typo class; it is what a correct-looking doorway plus an unwritten constraint produces.

## The sanctioned route

Open a **second connection directly on the store's database file**. That connection carries no authorizer; install the `RAISE(ABORT, …)` trigger there, then drive the code under test through the store:

- `tachi-server` tests: `crate::test_support::with_unrestricted_fixture_connection`
- `memcore` tests: `rusqlite::Connection::open(&path)` on the store's file

When a trigger is genuinely the wrong tool:

- prefer injecting failure at the seam that owns the contract — e.g. a nonexistent name inside a batch (the #1411 remediation moved the atomicity contract to memcore this way);
- if the failure is unreachable under `IMMEDIATE` transactions, say so in a comment and keep the analysis next to the code instead of deleting it silently (the #1431 remediation).

## What enforces this

The trigger-DDL census (`store_trigger_ddl_census.rs`) scans every workspace member and pins each trigger-DDL **site** (file + enclosing symbol + statement digest). A new unpinned site is a RED; so is a pinned site that disappeared. Adding a legitimate trigger fixture means adding a `MachineProof` exemption in the census — never weakening the authorizer.

## References

- #1443 — incident, diagnosis, and the owner's direction-1 ruling (document on an agent-facing surface).
- #1456 — the census and the `MachineProof` exemption regime.
- #1411 / #1431 — the four original occurrences and their remediations.

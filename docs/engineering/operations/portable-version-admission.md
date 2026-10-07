# Portable version admission (D7)

D7 implements `portable-version-policy.md` revision 6. The shipped catalogue
is contiguous through E=39. Its last Portable migration is π=36; the fixed
rollback floor is 39. Shipped classifications cannot be changed retrospectively.

The role and profile remain write-once identity. Reading a version status grants
no authority. Profile requirements, trigger admission, private-partition
boundaries, and authorized migration backups still apply.

## Admission and artifacts

A valid Portable profile stamped 36–39 is in band. Admission runs the existing
integrity and presence checks, frozen maintenance, and, on non-fresh admission, the complete Portable
shape check; it does not run versioned migration bodies, add sentinels, or write
a version stamp. The ordinary marker-fingerprint backup fallback still applies.

An older stamped Portable store requires explicit migration authority. Every
missing Portable migration runs, including a missing key with an index below
the stored version. Product migrations through the floor receive vacuous
sentinels; later Product migrations are omitted. The output stamp is the maximum
of the input stamp, Portable projection, and supported rollback floor.

Full, unprobed, and unstamped admission keeps the prior gate and refusal order.
The advisory preflight now reads header, profile and identity in one SQLite
snapshot. The authoritative transaction checks backup coverage before identity.
Coverage uses the version and probe of the actual consistent backup (or skip
snapshot), preserving the previously accepted backup-window races.

Private images use the same checks without plaintext backups or markers. Their
production entry point always passes `Deny`: a future Portable migration still
refuses until a separately authorized sealed-image migration door exists.

## Reader decisions

| Reader | Decision |
| --- | --- |
| `store_version_status(path, requirement)` | Public, read-only admission preflight; `Fresh`, `Current`, `Pending`, `Newer`, or `Refused` |
| doctor schema skew | Typed status; a Portable band stamp is not falsely behind E |
| migrate plan/apply | Typed status; band is up to date; authorized apply reports the committed stamp |
| manifest inspection | Uses doctor; no independent scalar decision |
| Wiki corpus scan/plan/legacy/backup | Exact Full assertion before retaining the Full equality check |
| runtime's ambient v23 migration exception | Inactive at E=39; unchanged, with no new authority |
| Hyperion intake/deploy readers | Separate downstream leaf; unchanged here |

The path status probe inspects **offline** stores using an immutable read-only
SQLite URI. A nonempty WAL or rollback journal returns `Refused`, because the
main file alone cannot describe those pending changes. The probe creates no
file, sidecar, backup or marker. It is a preflight, not post-maintenance shape
acceptance. Callers must preserve their offline/liveness checks; a probe is not
a lock preventing a writer from starting later.

The public standalone migration API retains its old E-stamping behavior. At a
Product-only bump to E=40 it can stamp a Portable store 40, which a v39 binary
then refuses. Portable consumers must migrate through the admission funnel.

## Test changes and provenance

- The existing Portable v36/v37 tests now admit `Deny` without changing their
  stamp; Full assertions remain unchanged.
- Portable-server's pending fixtures use π−1 rather than E−1, because E−1
  lies in band. Their migration/refusal contract is unchanged.
- Sentinel parity is guaranteed only through the floor. The current E=39 parity
  assertions still hold. Product bodies through that floor are vacuous on the
  projected path.
- The Portable required-table inventory now includes outbox, attachments,
  harness receipts, delivery, and search generation.
- The post-spec #2035 growth fault injection now has a stronger Portable
  outcome on a converged reopen: a surviving unexpected column is refused by complete shape admission
  and the transaction rolls back, rather than returning an invalid inventory.
  Full and inline-table assertions, frozen goldens, and the injected-growth
  discrimination remain intact. The fresh memories rebuild still discards a
  pre-rebuild injected column; a converged reopen refuses its surviving column.

The pre-B policy test fixture pins gate, sentinel/object integrity, transaction
order and backup decision code from `9ee323134123f8d48c267ed81681113951804502`.
Unchanged maintenance, identity resolution and object validators are shared.
Mechanical changes are qualification, scoped supported version/catalogue,
a local backup receipt, and a transaction-entry receipt. This is test-only and
runs through the real generic and sealed-image entry points.

Complete-shape SQL comparison ignores formatting and comments. It normalizes
quoted column declarations only when the name is verified by SQLite's column
metadata. Defaults, CHECK values and predicate literals retain their bytes;
uniqueness, column metadata and index keys are checked independently. This
admits the historical inline-column / later ALTER-column quoting difference.

The historical P36 source reconstruction uses the original v36 Portable
chunks, the repaired delivery SQL at `817a673f45c8bcdef14c5ff8bf89d84ffc05aeba`
before v37, and explicitly pinned search-generation maintenance. It contains
only sentinels 1–36. The original first v36 delivery shape missing later global
claim/ack indexes is refused by existing validators; this reconstruction does
not claim that deployed image is compatible.

Classification T1a tests reconstruct each complete Full predecessor from
pinned historical base, columns, indexes, enum/search maintenance, and already
shipped addon SQL. Independent literal inventories cover every prefix table;
all Portable tables, including FTS shadows and wiki memories, are populated
and protected by the schema/content oracle. Before D3/v28 these are unprofiled
Full-shaped reconstructions. The v12 input combines the E11 base with the
complete original co-release claims table before its unique index; v16/v17
co-released, so the v17 input includes the original v16 receipt ALTER. Neither
boundary claims a separately deployed image. T1b retains its explicitly labeled
pre-profile Portable projections and the v25/v26 co-release boundary.

Version/identity corruption and rollback fixtures are synthetic except where
explicitly backed by pinned historical SQL. They establish behavior, not the
lineage of a deployed database. Historical classification fixtures document
source objects and required maintenance bridges alongside their literal SQL;
versions preceding profile/counter introduction are historical projections.

No actual Hyperion router or `ca8b0540` database was obtained here. Its deployed
sentinel/object provenance, downstream client upgrade, sealed-image migration
authority, and Full shape hardening remain the separate leaves recorded in
revision 6 §11. No installed service or production database is upgraded by this
implementation.

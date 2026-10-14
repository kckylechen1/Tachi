# Existing FTS repair and Antigravity split verification

This is the operator boundary for #2001 and #2004. A merged writer fix or a
successful copy rehearsal does not authorize or prove a production rebuild,
split, service restart, installation, or issue closure.

## Resolve the execution targets

Record the reviewed commit/tree, installed CLI version/hash, Python/SQLite
versions, effective store profile, schema version, and physical database paths.
Resolve project aliases before choosing targets; the split script's legacy
`~/.tachi/projects/<project>/memory.db` and `~/.tachi/global/memory.db` defaults
may differ from the stores used by the daemon. Use `--source-db` and one
`--destination PROJECT=DB` per planned destination. Paths must exist, be
initialized by Tachi, and refer to distinct physical databases. The script does
not create directories, initialize stores, or migrate their schemas.
For an unresolved classification, pass `--retain-project PROJECT` to keep the
entire bucket in the source. The plan reports it separately and no destination
is opened for that bucket. This explicit retention does not change classification
or edit primary rows. Use the same option for rehearsal, execution and recovery.
A plan with no remaining moves performs no cleanup writes.

Run from the reviewed repository with its existing `required-v39.json` schema
contract present. Before rehearsing writes, held transactions validate the
canonical reserved-reference guard pair for applicable schemas and both
projections for schema 39. Altered guards and unsupported FTS layouts refuse;
the tool does not recreate a missing trigger or table. Explicit legacy stores
remain supported by their pre-v23 stamp and existing standalone projections;
legacy compatibility cannot create new reserved-reference metadata.

For production `simple` FTS, Python needs extension-loading support and a
trusted, operator-built libsimple shared library from the existing pinned
dependency. Record the library's source/build provenance and hash. Pass its
path explicitly with `--simple-tokenizer-library`; the script registers the
two existing libsimple entrypoints and then disables extension loading. It
downloads nothing and does not replace `simple` with another tokenizer.
Without support, the write rehearsal refuses before any database commit.

Current reserved-reference guards remain denied: this tool cannot enable
typed mutation or schema-migration authority. A row requiring those privileges
must go through a separately reviewed supported store operation. Missing
destination columns, coercion, conflicting existing memory/edge identities,
unavailable tokenizers, and failing triggers also stop execution. It does not
overwrite a different record merely because its ID matches.

## Rehearse on private consistent copies

Take SQLite backup API snapshots of the source and **every** destination into
an operator-owned directory with mode 0700 and database files with mode 0600.
Keep private IDs, memory text, query terms, and raw logs out of public reports.
Pin the input snapshots; do not overwrite a completed verification receipt.

1. Record schema, counts and logical memory/edge hashes. Inspect both FTS
   projections for missing, orphan, duplicate and NULL IDs and field parity.
2. For #2001, run the installed CLI's `backfill-fts --db COPY --full --dry-run`,
   then `--full` on the working copy. Use an isolated fixture `TACHI_HOME` and
   the ordinary Deny migration policy. Confirm all memory/edge fields and
   schema are unchanged, both projections match the corrected writer, and
   reopening returns `PRAGMA quick_check = ok`.
3. Compare real `MATCH` ID sets using the actual tokenizer, including escaped
   keyword/entity values in controlled fixtures. Field-query gains alone are
   not a ground-truth precision/recall benchmark. Restore backup copies and
   verify the original logical hashes and query results.
4. For #2004, run the split script against copies with explicit mappings.
   `--dry-run` only reads and classifies; it does **not** rehearse destination
   writes. Applying to copies rehearses all destination writes and source
   cleanup in rolled-back savepoints before the first commit. Write locks
   keep the source snapshot and uncommitted destinations stable through this
   phase. Current rows retain all source memory/edge columns; unsupported
   cross-schema shapes refuse rather than drop fields. The existing legacy
   persons-to-entities fold is retained only for valid string arrays when
   the destination has retired `persons`.
5. Check keeper memory/edge rows, NULL-ID source rows, and keeper FTS rows and
   rowids. Each destination's moved rows must be present exactly once in both
   FTS projections in its primary-write transaction. Normal FTS uses the
   memcore JSON-array-to-terms contract, including its malformed-data fallback;
   symbolic FTS retains the stored JSON. Primary JSON is not rewritten for
   current rows. Verify destination `MATCH` and reopening.
6. Kill only an owned test child immediately after its first destination
   commit, before source cleanup. Confirm durable destination copies and an
   unchanged source, then restart against the same copies. Identical existing
   memory/edge rows are accepted; conflicting ones refuse. Verify convergence,
   then a second rerun with unchanged logical storage. Separately restore the
   entire pre-operation source/destination snapshot set and verify rollback.

## Production execution and recovery

Obtain operator authorization for the exact physical targets, accepted
candidate/library, writer/service stop and restart, full rebuild, split, and
rollback. Resolve absent destinations and conflicts before requesting an
execution approval; do not substitute a fixture destination for a live one.

Identify every process/service that writes these stores, including clients
outside the daemon. Record launchd labels, owners, executable identity and
physical path binding without reading credentials. Quiesce them through their
own lifecycle controls and verify they remain stopped. SQLite write locks do
not keep a daemon quiescent after a destination has committed. Leave readers
and writers stopped through verification and any recovery.

Take a fresh full snapshot set, repeat the approved copy preflight on that
set, and compare its inventory with the approved plan. Stop on drift. Apply
only the reviewed commands to the bound targets. Retain per-database commit
and verification receipts. Reopen and verify schema, primary/edge integrity,
both projections and actual `MATCH` results before restarting the previously
identified services. Verify the installed runtime opens those physical stores.

The split is **not cross-database crash atomic**. A failure after a destination
commit can leave temporary duplicates while the source remains intact. With
writers still stopped, either rerun the identical approved plan after verifying
its identities, or restore **all** source/destination databases from the same
pre-operation snapshot set using the SQLite backup API. Do not restore only
the source, force overwrite conflicting IDs, delete WAL files by guesswork,
or restart services before recovery is verified. A snapshot restore is itself
an authorized production write. Record the actual recovered state and reopen
checks; preserve failure evidence.

## Verification snapshot, 2026-10-13

The installed CLI rebuilt copies of 3,170 memories across three stores: 2,580
normal keyword/entity projections differed before repair and zero afterward.
Primary memory/edge hashes and schema 39 stayed unchanged. Among 212 real
field queries, two gained an ID, none lost IDs; sampled originating hits were
already present before repair. These observations do not establish a precision
or recall increase.

The repaired split moved 537 schema-39 records on private copies, including
532 to hapi, one to Sigil and three to Quant. Existing destination records and
FTS keeper rows were retained, moved rows matched both canonical projections,
and rerunning made no logical change. The remaining one-record openclaw bucket
used a test-only initialized destination. A later routing inspection found
that this record's infrastructure path matched no project prefix; a soft
content marker alone selected openclaw. Its production ownership is unverified
and it must remain in the source pending adjudication. The existing classifier
is unchanged by this repair. Routing and operator approval remain production
blockers; a fixture destination is not a production destination.
No production database was changed and no data-verification issue was closed.

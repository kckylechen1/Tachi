# Host-defined typed records

Issue: [Tachi #2105](https://github.com/kckylechen1/Tachi/issues/2105).
Consumer: [Hyperion #3564](https://github.com/kckylechen1/Hyperion-Quant-SRC/issues/3564).

## Ownership

`memcore` is the shared implementation. `portable-kernel` re-exports it with
product admin features disabled. Hypermem is the Hyperion host adapter.
ZeroClaw owns its own adapter, Soul/Profile and User Model authority. Their
vocabularies, prompts, workflows and trusted writer configuration may differ.
This capability does not migrate a ZeroClaw personality store or automatically
convert existing ordinary memories.

A host declares `RecordKind { id: RecordKindId { name, version }, key_schema,
payload_schema }`, constructs a `RecordRegistry`, and installs it through
`KernelPolicy.record_registry`. Domain terms are data supplied by the host.
The kernel has no built-in thesis, outcome, personality or execution policy.

## Admission and registration

A host that needs strict separation opens with an exact schema profile and
role, for example:

```rust,ignore
let context = DbOpenContext::create_fresh()
    .with_exact_profile(StoreProfile::PortableKernel)
    .with_exact_role("personal");
let policy = KernelPolicy {
    record_registry: Some(Arc::new(RecordRegistry::new(kinds)?)),
    ..Default::default()
};
let mut store = MemoryStore::open_with_context(path, &context)?
    .with_kernel_policy(policy);
store.register_record_kinds()?;
```

Existing files use `open_existing_deny()` or an explicit
`open_existing_allow(approved_by)` migration authority. An exact role matches
the persisted token, including its namespace; legacy role aliases do not
satisfy it. A missing or wrong role on an existing store is refused before
migration or a role stamp write. Only an explicitly fresh open may confer the
requested role. Hosts must not infer this authority from a filename.

Registration freezes the entire canonical schema contract under
`hard_state/typed_record_kinds`, once per name and version. Re-registering the
same definition is idempotent. Changing that version's definition is refused;
a new schema needs a new kind version. Generic state setters, delete, expiry
reaping, raw SQL UPDATE/DELETE and conflict replacement cannot change a frozen
definition. The registry is trusted startup configuration, not request input.

Schemas use JSON Schema draft 2020-12, compiled once per registry. Validation
is offline, validates known formats and refuses unknown formats. External HTTP
and filesystem references cannot be resolved. Key schemas are closed objects
with declared properties. Definitions and inputs have explicit size limits.

## Immutable records and revisions

`TypedRecordInput` contains `id`, `kind`, `keys`, `payload` and typed `links`.
The kernel validates it, writes canonical JSON and a content SHA-256, and adds
trusted writer provenance, creation time and revision number. A record id is
unique. Declared keys are not globally unique: multiple observations may refer
to the same subject, and a correction may preserve its logical key.

`append_typed_record(writer, input, predecessor)` performs one
`BEGIN IMMEDIATE` operation through `ImmutableSupersessionTransaction`:

1. Recheck the admitted persisted role, writer grant and host schema.
2. Check every local reference against the target's kind, keys and digest.
3. Append the new immutable record.
4. For a correction, append one source-to-target lineage row and persist the
   existing `memory.supersession.receipt.v1` receipt in the same transaction.
5. Commit all changes together, or roll back all of them.

A predecessor must exist, have the same kind name and have no other successor.
The writer needs grants for both source and target kind versions. A changed
schema version can therefore be admitted explicitly without modifying the old
record. Exact retries return the existing record/receipt; reusing an id for
changed content, writer or lineage is refused. Concurrent corrections to one
predecessor have one winner.

The predecessor row is never updated, including access statistics. Its
`superseded_by` response field is derived from the append-only lineage ledger.
The receipt consequently keeps `source_revision_before == source_revision_after`
and identifies the route as `typed_record_revision_v1`. Its event effects name
`typed_records.append` and `typed_record_supersessions.append`. Ordinary
`MemoryEntry` upsert and its existing supersession routes retain their behavior.

The two new tables use `WITHOUT ROWID`. Persistent triggers deny UPDATE,
DELETE and conflicting INSERT OR REPLACE, even inside the typed write scope.
The kind namespace also checks replacement victims by physical rowid and
refuses shadowed rowid layouts. These protections work with recursive triggers
both on and off. Open admission checks canonical trigger definitions and the
versioned schema inventory; it does not repair a missing current-version guard.
Raw SQLite writer connections follow the existing reserved-reference guard
initialization contract: install the deny-by-default function with
`db::ensure_reserved_reference_write_guard` before ordinary writes. This grants
no typed authority. Connections from the supported openers already have it.

## References

Each link uses one existing closed ontology relation and a target:

- `local`: record id, kind/version, declared keys and content SHA-256. The
  transaction verifies all fields against the actual immutable target.
- `external`: kind/version and declared keys. The target kind must be in the
  host registry and its key schema must validate. Existence, truth and host
  authorization remain the external owner's responsibility.

`supersedes` is reserved for the checked predecessor transaction. Hosts can
require additional domain relationships before calling the kernel. A reference
only kind can declare `payload_schema: false`, permitting references while
refusing locally authored copies of its facts.

## Trusted writers and server transport

`RecordWriter::from_host_config(actor, role, allowed_kinds)` is a trusted host
constructor. Its fields are private and it does not implement `Deserialize`.
Clients submit record content, never an authoritative writer or role claim.
Granting a kind requires its exact name and version. This is a host-process
trust contract; arbitrary code inside the embedding process is already trusted.

`portable-server` opts in with a startup configuration file and uses the
existing save/get tools. Its private stdio process binds the configured grant;
shared HTTP has no typed write grant. Hypermem independently binds its own
private stdio writer profiles and enforces its own required relationships.
ZeroClaw can use the library directly while retaining its separate personality
configuration and application rules.

## Upgrade and preservation

Migration 41 adds these tables and guards to both supported schema profiles.
Existing stores require the normal explicit migration authority and a backup;
fresh stores receive the current schema. The new version has its own frozen
inventory; v39 and v40 inventories are unchanged. The historical D7 tests use
an explicit test-only v39 reader reference, separate from the shipped cache;
current 39/40 to 41 migration tests run outside that scope.

SQLite image backups retain typed rows, lineage, kind definitions and receipts.
The older Hypermem memories/vector snapshot format cannot preserve them and
refuses any nonempty typed state before creating a destination. Typed records
are retrieved explicitly by id; ordinary note search is not a typed-record
search API. Neither production migration nor adoption of a new dependency pin
is implied by adding this capability.

## Verification contracts

The kernel suite covers payload/key/kind refusal, exact role, per-kind grants,
immutable SQL guards, rowid replacement victims, frozen definitions after
reopen, local-reference identity, idempotency, concurrent corrections and
receipt-failure rollback. An ordinary note remains mutable beside typed rows.

The isolated `typed_record_contract` covers a personal preference vocabulary
without admin features and the real Hypermem JSON fixture. Hypermem's own
adapter suite consumes that fixture, tests its required links, invokes actual
stdio MCP subprocesses, and verifies HTTP removes a programmatically bound
writer. Build and run evidence belongs to the candidate's CI results; these
tests being present is not itself a claim that deployment or cutover occurred.

# Remote execution V1: proposed contract

Status: proposed for #1129; not an implemented or accepted remote surface.
Implementation owners: #1171 (verified admission), #1940 (signaling),
#1934 (destination execution). Baseline: `e3a101f6a`.

This proposal turns the completed [#2175 research](https://github.com/kckylechen1/Tachi/issues/2175#issuecomment-6970102773)
into one bounded implementation target. Requirements below describe that target;
they do not grant execution authority to current local APIs.

## 1. Selected scope

One owner-controlled origin delegates one job to one enrolled destination.
There is no onward delegation, memory replication, remote Memory Lease or
arbitrary tool tunnel. The initial consumers are:

| Consumer | Job | Destination owns | Permitted context |
| --- | --- | --- | --- |
| Cloud leader → Mac | `review_revision` | Read-only native review of one repository revision | Explicit, redacted static snapshot, at most 16 KiB |
| Mac leader → DGX2 | `verify_pr` | Existing repository acceptance job for one PR/head | Task and claim references only; no agent-memory content |

The destination maps each job to an installed local policy. A caller cannot
choose a command, executable, shell, argv, environment, credential, filesystem
path, native profile or arbitrary callback URL. Repository access uses the
destination's existing authorization. Task text and repository content are
untrusted input, not permission to change the job or resource policy.

For the two-carrier criterion, this proposal selects **managed native review
and the existing externally owned DGX2 acceptance/pi lifecycle**. Codex app-server
and OpenCode ACP are adapters within the managed family; exercising both alone
does not prove external-lifecycle neutrality. This is a proposed replacement for
the historical Amp-specific condition, not a claim that the owner has accepted
that change or that either remote path already works.

## 2. Transport and deployment boundary

Use the [A2A 1.0 specification](https://a2a-protocol.org/latest/specification/),
JSON-RPC 2.0 over HTTPS, with `A2A-Version: 1.0`. The destination has a dedicated
authenticated A2A router. Do not expose or forward the existing loopback MCP
router, Vault routes or unrestricted local control surface.

The selected network shape is an owner-operated **TLS pass-through transport
relay**, reached by an outbound connection from the destination. TLS terminates
at the destination gateway; the relay cannot read or authorize application
messages. This accommodates the retained outbound-connectivity constraint
without a second application queue or controller. Relay loss means unavailable
transport; it does not mean a task failed or can be relaunched.

Before activation, deployment must bind a named operator, relay endpoint,
gateway endpoint, reconnect policy and OS isolation mechanism. This proposal
does not provision those resources or assert that this environment can reach
the owner Mac or DGX2. The current GitHub-based DGX2 workflow remains a separate
working path until its replacement passes acceptance.

## 3. Verified peers and disclosure

Use owner-enrolled, mutually authenticated TLS peers. An enrollment binds the
validated peer public key to an explicit stable `agent_identity_id`, trust
domain and allowed host binding. Hostname, AgentCard, model, carrier and a
caller-supplied identity header cannot establish that mapping. Key rotation
changes the enrollment evidence, not the principal; revocation is append-only.

The production verifier must check the TLS proof of key possession, peer
enrollment, certificate validity, current revocation and exact audience/domain
before admitting a request. The origin applies the same checks to the
destination **before sending task or context bytes**. Reject redirects and
unenrolled endpoints; a reachable AgentCard is discovery information only.

Reuse the private `TrustedAdmissionVerifier` and verified-admission writer.
Current storage accepts only `device-envelope/v1` and
`agent_identity:remote_admission`; it does not contain a production TLS verifier.
The implementation must add a reviewed, versioned **`mtls-peer/v1`** policy
alongside the existing method, with corresponding persistence/read validation.
It must not mislabel TLS evidence as an existing device-envelope receipt or
accept a client-provided `verified` flag.

The trusted gateway constructs evidence bound to its server-generated
connection ID, principal, issuer, domain and scope; it persists a digest and
secret-negative reference, never private keys or raw credential bytes. Use a
fresh server nonce per admission and retain current limits: evidence age at
admission at most five minutes, lifetime at most fifteen minutes, no future
issue time. Reconnect requires fresh admission. Current evidence and revocation
are checked at each protected read and at the existing transactional write
boundary. A still-open TLS connection cannot preserve expired authority.

**Worker separation is an activation requirement.** Gateway keys, enrollment,
admission writes and host-receipt production must be inaccessible to workers
through both files and local APIs. The existing native sandbox confines writes;
it is not network isolation or host-file confidentiality. A private HOME,
same-UID `0600` key or loopback endpoint does not establish this separation.
Use a separately protected gateway authority and verify it with a real worker;
do not add an unauthenticated signing/proxy endpoint. This bounded requirement
does not claim to implement the whole delegated-capability design in #1262.

V1 uses explicit enrolled destinations, not a presence-driven scheduler.
Connection availability is advisory. Standalone signed federated presence
remains a separate #1171 remainder and cannot grant claims or completion.

## 4. Standard messages and closed business data

Advertise two required extensions:
`urn:tachi:a2a:ext:delegation:v1` and
`urn:tachi:a2a:ext:provenance-receipt:v1`.
Both peers must negotiate them through `A2A-Extensions`; unsupported required
extensions fail before business processing. AgentCard discovery advertises
A2A `protocolVersion: "1.0"` with the JSON-RPC 2.0 binding and mutual-TLS
security scheme, not local secrets.

| A2A operation | Tachi profile |
| --- | --- |
| `SendMessage` | One new delegation, `returnImmediately: true`; returns a Task, not a free-form worker answer |
| `GetTask` | Authenticated current task and bounded host receipt snapshot; `historyLength: 0` |
| `ListTasks` | Only the caller's authorized tasks, with bounded cursor pagination, `historyLength: 0` and artifacts omitted |
| `SubscribeToTask` | Optional live updates; reconnect recovers through `GetTask`, not assumed stream replay |
| `CancelTask` | Requests destination-local cancellation; request success alone cannot assert process termination |

Every task read, subscription, cancellation and replay must bind the current
verified caller to the task's persisted **origin principal and trust domain**,
the enrolled destination and the repository/project authorization binding.
The repository grant must still be valid. A fresh connection for the same
principal may resume; another enrolled peer in the same domain may not read or
cancel the task by knowing its ID. Cancellation rechecks that binding with the
state mutation. Missing and unauthorized task IDs both return `TaskNotFoundError`;
listing filters before producing tasks, counts or cursors. V1 has no implicit
cross-principal admin or shared-owner bypass.

Each submission contains one `ROLE_USER` message with one structured
`{"data": {...}}` Part. A2A 1.0 Parts have no outer `kind` discriminator.
Task/context IDs are correlations, not authority. Reusing an existing A2A task
to submit a new instruction, text/file Parts, arbitrary metadata and unknown
business fields are outside this profile.

The Part's data is the following closed object. The business `kind` below is
inside `data`, not an A2A Part discriminator. Limits are UTF-8 bytes.

| Field | Required meaning and bound |
| --- | --- |
| `kind` | Exactly `tachi.remote_job/v1` |
| `sender`, `recipient`, `trust_domain` | Explicit enrolled principal IDs/domain; each at most 256 bytes and equal to admitted bindings |
| `idempotency_key`, `nonce` | Distinct opaque tokens, each 1–128 bytes; the nonce is not the transport-admission nonce |
| `issued_at`, `expires_at` | UTC timestamps; `issued_at` may run at most 60 seconds ahead of receiver clock (`REMOTE_CLOCK_SKEW_TOLERANCE_SECS`); `expires_at` is evaluated against receiver clock with start window at most fifteen minutes (TLS `mtls-peer/v1` verified-admission TTL is not relaxed) |
| `repository` | Canonical repository identifier in both peers' allowlists; never a caller-selected clone URL or path |
| `head_sha` | Exact immutable repository commit ID, checked against the fetched object and requested PR where applicable |
| `origin_claim` | Closed `{claim_id, transition_version}` reference in the authenticated sender's namespace |
| `job` | `review_revision` or `verify_pr`; only the latter requires a positive `pull_request` number |
| `question` | Optional review question, at most 4 KiB, only for `review_revision` |
| `context` | Optional closed static snapshot below, only for `review_revision` |

The entire decoded request is at most 32 KiB; unknown fields, duplicate JSON
keys, non-integer numeric fields and unsupported enum values fail closed. No
file/URI Parts, compressed blobs, raw transcripts, credentials or opaque nested
payloads are admitted. Error responses must not echo rejected private input.

The origin's trusted submission path confirms the current local holder,
transition revision, repository/head and owner-authorized remote job before
egress. An `origin_claim` string supplied by a worker is not proof. The
destination records that origin assertion as provenance; it never imports it
as a local claim or assumes it can query the origin's private database.

### Static context

`context` is an optional object with exactly `source_ref`, `source_revision`,
`text` and `sha256` (digest of the UTF-8 text). Its complete serialized size is
at most 16 KiB. Source references are opaque, bounded identifiers, not paths
or fetchable URLs. The origin explicitly selects an allowed source and scope,
redacts before egress and checks for secrets/private reasoning. Failure blocks
submission; it must not silently send the unredacted source or log it.

The destination treats the snapshot as read-only task input. It cannot expand
it, fetch the source, borrow a memory grant or write it to recall/export stores.
`verify_pr` rejects `context` and `question` even when empty or allegedly
redacted. DGX2 receives no agent-memory reads or content; ordinary authorized
repository checkout and acceptance artifacts are not agent-memory transfer.

## 5. Persistence, claims and at-most-one launch

Compute a SHA-256 digest over the admitted business object using
[RFC 8785 JSON canonicalization](https://www.rfc-editor.org/rfc/rfc8785).
The digest includes all business fields, including audience, claim revision,
nonce, expiry and context; it excludes JSON-RPC ID, transport admission and
A2A message ID. Redaction happens before that object is frozen. Reject malformed
input rather than silently normalizing its authority-bearing values.

At the destination, `(admitted sender, idempotency_key)` identifies one durable
submission across reconnects and projects. Same key/digest returns the original
task without relaunch; a changed digest conflicts, including changed recipient,
repository, expiry or claim. Nonce reuse with a different key is rejected within
the sender/domain scope. A retry retains the original nonce and immutable input.
Current admission is still required to read a replay. A recorded task can be
queried after its start window expires, but an unrecorded expired request cannot
start work. Reuse the existing maintenance retention policy for remote envelope
bodies: scrub terminal bodies after 90 days and unconsumed bodies 90 days after
expiry, retaining immutable header/digest/receipt tombstones for replay. Do not
create a second scheduler. Large execution artifacts retain their destination's
explicit artifact policy; their retention never extends memory grants.

The origin durably records the frozen submission before sending. Bound automatic
submission to three attempts inside the original start window, always with the
same key/digest. Exhaustion is an unresolved handoff requiring reconciliation;
it is not proof of destination refusal. Do not allocate a new key or destination
to escape ambiguity. Lost responses recover by exact replay or authenticated
task lookup. Non-retryable refusal is terminal for that submission.

Reuse and extend the existing envelope/receipt substrate deliberately:

| Existing seam | Required delta; current limitation |
| --- | --- |
| `a2a_ops.rs`, `db/schema/a2a.rs` | Current mailbox accepts only same-host self-asserted peers and two local kinds. Add reviewed remote admission/storage semantics; never spoof a local identity or silently relax the schema checks. |
| Local mailbox replay/project key | Current uniqueness includes a physical-DB-derived project key. It is not a portable remote project ID or this proposal's sender-wide key. Preserve local compatibility when adding the remote namespace. |
| WorkClaim and claimed intake | Map the allowlisted repository to the destination's local project; atomically authorize a destination-local claim/grant and resource reservation. Remote verified project binding and execution admission are missing today. |
| Requester delivery facade | Delivers existing terminal results with host binding and revisions. It is not an outbound remote-command queue and cannot be reused as one by inventing a destination field. |

Origin and destination claims retain their separate owners and revisions. The
destination rechecks current admission, local claim/grant and resource policy at
the launch boundary. It may refuse insufficient resources or stale authority.
Current canonical sealed flows reject Staff/direct launch: the remote adapter
must supply an explicit reviewed execution-admission seam, not remove that guard.

Before spawning, persist the task, authorization decision and immutable local
run correlation. A crash after spawn but before acknowledgment reconciles that
same run through its lifecycle owner. Missing or contradictory evidence stays
unknown; it must never create another process merely because `started` is absent.
Expiry before launch refuses; expiry after launch is not automatic completion or
cancellation. Runtime limits and cleanup use the destination's existing policy.
An origin claim change is not instantaneous remote revocation; cancellation and
destination confirmation are required, and partitions remain observable.

## 6. Host receipts and outcomes

Only the protected destination controller can emit authorization, started,
terminal and cleanup facts. Worker prose, stdout, self-reported completion and
A2A artifacts are untrusted result content. A receipt binds the admitted origin,
destination, task/input digest, both claim references/revisions, local run ID
when allocated, receipt ID and monotonic revision. Refusal before a local claim
or run exists records that absence rather than inventing an ID.

Retrieve host receipts over the authenticated destination channel. V1 does not
make unsigned exported JSON independently authoritative. Persist the authenticated
source/admission reference with the observation. `GetTask` returns the current
bounded receipt set, including authorization, start, terminal and cleanup facts
when present; large logs stay in destination-owned artifacts. Intermediate events
may be compacted, but their omission cannot erase these lifecycle facts.

| Observed evidence | A2A status / interpretation |
| --- | --- |
| Durable receipt of request only | `TASK_STATE_SUBMITTED`; neither destination authorization nor process start |
| Destination policy refuses | `TASK_STATE_REJECTED`, authenticated refusal, no invented start |
| Authorization accepted, spawn fails | `TASK_STATE_FAILED`, accepted authorization plus launch failure; no started receipt |
| Process start confirmed | `TASK_STATE_WORKING`, host start receipt and actual local run ID |
| Lifecycle owner confirms success/failure | `TASK_STATE_COMPLETED` / `TASK_STATE_FAILED`, host terminal receipt; cleanup reported separately |
| Cancel requested but not confirmed | Last confirmed task state plus cancellation-pending observation |
| Lifecycle owner confirms cancellation | `TASK_STATE_CANCELED`, host terminal receipt; cleanup can still be pending |
| Connection lost or required receipt absent | Origin observation unknown/reconciling; do not fabricate a terminal A2A state |

Late receipts cannot reduce the persisted revision or overwrite terminal facts.
An authenticated task snapshot is the reconciliation source; an event gap alone
is not failure. A successful process exit is execution evidence, not review or
owner acceptance. For DGX2, GitHub carries the authoritative acceptance record;
Tachi retains the receipt URL plus exact head SHA and reported verdict provenance.
It cannot turn a worker's claimed pass or a receipt for another SHA into acceptance.

Managed review uses the existing supervisor and SessionDriver. External DGX2
acceptance/pi retains its lifecycle owner and existing slot/lock policy. The
gateway must observe that owner's start/terminal/cleanup evidence; attaching a
session or ingesting a worker event alone cannot become process-control authority.

## 7. Delivery sequence and acceptance

Implement in this order, in bounded PRs under the existing issues:

1. **#1171:** production mutual-TLS verifier, versioned evidence policy and
   protected gateway authority. Prove wrong peer/domain, expiry/revocation,
   reconnect and worker key/API isolation before any remote job can be enabled.
2. **#1940:** closed A2A profile, durable submission/replay and authenticated
   receipt reads. Preserve local mailbox wire/storage behavior; demonstrate
   reconnect and lost-ack reconciliation without duplicate launch.
3. **#1934:** explicit destination project/claim/execution admission and the two
   selected lifecycle adapters. Verify rejection before disclosure/start,
   exact revision binding, local resource refusal, cancellation and cleanup.

Use existing HTTP, admission, mailbox and lifecycle fixtures for discriminating
regressions; add real transport checks where those fixtures cannot prove TLS or
authority isolation. Required cases include another enrolled peer attempting to
read/subscribe/cancel a task it does not own, changed-payload replay, cross-project
key collision, expired first submission, crash after spawn before acknowledgment,
revocation at launch, late receipts, forged worker completion and DGX2 memory
rejection. Do not treat config readback or a fabricated verifier as this evidence.

Activation additionally requires actual Cloud→owner-Mac and Mac→DGX2 runs,
including the selected external lifecycle, authenticated refusal and recovery.
Native changes retain the repository's real owner-Mac acceptance requirement.
Record endpoint/operator bindings and receipts outside the repository. Until
those checks pass, report the implementation as incomplete or unavailable;
contract review, merged code, successful transport and owner acceptance remain
distinct states.

## References

- [Identity and WorkClaim spine](identity-workclaim-spine-v1.md)
- [Current same-host mailbox](coordination-mailbox.md)
- [Verified admission boundary](../../../crates/tachi-server/src/verified_admission.rs)
- [Verified admission storage policy](../../../crates/memcore/src/db/verified_admissions.rs)
- [Native sandbox boundary](../../../crates/tachi-dispatch/src/native_sandbox.rs)
- [Requester delivery contract](../../../crates/tachi-params/src/facade/delivery.rs)
- [Driving remote-host case](https://github.com/kckylechen1/Tachi/issues/1934#issuecomment-6623989317)

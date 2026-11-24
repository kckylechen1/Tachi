# Remote task admission and refusal receipts

This implements origin submission and inbound refusal for #1940 on top of the
[mutual-TLS gateway](remote-admission.md). It validates and durably records a
bounded request, then returns an authenticated **execution-unavailable refusal**.
It does not launch a worker, allocate a local WorkClaim, reserve resources or
claim that the requested revision was fetched or verified.

## Configure the destination

The gateway keeps its existing explicit identity, certificate, private key, CA
and private home configuration. To expose the remote profile, add a public HTTPS
origin and repository/job grants to each enrolled peer that may submit or read
tasks:

```json
{
  "public_endpoint": "https://mac-gateway.example",
  "peers": [{
    "agent_identity_id": "agent-cloud",
    "host_id": "cloud-host",
    "public_key_sha256": "<enrolled DER SPKI SHA-256>",
    "repositories": [{
      "repository": "owner/repository",
      "jobs": ["review_revision", "verify_pr"]
    }]
  }]
}
```

This fragment extends the complete gateway configuration; it is not a standalone
file. Missing `repositories` means no remote task grant. A repository identifier
is an exact, case-sensitive `owner/name`, not a clone URL or filesystem path.
Grants authorize only this request/receipt surface. They do not authorize local
execution or establish an origin claim's validity.

Configuration is a process-start snapshot. To change enrollment, repository
grants, host identity or the trust domain, update the protected operator file and
restart the gateway. Removing a grant hides historical tasks from that peer as
well as refusing new submissions and replays. Key revocation remains the existing
live, durable `a2a remote revoke` operation and does not wait for restart.

The store is Product schema **46**; Portable remains **44**. A pre-existing Product
store must cross the normal explicit migration gate. Use
`tachi --allow-schema-migration a2a remote serve --config /absolute/gateway.json`
only for the operator-selected store after normal backup/deployment preparation.
Ordinary startup still denies implicit migrations. V45 extended the existing
A2A envelope/receipt pair; v46 adds `a2a_remote_submissions` for origin intent.
The origin table does not claim a destination admission or receipt before a
verified response exists. Existing local/inbound rows and constraints remain.

## Configure and submit from the origin

The origin uses its protected gateway configuration and explicitly enrolled
destination. Inbound `repositories` grants never authorize outbound work. Add
the separate outbound grant and the existing local project's exact canonical
name (the project recorded on its WorkClaim):

```json
{
  "origin_database": "/absolute/origin-home/global/tachi-memory.db",
  "peers": [{
    "agent_identity_id": "agent-mac",
    "host_id": "mac-host",
    "public_key_sha256": "<enrolled DER SPKI SHA-256>",
    "endpoint": "https://mac-gateway.example",
    "outbound": [{
      "repository": "owner/repository",
      "canonical_project": "<actual local WorkClaim project>",
      "jobs": ["review_revision"]
    }]
  }]
}
```

This is another fragment of the complete configuration. `origin_database`, when
present, must name an existing regular file under the protected `home`, owned by
the operator account and not writable by other users. It allows the origin to
reuse the normal daemon's global WorkClaims and store its submissions in the
same transaction. Without it, origin commands use `home/global.db`. Do not copy
claims into a second ledger. The gateway's Serve/Revoke authority remains in
`home/global.db`; the origin checks key retirement in **both** stores, before
reserving an attempt and again after TLS health, immediately before POST. An
unreadable authority ledger prevents sending. Both existing stores must pass the
explicit migration gate; neither command starts background workers.

Prepare a private, operator-owned `0600` JSON file containing exactly the
business object described below. Use an actual current local WorkClaim and its
current transition version, exact repository/head, and explicit key/nonce.
Select and redact any review snapshot before freezing this file. There is no
automatic memory export, reference expansion, command selection or key renewal.

```sh
tachi a2a remote submit --config /absolute/origin.json \
  --request /absolute/selected-job.json --prepare-only
tachi a2a remote resume --config /absolute/origin.json \
  --submission remote-submit-EXACT-ID
tachi a2a remote status --config /absolute/origin.json \
  --submission remote-submit-EXACT-ID
```

Omit `--prepare-only` to prepare and send in one command. Preparation checks the
actual local holder, active lease, transition version, project and expected
head, plus the operator's repository/job grant. It atomically freezes the body,
SHA-256, key, nonce, destination/host/key/endpoint and origin claim reference.
The entire framed RPC is bounded to 32 KiB. Same-key identical preparation
returns the existing submission; changed bytes or destination binding conflict.

Every attempt is committed before network I/O. The client checks the destination
certificate chain, hostname and enrolled SPKI before HTTP, then checks the
authenticated health identity/domain. It reloads operator policy and rechecks
both current key retirement and the local WorkClaim immediately before the task
POST. Revocation or claim transitions after that authorization point cannot
recall bytes already sent. Destination execution and cancellation remain a
separate admission/lifecycle seam. Redirects, ambient proxies and implicit HTTP
retries are disabled.

Each host judges the other's timestamps with its own clock, so both sides
tolerate the other clock running ahead by up to 60 seconds
(`REMOTE_CLOCK_SKEW_TOLERANCE_SECS`, one constant shared by origin and
destination). The destination admits a first submission whose `issued_at` is at
most that far ahead; the origin accepts refusal receipts stamped at most that
far ahead. Expiry is never relaxed: a first submission whose `expires_at` has
passed on the destination's clock is refused, and the 15-minute TTL bound is
unchanged. Timestamps a host writes for its own rows use the canonical
millisecond UTC form.

Automatic submission is bounded to three attempts across all CLI invocations,
inside the original start window. A 45-second durable reservation covers the
health and POST deadlines (15 seconds each); concurrent/early resumes return the
existing state. After a crashed sender's reservation expires, resume consumes
the next attempt with exactly the original bytes. Late responses are fenced by
the attempt token. No database write lock is held during network I/O.

| Origin state | Meaning |
| --- | --- |
| `pending` | Frozen; no attempt reserved. Current policy and authority still need to pass. |
| `in_flight` | An attempt was durably reserved. The destination result is not known yet; a crashed process can leave this until recovery. |
| `refused` | A verified, exactly bound destination Task and both refusal receipts were saved. This slice only recognizes `execution_unavailable`. |
| `blocked` | No attempt was made; the start window, claim or frozen destination binding prevented egress. |
| `unknown` | At least one attempt was reserved, but no valid destination outcome is established. Includes lost replies, invalid receipts and exhausted retries. |

A store that cannot answer an authority recheck is not an authority verdict. The
attempt is then recorded as `unknown` with `destination_authority_unavailable`
or `origin_authority_unavailable` and stays retryable, because no task bytes
were sent. Only a revoked key or a changed claim, binding or attempt records a
`*_authority_changed` error and stops automatic retries.

Status contains headers, counters and validated receipts, never the frozen
question/context. `retry_allowed` records the remaining automatic-retry policy;
current claim, key, grant and time checks still apply. Missing enrollment/grants
or an already revoked gateway key fail the command before allocating another
attempt; a previously prepared row remains inspectable. A permanent HTTP/RPC
refusal stops retries but remains `unknown` unless the host's exact durable
refusal Task was validated. It is not proof that an earlier attempt created no
task. Exhaustion requires authorized `GetTask`/`ListTasks` reconciliation at the
destination; do not change the key, nonce or destination to escape ambiguity.

## Wire profile

The authenticated routes are `GET /.well-known/agent-card.json` and `POST /rpc`.
The gateway never mounts the local MCP router. AgentCard advertises the A2A 1.0
JSON-RPC binding, owner mTLS, both required extension URIs and unavailable
execution. Its JSON follows the normative
[A2A v1.0.0 protobuf model](https://github.com/a2aproject/A2A/blob/v1.0.0/specification/a2a.proto),
including `securityRequirements` and `mtlsSecurityScheme`.

Every RPC requires these headers:

```text
Content-Type: application/a2a+json
A2A-Version: 1.0
A2A-Extensions: urn:tachi:a2a:ext:delegation:v1,urn:tachi:a2a:ext:provenance-receipt:v1
```

`application/json` is also accepted. Compressed bodies, unknown extension URIs,
duplicate JSON keys and requests above 32 KiB are refused. Version/extension
negotiation happens before business processing. Errors do not echo rejected
business input. Request IDs are nonnegative safe integers or bounded opaque
tokens; notifications and batches are outside this profile.

`SendMessage` accepts one `ROLE_USER` message with one `{ "data": ... }` Part and
`configuration.returnImmediately: true`. It returns `{ "task": ... }` under the
JSON-RPC result. Task/context IDs are assigned by the destination; incoming task
continuations, metadata, text/file Parts and arbitrary job fields are refused.

The business object contains exactly:

| Field | Contract |
| --- | --- |
| `kind` | `tachi.remote_job/v1` |
| `sender`, `recipient`, `trust_domain` | Exact admitted peer and destination bindings; bounded identifiers |
| `idempotency_key`, `nonce` | Distinct opaque tokens of 1–128 bytes |
| `issued_at`, `expires_at` | UTC `Z` timestamps, a positive start window no longer than 15 minutes; `issued_at` may be up to 60 seconds ahead of the destination's own clock |
| `repository` | Exact repository in this peer's configured grant |
| `head_sha` | A 40- or 64-character lowercase commit ID; recorded as input, not asserted fetched |
| `origin_claim` | `{ "claim_id": "...", "transition_version": 0 }`; revision is an integer at most 2^53−1 |
| `job` | `review_revision` or `verify_pr`, also present in the repository grant |
| `pull_request` | A positive 32-bit integer, required only for `verify_pr` |
| `question` | Optional UTF-8 text up to 4 KiB, only for `review_revision` |
| `context` | Optional closed static snapshot below, only for `review_revision` |

Optional fields must be omitted when unused; explicit null values are refused.
`verify_pr` rejects `question` and `context` even if empty. A context contains only
`source_ref`, `source_revision`, `text`, and `sha256` of the UTF-8 text, and its
serialized size is at most 16 KiB. References are bounded opaque tokens. The
gateway never fetches them or writes their contents into memory/recall stores.
Known credential patterns and hidden-reasoning delimiters are rejected. This
screening complements explicit operator selection/redaction and the origin's
current-claim egress gate; it does not automatically select or export memory.

The immutable digest covers every admitted business field, including the nonce,
start window and optional context. It excludes the JSON-RPC/message IDs and the
transport connection. The closed schema uses fixed ASCII keys, Unicode strings
and nonnegative safe integers; recursively sorted JSON therefore matches RFC
8785 on this restricted domain. The implementation does not expose a general JCS
canonicalizer or accept floating-point authority fields.

## Persistence and reconciliation

The remote namespace is distinct from local mailbox project digests. A sender's
`idempotency_key` identifies one durable remote task across repositories. An exact
replay returns that same task and its original receipts; changed business bytes
conflict. A nonce cannot be moved to a different key within a sender/domain.
Admission, recipient binding, current repository/job grant and replay writes are
checked in the same existing SQLite transaction. Expired first submissions are
refused; an already recorded task remains readable and replayable after its start
window, subject to current authority.

For a new allowed input, one transaction records the envelope, a `received`
receipt and an `authorization_refused` receipt with reason
`execution_unavailable`. The A2A state is `TASK_STATE_REJECTED`. The receipt includes
the original input digest, origin/destination, repository/head, origin claim
revision, local gateway admission reference and monotonic revision. Local claim
and run references are explicitly absent. No start, terminal-process or cleanup
fact is invented.

`GetTask` returns the current bounded receipt snapshot with `historyLength: 0`.
`ListTasks` accepts `pageSize` (1–100, default 20), `pageToken`,
`historyLength: 0` and `includeArtifacts: false`. Other filters are outside this
first profile. It filters ownership/domain/destination/host/grants before returning
tasks, totals or a cursor. The opaque cursor is an already authorized task ID;
knowing or editing a cursor grants no authority, and reconnecting as the same
principal can resume a page.

Another enrolled peer cannot get, cancel or subscribe to a task by ID. Missing
and unauthorized IDs have the same `TaskNotFoundError`. For an authorized task,
`CancelTask` returns `TaskNotCancelableError` because this slice has already
refused execution; `SubscribeToTask` reports unsupported streaming for the terminal
task. Neither manufactures a cancellation or launch outcome.

Bodies use the existing maintenance class: terminal content is scrubbed after
90 days, while immutable header/digest/receipt tombstones remain for replay.
Origin bodies use the same maintenance workflow and 90-day policy: every body
becomes eligible 90 days after original expiry; refused or blocked bodies are
also eligible 90 days after their state update, whichever condition is met first.
A batch scrubs at most 100 origin bodies, preserving immutable replay
headers and any receipt. The new maintenance class advances canonical GC plans
to policy `tachi-memory-maintenance-v3`; regenerate an older plan before apply.
Receipt JSON is authoritative only as an observation over the authenticated
gateway channel; an exported unsigned copy is not independent proof.

## Verification and remaining activation work

The existing dogfood runner exercises real TLS, protocol negotiation, closed
business input, concurrent first submission, cross-repository key collisions,
nonce reuse, a deliberately unread acknowledgment, reconnect/restart recovery,
cross-peer task/cursor access, grant removal, key revocation and no claim/execution
artifact creation:

```sh
python3 scripts/dogfood_e2e.py --tachi /absolute/candidate --only remote-admission
```

The same scenario also obtains a real WorkClaim through normal daemon MCP,
submits through the product origin CLI, drops authenticated replies, kills and
restarts the sender across its durable reservation, checks the three-attempt
ceiling, rejects mismatched receipts and changed endpoints, and exercises live
claim release and gateway-key revocation across separate existing stores.
Its TLS-terminating fault proxy is test infrastructure; deployment still uses
the separately reviewed pass-through relay boundary.

This code does not enable remote execution. The following remain required:

- Destination repository-to-project mapping, fetched-object/PR head checks,
  local claim/grant/resource reservation and an explicit execution admission
  adapter that preserves canonical sealed-flow guards.
- Managed native and externally owned DGX2 lifecycle adapters, with truthful
  start, cancellation, terminal and cleanup evidence and crash reconciliation.
- Named operator/relay/endpoints, gateway-versus-worker OS/API isolation and
  actual owner-Mac/DGX2 acceptance. A private same-UID key does not prove this.

The existing GitHub-owned DGX2 acceptance workflow remains in service. These
refusal receipts do not grant merge or owner acceptance and do not replace it.

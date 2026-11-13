# One mailbox: project coordination

Implementation contract for #2232 and #2252 phases 2–3. Phase 1 was accepted
in #2238. This change extends the existing mailbox and identity admissions;
there is no separate sticky store, second registry, or general message bus.

## Wire and admission

| Action | Addressing and effect |
| --- | --- |
| `respond` | Existing `turn_response/v1`, explicit recipient and existing reply subject rules; unchanged default seven-day TTL. |
| `send` | `coordination/v1`, explicit recipient, `coordination:<opaque-id>` subject, explicit `ttl_days` in the existing 1–30 day range. |
| `status` | Header/receipt inventory for the caller; never consumes. Legacy rows plus coordination rows in the admitted project. |
| `receive` | Deliberate turn-boundary intake, bound project required; atomically accepts/consumes eligible rows and returns their bodies. |

Send/receive require the host-supplied identity and project/workspace binding.
The server records the resolved physical database digest on the existing local
identity admission. An alias and canonical project address therefore share a
scope. No tool parameter can override that scope. Recipients must have a
historical local admission in the same scope; absent and ineligible recipients
receive the same denial. Offline recipients remain eligible after admission.
The trust level remains local self-assertion, not remote authentication.

`send` adds only one closed kind. Subject and replay keys are bounded opaque
identifiers. The existing 4096-byte body bound, private-reasoning scrub and
credential rejection apply before persistence. Bodies are stored only in
`a2a_envelopes`, never in memories, recall, FTS, enrichment or memory export.
An explicit separate memory save remains an independent action.

Schema 42 is a Product migration after the inspected schema-41 catalogue.
It preserves all reply bodies, digests, IDs, timestamps and receipt chains,
adds a project column to existing admissions and envelopes, and scopes the
existing issuer replay constraint by project. Historical admissions remain
unbound. PortableKernel skips this Product migration.

## Delivery and replay

The replay key is `(issuer identity, admitted project, idempotency_key)`.
Same key and canonical input returns the original envelope; changed recipient,
subject, scrubbed body or TTL conflicts. A changed connection does not change
the issuer identity. Terminal rows and body scrubbing do not free the key.

`receive` runs under one SQLite immediate transaction. Expiry at the exact
deadline wins over consumption. Competing consumers have one winning body
delivery and one receipt chain. Status reports persisted states; an overdue
pending row is marked expired when a recipient intake performs its bounded
expiry pass. A status read never manufactures an expiry receipt.

`received`, `accepted`, `consumed`, and `expired` are transport facts.
`consumed` means delivery-adapter intake, not model comprehension, approval,
claim ownership, or execution. A response lost after commit is an unknown
delivery outcome; this is not end-to-end exactly-once delivery and must not
trigger blind resubmission. Receipt headers remain available.

Retention reuses the existing bounded maintenance scrub: terminal bodies are
eligible after 90 days; unread coordination bodies are also eligible 90 days
after expiry even if the recipient never returns. Immutable header/digest and
receipt tombstones remain for replay. GC execution is the existing operator
maintenance path, not a new scheduler. Legacy reply retention is unchanged.

## Independent Claude Code adapter

`scripts/tachi_mailbox_hook.py` implements the documented synchronous
[`UserPromptSubmit`](https://code.claude.com/docs/en/hooks#userpromptsubmit)
hook. It reads one message per turn to keep context bounded and emits
`hookSpecificOutput.additionalContext`. The host retains its lifecycle.

Set `TACHI_MCP_URL` to the existing loopback `/mcp` endpoint and
`TACHI_AGENT_IDENTITY` to the **same independent session identity as the MCP
connector**. Do not put one shared identity in a global multi-session connector.
The hook uses the host event's `cwd` for workspace binding, ignores the prompt,
and never sends it to Tachi. A configuration example:

```json
{
  "hooks": {
    "UserPromptSubmit": [{
      "hooks": [{
        "type": "command",
        "command": "python3 /absolute/path/to/Tachi/scripts/tachi_mailbox_hook.py",
        "timeout": 20
      }]
    }]
  }
}
```

This is an installation example, not a claim that a live host was modified.
The adapter permits only literal loopback HTTP endpoints, refuses redirects,
does not retry a potentially consumed response, and never logs message bodies.
Failure leaves the host turn running and reports an intake-unknown diagnostic.

Managed ACP between-turn delivery uses this same mailbox contract but requires
the Phase-4 controller lifecycle. This PR does not claim native ACP supervision,
write-profile migration, model-profile rerouting, or Clanker retirement.

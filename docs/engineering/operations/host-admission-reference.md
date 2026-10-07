# Current Host admission reference

After MCP initialization, read the existing Task board on that same connection:

```json
{"name":"tachi_task","arguments":{"action":"board","format":"json"}}
```

An active Host admission adds `admission_receipt_ref` to the board response.
The Markdown response exposes the same field. Treat its value as opaque and
pass it unchanged to `tachi_agent_eval(action="attach_session")`, with the
Host identity asserted during initialization. The client needs no database
access. No new action, input field or ledger is introduced (#1961).

Local self-asserted admissions expose the existing generated connection
reference. Verified admissions expose their canonical admission reference
only while the exact durable verification binding remains current. Neither
reference exposes private paths, verification evidence or credentials.

Reconnecting produces a new reference. Read it again on the new initialized
connection; old, foreign and invented references cannot authorize new
attachments or session facts. The attachment/event mutation gates remain
authoritative, including revocation and expiration checks. Reading the board
does not grant a worker capability, WorkClaim, launch or process authority.
The caller must already have an admitted profile exposing the attachment tool;
the reference does not change discovery or call-time permissions.

Uninitialized, anonymous, unavailable, expired or revoked admissions return
no reference. Existing board reads remain available under their existing
profile policy. A local reference does not assert remote verification.

The supported initialized MCP session path uses protocol `2025-11-25` (or the
other admitted legacy versions). Modern stateless requests have no initialized
connection continuity and cannot use a board reference from a different
request as connection authority. This change does not add a remote verifier
or an attachment capability grant; those retain their separate owning tasks.

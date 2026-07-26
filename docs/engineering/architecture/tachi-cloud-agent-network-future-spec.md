# Tachi Cloud Sync Space & Agent Network - Future Spec

**Status:** FUTURE / DRAFT - owner direction, not ratified canon
**Date:** 2026-07-23
**Owner direction (verbatim):** "Tachi 以后的商业模式就是提供一个 online 的空间，可以选择记忆上传，也可以选择作为一个存跨机器 agent 设置的同步设置；API key、auth token 放一次。" 以及 "让用户在 Tachi 上通过 agent phone 去找别人的 agent 问问题，或者邀请别的 agent 来解决问题。"

> This document preserves two owner-directed future directions. It is a
> non-canonical record, not an implementation plan, product commitment,
> security decision, or acceptance contract. Ratified canon wins on conflict;
> this draft grants no authority and adds no dependency, issue, or schedule.

## Relationship to existing canon

The following are relevant canon to consult if a separately ratified design
leaf is ever proposed. Their presence here does not claim compatibility,
extension, or implementation.

- [`state-portability.md`](state-portability.md) discusses portable state.
- [`vault-auth-broker.md`](vault-auth-broker.md) governs the current local
  credential boundary.
- [`security-model.md`](security-model.md) governs current trust invariants.
- [`agent-fleet.md`](agent-fleet.md) describes agent coordination.
- [`memory-soul-architecture.md`](memory-soul-architecture.md) defines the
  AgentIdentity boundary.

---

## Part A - Tachi Cloud sync space

A possible hosted, opt-in space for memory and agent-setting portability. The
owner direction does not decide whether this product exists, which data it can
hold, or whether it is paid.

### A.1 Memory sync (potentially opt-in per library)

- A future design could use state-portability bundles rather than live WAL
  files, but the sync unit and its representation remain open.
- Conflict handling, merge behavior, generation tracking, machine identity,
  and whether projections are rebuilt or merged all require a separate,
  ratified decision.
- Per-library consent, including whether project libraries may ever be
  uploaded, remains a product and security decision.

### A.2 Settings sync

- A future scope might include agent-facing configuration such as profiles,
  skill loadouts, wiki, host projection settings, and non-secret config.
- Which settings are portable, versioned, restorable, or excluded remains
  open. "New machine, one command, my whole agent setup is back" is a target
  experience, not a promised workflow.

### A.3 Credential sync ("放一次，处处可用")

The owner direction may be recorded, but it authorizes no credential-sync
design. No credential behavior, encryption scheme, device enrollment,
revocation, recovery, server-access claim, or secret-delivery path is ratified
by this draft. Whether credentials are ever in scope must be decided alongside
the recovery, abuse, payment, authority, and protocol questions below.

### A.4 Monetization

The business model, local/free scope, backup allowance, subscription scope,
team behavior, pricing, and billing are all open. This draft makes no tier or
payment decision.

---

## Part B - Agent network ("agent phone")

A possible cross-owner interaction space in which users could ask another
owner's agent a question or invite it to help on a bounded problem. This is
not approval for a directory, a network protocol, cross-owner execution, or
data sharing.

### B.1 Addressing and identity

Any addressability design must reconcile with the AgentIdentity boundary in
[`memory-soul-architecture.md`](memory-soul-architecture.md). Identity format,
directory participation, discoverability, owner domains, scope declarations,
and cost or permission terms remain open.

### B.2 Ask flow (问问题)

A future design could define a bounded question packet and an owner-side
decision point. The authority to receive, inspect, answer, disclose evidence,
or decline a cross-owner request is unresolved; neither raw-memory access nor
any particular receipt format is approved here.

### B.3 Invite flow (邀请来解决问题)

A future design could let an owner invite another owner's agent to a bounded
piece of work. Which side holds authority, what work or data may cross the
boundary, whether credentials can be re-minted, and what artifacts may return
are unresolved. This draft authorizes no cross-owner execution or access.

### B.4 Candidate security questions

The following are questions for a separately ratified threat model, not
invariants established by this draft:

1. Can any interaction avoid standing cross-owner access, and what expiry or
   revocation semantics would be required?
2. How would a foreign request remain bounded by the receiving owner's
   authority and consent model?
3. What typed refusal, audit, and failure behavior would prevent an ambiguous
   timeout or implied consent?
4. Could any scoped access be safe without credential transit, and who can
   mint, observe, revoke, or recover it?
5. How would stale directory state, forged evidence, spam, coercion, and other
   abuse be detected and handled?

### B.5 Potential rationale (unratified)

The linked canon may inform an eventual rationale around persistent identity,
evidence, portability, and a billing rail. It does not establish market fit,
security, interoperability, or a competitive outcome.

---

## Open decisions (required before any proposal can become canon)

1. **Credential scope and security:** whether credentials are ever synced;
   encryption, key ownership, service access, enrollment, revocation, and
   secret delivery if they are.
2. **Recovery:** what happens when all enrolled devices or recovery material
   are lost, including whether accepted data loss is the only safe outcome.
3. **Memory and settings semantics:** sync units, consent boundaries, conflict
   resolution, rollback, and handling of concurrent changes.
4. **Product and payment:** whether the service exists, who may use it, what
   is paid, billing, credits, refunds, and any cross-owner compensation.
5. **Cross-owner authority:** directory participation, invitation authority,
   consent, data boundaries, execution rights, receipts, and revocation.
6. **Abuse and safety:** spam, coercion, fraud, forged evidence, privacy
   leakage, incident response, and a full threat model before exposure.
7. **Protocol:** whether any network uses MCP, an HTTPS envelope, another
   protocol, or no interoperable protocol at all.
8. **Governance and accountability:** ownership, auditability, disputes, and
   operational responsibility across users and service operators.

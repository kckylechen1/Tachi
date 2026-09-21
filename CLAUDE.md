# Claude adapter

Read `AGENTS.md` first. It is the repository policy; this file only maps that policy onto Claude mechanisms and grants no additional authority.

## Native helpers

- Work directly for coherent implementation. Use native helpers only for bounded search, analysis, independent review, or a clearly isolated implementation slice.
- Give every helper its scope, relevant base or candidate, write permission, expected evidence, and stopping point. Helpers return results to the coordinating session and do not delegate.
- A helper is not automatically a formal delivery lane. Do not invent dispatch IDs, packets, or top-level sessions for ordinary helper work.

## Workspace mechanics

- Use native isolation for delegated writers when available. Otherwise follow `AGENTS.md` workspace ownership without inventing a competing worktree rule.
- Read-only helpers may inspect the owned workspace. Concurrent writers never share files or build resources whose outputs could be misattributed.

## Unavailable capabilities

- If required review, model identity, isolation, or tooling is unavailable, report the exact requirement as incomplete or infrastructure-blocked.
- Do not silently substitute another model, launcher, top-level session, or weaker evidence route.

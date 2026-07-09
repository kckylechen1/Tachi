# Decision: Node runtime floor stays at >=18 (not 22.12 yet)

**Date:** 2026-07-09  
**Status:** accepted (provisional)  
**Issues:** #853 (commander 15), #841 (dependency freshness)

## Context

`commander` 15 requires Node `>=22.12`. CI and `packages/tachi-cli` currently
target Node 20 / engines `>=18`. Raising the floor is a **repo-wide runtime
decision** (CI matrix, install docs, napi consumers), not a one-line dependency
bump.

## Decision

**Do not raise the Node floor to 22.12 in this cycle.** Keep:

- `packages/tachi-cli` `engines.node`: `>=18.0.0` (or the current CI Node 20)
- `commander` on `^12.x` until a deliberate floor migration is planned

## Consequences

- #853 remains **blocked on decision**, not on implementer effort. Closing #853
  without a floor bump means "commander 15 deferred".
- Dependency freshness (#841) may still land patch/minor Node-ecosystem updates
  that honor the current floor (e.g. js-yaml 5 under #849).
- When the floor is raised later: update CI `node-version`, engines field,
  INSTALL/release docs, and only then bump commander.

## Alternatives considered

1. **Raise to 22.12 now** — premature; no product requirement for commander 15.
2. **Dual-matrix CI (20 + 22)** — cost without user demand; revisit if a dep
   forces it.

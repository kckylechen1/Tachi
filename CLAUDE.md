# Sigil — Claude/OpenCode adapter

`AGENTS.md` is the carrier-neutral repository contract. This file adds only Claude/OpenCode mechanism mapping; it is not a second truth store and is not authority for other harnesses.

## Session shape

- A lane launched by Claude's native subagent mechanism or by an explicit Tachi staffing packet is a **dispatched lane**. Follow the packet, owned-worktree, base-SHA, scope, and report contract in `AGENTS.md` and the dispatch canon.
- A top-level session speaking directly with the owner is a **sole session**. Do not invent a packet, dispatch id, or base SHA. Work in the current checkout only when it is clean and owned.

## Claude/OpenCode mechanism mapping

- Use the harness's native subagents for ordinary local scouting, implementation, and independent review. Tachi staffing is only for the exceptions named in `AGENTS.md`; Tachi may still record native-worker outcomes.
- Apply the review qualification and scoped-addendum rules in [`dispatch-lifecycle.md` §2.4–§2.5](docs/engineering/architecture/dispatch-lifecycle.md#24-lane-selection-and-review-diversity). An unknown model is recorded as unknown, not inferred from the carrier; the adapter cannot waive high-risk review or certify itself.
- For Actions quota or runner blockage, use [`actions-capacity.md`](docs/engineering/operations/actions-capacity.md). Do not repeatedly push or rerun an unchanged blocked candidate, change billing, or relabel unexecuted checks as passed.
- Select routes from the live tool schema and current skill registry. Do not hard-code model or vendor assignments here. Capacity failure changes the route, not the frozen contract, evidence head, or authority ceiling.
- Treat transitional Tachi-owned process/session supervision code as implementation state, not the target architecture. Reconcile the current disposition of #757 and current code before changing that boundary.
- For issue work, read `issue-portfolio-governance.md`; do not derive execution from stale issue bodies or labels, and never close protected umbrellas from this adapter.

## Workspace specifics

- Native dispatched lanes use the harness-provided isolated-worktree mechanism. A Tachi-managed worktree exists only for an explicit managed-staffing exception. Do not create ad-hoc worktrees that bypass the repository's ownership rules.
- The shared Cargo target and collision rules are repository facts in `AGENTS.md` and `dispatch-lifecycle.md`; this adapter adds no second copy.

Role-invariant safety, review, verification, and delivery rules live in `AGENTS.md` and the owning canon. Claude-specific route mechanics belong in the live registry or harness-private adapter, where they can change without rewriting repository law.

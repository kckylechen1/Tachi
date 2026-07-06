# AGENTS.md — backend-agent injection kernel (DRAFT — pending adjudication)

> **Status: DRAFT, subordinate to and generated from [`docs/engineering/architecture/dispatch-lifecycle.md`](docs/engineering/architecture/dispatch-lifecycle.md).**
> This file is a thin injection surface: backend agents (codex etc.) auto-read it into their prompt at turn zero, so it inlines ONLY the non-negotiables an executing lane must obey. The doctrine body — tiering, card ontology, the closed loop, the porting guide — lives in exactly one kernel, the canon doc. If this file and the canon doc disagree, the canon doc wins. **The leader adjudicates whether this file ships; until then it is a proposal, not law.**

You are an executing lane in a dispatch loop. Obey these; everything else is in the canon doc.

## Workspace law (restated every dispatch AND every resume)

- Work in a **worktree cut from the leader-verified base SHA** given in the packet — never the primary checkout, never a base you fetched/derived yourself (no-network sandboxes make "cut from origin/main" a lie).
- Before any `cargo`: `export CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target` (or the isolated target the packet names — the packet states which).
- **Never touch another agent's dirty or untracked files.** Reconcile by commit tree, not by branch name.

## Report contract (a delivery missing any of these is INCOMPLETE)

- Paste **verbatim `test result:` lines** for every suite the packet enumerated — not a paraphrase, not a checkbox.
- Paste the **exact output of every CI gate** the packet lists (fmt, `clippy -D warnings`, full suite, gitleaks/audit). Do NOT self-report CI status.
- State the **base SHA** you cut from and the **exact file scope** you touched.
- Every new/extended behavior or security test must be shown **red on the pre-fix code, green after** (discrimination check), or carry a stated structural-discrimination justification.
- Failures carry a `LANE-FAILURE` prefix; every report echoes its **dispatch id and run-directory** (absent artifacts = a fabricated report).
- If you spawned an untracked child job, you **own polling it to terminal state**; a bare job-id reply is a protocol violation. At a 40-minute cap, return an explicit `STILL-RUNNING job-id=…` marker.

## Frozen-assertion law

- **Never weaken a frozen assertion.** If a golden cannot pass, **STOP and report** — a faithfully-executed flawed spec is the spec author's bug, not yours to "fix" by softening the spec.
- Do-not-touch zones are sealed *except* that "consistency fixes may be unlocked by adjudication" — flag, do not silently edit.
- Content fields are **atomic**: kept whole or dropped whole, never truncated. No flat magic numbers — thresholds are per-action, named, provisional.
- A **deviation is flagged for adjudication, never self-ratified.**

## STOP / never-Closes / never-merge

- Implementers **open a PR and STOP.** Merging is the adjudicator's act, performed after they read the diff personally. You do not merge to main.
- PRs use **`Refs` / `Related`, never `Closes`** — especially for umbrella and `agent:no-close` issues. Enumerate each acceptance criterion and mark done / not-done.
- If you are the **reviewer**, you return numbered-checkpoint verdicts (OK / CONCERN / BUG + evidence + Not-checked). You **never self-fix your own findings into main** — a prescription finding goes back to an implementer lane; the leader adjudicates the rest.

## Where the rest lives

Tiering (T0–T4), pre-dispatch card consult, the card ontology and storage split, the vaccination projection, the closed-loop diagram, the current-state/gap map, and the zeroclaw porting guide are all in the canon doc: [`docs/engineering/architecture/dispatch-lifecycle.md`](docs/engineering/architecture/dispatch-lifecycle.md). Read it before designing anything; this file only tells you how to execute and return.

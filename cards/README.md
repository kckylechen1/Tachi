# Lane Cards — AI 角色卡的工程孪生

A lane card is to an engineering agent what a character card is to a roleplay persona:
the persona card defines *who it is and how it speaks* (emotional value); the lane card
defines *what it's good at, where it fails, and how to constrain it* (engineering value).
Both are portable, importable, and shareable — cards are part of Tachi's portable state
(`db + vault + hub + cards`).

## Three-layer storage (frozen in docs/engineering/architecture/experience-to-card-evolution.md)

- **TOML (this directory)** = birth certificate: declaration + evidence *seed*. Hand-editable, reviewed via PR.
- **SQLite (Tachi DB)** = medical record: accumulating eval rows and error signatures, appended by the
  per-dispatch eval line (#534). Never a file.
- **Projection (computed)** = health report: hexagon, top-N counter-clauses at packet-assembly time.
  Never persisted as truth, never hand-edited.

Import path: #534 first slice parses these TOMLs to seed the `(role, vendor)` card tables;
thereafter the DB accumulates and the TOML remains the declaration layer.

## Schema `tachi-lane-card/v1`

- `[identity]` — name, vendor, model id, transport lane, roles.
- `[hexagon]` — six evidence-grown axes, 0–1: spec_fidelity, self_report_trust, test_discipline,
  equivalence_refactor, security_competence, speed_cost. Reviewer-seat cards add precision/breadth/severity_calibration.
- `[routing]` — route_to / route_away / forbidden_without_gates (high-severity domains change routing
  *topology*: e.g. security → mandatory dual-track + cross-vendor review, not just a different vendor).
- `[[failure_modes]]` — error signature, occurrence count, evidence refs, counter-clause, and
  **efficacy**: which constraint layer actually works for this failure on this vendor
  (prompt < config < packet-clause < structural-gate).
- `[constraints]` — mandatory clauses injected into dispatch packets, per domain.
- `[trust]` — global trust parameters (e.g. `self_report`) that gate how much verification the leader owes.
- `[[eval_rows]]` — seed evidence (date, task, score, verdict). The DB continues this series.

All seed data: 2026-07-05 multi-vendor campaign (9 PRs to main, cross-vendor adversarial review).

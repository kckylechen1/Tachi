# Decision: #586 audit track residual

**Date:** 2026-07-09  
**Status:** accepted  
**Issue:** #586

## Context

Third-party KIMI audit track listed CRITICAL / HIGH / MEDIUM findings. All
CRITICAL and HIGH checklist items are **closed** (verified 2026-07-09).

The only still-open linked item in the original checklist is:

- #547 Typed errors at facade/retry boundaries only — **opportunistic; campaign
  rejected** as a full rewrite.

## Decision

1. **#586 is done for release-blocking purposes.** CRITICAL + HIGH are closed.
2. **#547 stays open as opportunistic**, not a release gate. Do not reopen a
   typed-error campaign.
3. CI floors (fmt/clippy/audit paths) already advanced via prior remediation
   PRs; further hardening rides #841, not a new audit epic.

## Definition of Done update for #586

- [x] All CRITICAL closed
- [x] All HIGH closed (or postponed with risk acceptance — none remaining open)
- [x] #547 explicitly postponed (this note)
- [ ] Full medium list zeroed — **not required** for closing the track

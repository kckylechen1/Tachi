//! Branch #6 — Antigravity rescue: split a multi-project memory.db into the
//! per-project Tachi DBs (`~/.tachi/projects/<name>/memory.db`).
//!
//! Background: `~/.gemini/antigravity/memory.db` accumulated 700+ entries from
//! many different projects (hapi trading, quant analyzer, hyperion, openclaw,
//! sigil, tachi, plus residual user/kanban notes). The owner wants those
//! memories routed into their canonical project DBs so the corresponding
//! agents (and only those agents) can see them, with a hard split between
//! coding context and trading context.
//!
//! Design rules:
//!   1. **Plan-first.** `plan_rescue` produces a deterministic `RescuePlan`
//!      enumerating every source row's target DB. No writes.
//!   2. **Insert-only.** `apply_rescue` writes to target DBs only. The source
//!      DB is renamed (`.bak.<ts>`) on success but never deleted.
//!   3. **Trading isolation.** Anything routed to the `hapi` DB gets
//!      `domain = 'equity_trading'` and `scope = 'user'` so role-sandboxed
//!      coding agents do NOT match it via search.
//!   4. **Idempotent.** New row ids are deterministic (UUID v5 of source-id +
//!      target name) so re-running plan against partially-applied state
//!      surfaces collisions cleanly instead of double-inserting.
//!   5. **Schema-aware.** Target schemas may include `domain` /
//!      `retention_policy` columns that the legacy source lacks — we detect
//!      and conditionally populate them. Legacy source `persons` is folded into
//!      `entities`; new target writes do not populate the physical column.

mod apply;
mod classify;
mod plan;
mod render;
mod source;
mod types;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use self::apply::apply_rescue;
#[allow(unused_imports)]
pub use self::classify::classify;
#[allow(unused_imports)]
pub use self::plan::plan_rescue;
#[allow(unused_imports)]
pub use self::render::render_plan;
#[allow(unused_imports)]
pub use self::types::{RescueApplyReport, RescueAssignment, RescuePlan, SourceRow};

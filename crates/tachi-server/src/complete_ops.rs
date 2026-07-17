//! Task completion + eval ledger handler.
//!
//! Extracted from `tools.rs` so the watchdog in `dispatch_ops` can call
//! `handle_tachi_complete` directly without going through the MCP tool layer.

// `pub(crate)` so the non-`tachi_complete` terminal paths in `dispatch_ops`
// (backend/preflight/watchdog/cancel) can reuse `record_terminal_failure_outcome`.
pub(crate) mod dispatch_outcome;
mod eval_record;
mod flow_link;
mod handler;
mod kanban;
mod lessons;
mod mirror_eval_projection;
mod scrub;

pub(crate) use handler::handle_tachi_complete;

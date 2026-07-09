//! Task completion + eval ledger handler.
//!
//! Extracted from `tools.rs` so the watchdog in `dispatch_ops` can call
//! `handle_tachi_complete` directly without going through the MCP tool layer.

mod eval_record;
mod flow_link;
mod handler;
mod kanban;
mod lessons;
mod scrub;

pub(crate) use handler::handle_tachi_complete;

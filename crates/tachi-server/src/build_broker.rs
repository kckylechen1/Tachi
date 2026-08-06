//! Compatibility shim: build broker domain lives in `tachi-build-broker`
//! (#1702 Track T3, carve 4). Keeps `crate::build_broker::{...}` paths working
//! for `build_cli` and `bootstrap/serve/background` without renaming call sites.

pub(crate) use tachi_build_broker::{
    abandon_stale_slot, cancel_queued_ticket, pending_tickets, repo, run_next, runner, slot,
    target, ticket, ExecutorSeat, RECEIPT_NS,
};

//! Compatibility shim: build broker domain lives in `tachi-build-broker`
//! (#1702 Track T3, carve 4). Keeps `crate::build_broker::{...}` paths working
//! for `build_cli` and `bootstrap/serve/background` without renaming call sites.

#[allow(unused_imports)]
pub(crate) use tachi_build_broker::{
    abandon_stale_slot, cancel_queued_ticket, execute_ticket, load_receipt, pending_tickets, repo,
    run_next, runner, slot, target, ticket, BuildReceipt, DrainStep, ExecutorSeat, RECEIPT_NS,
};

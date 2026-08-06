// Compatibility shim for old crate::build_broker::* paths (#1702 carve-4).
// Keeps bootstrap/build_cli.rs and serve/background.rs imports unchanged.
#[allow(unused_imports)]
pub(crate) use tachi_build_broker::{
    abandon_stale_slot, cancel_queued_ticket, execute_ticket, load_receipt, pending_tickets, repo,
    run_next, runner, slot, target, ticket, BuildReceipt, DrainStep, ExecutorSeat, RECEIPT_NS,
};

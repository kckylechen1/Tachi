#![cfg(unix)]

mod common;

#[test]
fn packaged_recall_receipts_refuse_incompatible_runs() {
    common::assert_dogfood_scenario("recall_eval_receipts_bind_inputs_and_reject_drift,recall_eval_contracts_fail_closed_and_status_is_private");
}

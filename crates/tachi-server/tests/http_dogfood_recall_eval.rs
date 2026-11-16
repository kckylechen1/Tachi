#![cfg(unix)]

mod common;

#[test]
fn packaged_recall_eval_contracts_and_private_status() {
    common::assert_dogfood_scenario("recall_eval_contracts_fail_closed_and_status_is_private");
}

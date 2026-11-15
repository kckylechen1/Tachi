#![cfg(unix)]

mod common;

#[test]
fn packaged_ledger_snapshots_http_dogfood() {
    common::assert_dogfood_scenario("retired_ledger_snapshots_stay_out_of_default_recall");
}

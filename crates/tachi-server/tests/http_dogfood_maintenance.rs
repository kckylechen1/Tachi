#![cfg(unix)]

mod common;

#[test]
fn packaged_maintenance_http_dogfood() {
    common::assert_dogfood_scenario("maintenance_candidates_preserve_active_pinned_and_references");
}

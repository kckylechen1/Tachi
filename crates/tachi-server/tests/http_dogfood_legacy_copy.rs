#![cfg(unix)]

mod common;

#[test]
fn packaged_legacy_copy_http_dogfood() {
    common::assert_dogfood_scenario("interrupted_legacy_copy_refuses_then_recovers");
}

#![cfg(unix)]

mod common;

#[test]
fn packaged_store_badge_http_dogfood() {
    common::assert_dogfood_scenario("global_library_rows_are_not_labeled_legacy");
}

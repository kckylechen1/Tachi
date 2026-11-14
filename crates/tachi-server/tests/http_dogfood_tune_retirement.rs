#![cfg(unix)]

mod common;

#[test]
fn packaged_retired_tune_http_dogfood() {
    common::assert_dogfood_scenario("retired_tools_are_not_listed_or_callable");
}

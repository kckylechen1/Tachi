#![cfg(unix)]

mod common;

#[test]
fn packaged_projection_retirement_http_dogfood() {
    common::assert_dogfood_scenario("continuity_projection_retirement");
}

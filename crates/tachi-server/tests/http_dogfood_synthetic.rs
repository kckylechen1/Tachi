#![cfg(unix)]

mod common;

#[test]
fn packaged_synthetic_http_dogfood() {
    common::assert_dogfood_scenario("synthetic_recall_is_opt_in_and_real_lessons_remain_visible");
}

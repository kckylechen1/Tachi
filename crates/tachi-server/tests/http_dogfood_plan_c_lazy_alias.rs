#![cfg(unix)]

mod common;

#[test]
fn packaged_plan_c_lazy_alias_http_dogfood() {
    common::assert_dogfood_scenario("clean_clone_manifest_init_defers_plan_c_alias");
}

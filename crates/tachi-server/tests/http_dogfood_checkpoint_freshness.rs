#![cfg(unix)]

mod common;

#[test]
fn packaged_checkpoint_freshness_http_dogfood() {
    common::assert_dogfood_scenario("checkpoint_freshness_respects_agent_and_project");
}

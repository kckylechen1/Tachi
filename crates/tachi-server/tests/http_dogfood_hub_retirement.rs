#![cfg(unix)]

mod common;

#[test]
fn packaged_retired_hub_proxy_http_dogfood() {
    common::assert_dogfood_scenario("retired_hub_mcp_proxy_and_virtual_capabilities_are_rejected");
}

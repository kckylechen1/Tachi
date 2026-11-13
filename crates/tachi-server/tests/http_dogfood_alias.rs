#![cfg(unix)]

mod common;

#[test]
fn packaged_alias_http_dogfood() {
    common::assert_dogfood_scenario("repository_short_name_addresses_the_library");
}

#[test]
fn packaged_project_mailbox_http_dogfood() {
    common::assert_dogfood_scenario("project_mailbox_delivers_at_a_host_turn_boundary");
}

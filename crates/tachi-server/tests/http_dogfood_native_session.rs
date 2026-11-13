#![cfg(unix)]

mod common;

#[test]
fn packaged_native_session_supervision_http_dogfood() {
    common::assert_dogfood_scenario("native_session_supervision_and_mailbox");
}

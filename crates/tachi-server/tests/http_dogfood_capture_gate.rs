#![cfg(unix)]

mod common;

#[test]
fn packaged_capture_gate_http_dogfood() {
    common::assert_dogfood_scenario("capture_gate_warning_states_consequence");
}

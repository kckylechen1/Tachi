#![cfg(unix)]

mod common;

#[test]
fn packaged_staff_submission_recovery_http_dogfood() {
    common::assert_dogfood_scenario("lost_staff_start_response_recovers_original_run");
}

#[test]
fn packaged_prompt_boundary_http_dogfood() {
    common::assert_dogfood_scenario("recalled_data_cannot_forge_worker_prompt_sections");
}

use super::*;

// tachi#1184 item 1 / cross-vendor review C6(a): assert the literal contract
// strings actually appear in a packet rendered through the REAL renderer
// (`crate::dispatch_ops::assemble_prompt` -> `render_dispatch_profile_overlay`),
// not a hand-built string. A profile-field change with no assertion that it
// reaches the wire is exactly the gap the review called out.

#[tokio::test]
async fn glm_impl_packet_carries_the_cargo_target_dir_guard() {
    let server = make_server();
    let mut params = dispatch_params(Some("claude"), "fix a bounded bug in memcore");
    params.profile = Some("glm_impl".to_string());

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(
        prompt.contains("## Dispatch profile"),
        "profile overlay must render at all: {prompt}"
    );
    assert!(
        prompt.contains("self_managed_cargo_target_dir"),
        "glm_impl's forbidden_skills ban must reach the packet: {prompt}"
    );
    assert!(
        prompt.contains("build_through_oz_or_declared_shared_target"),
        "glm_impl's passive_traits queue-or-declare clause must reach the packet: {prompt}"
    );
}

#[tokio::test]
async fn opencode_builder_packet_carries_the_cargo_target_dir_guard() {
    let server = make_server();
    let mut params = dispatch_params(Some("claude"), "implement a bounded credentialed patch");
    params.profile = Some("opencode_builder".to_string());

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(prompt.contains("self_managed_cargo_target_dir"), "{prompt}");
    assert!(
        prompt.contains("build_through_oz_or_declared_shared_target"),
        "{prompt}"
    );
}

#[tokio::test]
async fn a_review_only_profile_does_not_carry_the_build_guard() {
    // Negative control: a non-write-capable profile (codex_55_review has
    // write_actions=false) must NOT carry a build-routing clause that makes
    // no sense for a lane that never runs cargo — proves the assertion above
    // is discriminating the two glm/opencode profiles specifically, not
    // matching on a substring every profile happens to emit.
    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "review a diff for missing tests");
    params.profile = Some("codex_55_review".to_string());

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(prompt.contains("## Dispatch profile"), "{prompt}");
    assert!(
        !prompt.contains("build_through_oz_or_declared_shared_target"),
        "a review-only profile must not carry the build-routing clause: {prompt}"
    );
}

use super::*;

// tachi#1184 item 1 / cross-vendor review C6(a): assert the literal contract
// strings actually appear in a packet rendered through the REAL renderer
// (`crate::dispatch_ops::assemble_prompt` -> `render_dispatch_profile_overlay`),
// not a hand-built string. A profile-field change with no assertion that it
// reaches the wire is exactly the gap the review called out.

#[tokio::test]
async fn codex_terra_write_packet_carries_the_cargo_target_dir_guard() {
    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "fix a bounded bug in memcore");
    params.profile = Some("codex_terra_write".to_string());

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(
        prompt.contains("## Dispatch profile"),
        "profile overlay must render at all: {prompt}"
    );
    assert!(
        prompt.contains("self_managed_cargo_target_dir"),
        "the native write profile's forbidden_skills ban must reach the packet: {prompt}"
    );
    assert!(
        prompt.contains("build_through_oz_or_declared_shared_target"),
        "the native write profile's queue-or-declare clause must reach the packet: {prompt}"
    );
}

#[tokio::test]
async fn opencode_terra_write_packet_carries_the_cargo_target_dir_guard() {
    let server = make_server();
    let mut params = dispatch_params(Some("opencode"), "implement a bounded patch");
    params.profile = Some("opencode_terra_write".to_string());

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(prompt.contains("self_managed_cargo_target_dir"), "{prompt}");
    assert!(
        prompt.contains("build_through_oz_or_declared_shared_target"),
        "{prompt}"
    );
}

#[tokio::test]
async fn native_review_packet_keeps_the_build_resource_guard() {
    // Native read-only confinement still permits validation commands. The
    // review lane must obey the same allocation policy before running them.
    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "review a diff for missing tests");
    params.profile = Some("codex_astra_review".to_string());

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(prompt.contains("## Dispatch profile"), "{prompt}");
    assert!(prompt.contains("self_managed_cargo_target_dir"), "{prompt}");
    assert!(
        prompt.contains("build_through_oz_or_declared_shared_target"),
        "native review validation must retain the build-routing clause: {prompt}"
    );
}

#[tokio::test]
async fn observation_only_profile_does_not_carry_the_build_guard() {
    // Keep a negative control: the existing UX observation profile has no
    // execution/build contract, so the renderer must not add this to all cards.
    let server = make_server();
    let mut params = dispatch_params(Some("kimi"), "review an agent-facing workflow");
    params.profile = Some("kimi_ux".to_string());

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(prompt.contains("## Dispatch profile"), "{prompt}");
    assert!(
        !prompt.contains("build_through_oz_or_declared_shared_target"),
        "an observation-only profile must not carry the build-routing clause: {prompt}"
    );
}

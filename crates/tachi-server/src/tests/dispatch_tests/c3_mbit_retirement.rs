use super::super::make_server;
use super::dispatch_params;
use crate::test_support::EnvRestore;
use crate::tool_params::{TachiAgentsParams, TachiSkillParams};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

/// #1690 slice B discriminator: the retired MBIT/card-personality surface must
/// be absent from every dispatch-adjacent response surface — the dispatch
/// receipt (default and verbose), `tachi_skill(action='loadout')`, `recommend`,
/// and the `tachi_agents` profiles projection — and from the prompt overlay.
///
/// Asserting KEY ABSENCE on the parsed JSON (not merely non-null) makes each
/// of these RED on the pre-slice-B tree, where `mbit_card` is present on all
/// four response surfaces and `render_dispatch_profile_overlay` emits
/// `mbit_card_evolution` lines.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn c3_dispatch_receipts_carry_no_mbit_card_or_dispatch_profile() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let server = make_server();

    for verbose in [false, true] {
        let mut params =
            dispatch_params(Some("custom"), "c3 mbit retirement dispatch discriminator");
        params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
        params.cwd = Some(tmp.path().to_string_lossy().to_string());
        params.unmanaged_cwd = Some(true);
        params.profile = Some("glm_impl".to_string());
        params.verbose = Some(verbose);

        let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
            .await
            .expect("dispatch should start");
        let response: Value = serde_json::from_str(&raw).expect("response JSON");
        assert!(
            response.get("mbit_card").is_none(),
            "verbose={verbose}: dispatch response must not carry mbit_card: {response}"
        );
        assert!(
            response.get("dispatch_profile").is_none(),
            "verbose={verbose}: dispatch response must not carry dispatch_profile: {response}"
        );
        assert!(!raw.contains("mbit_card"), "{raw}");
    }
}

#[tokio::test]
async fn c3_loadout_is_typed_rejected() {
    // #1690 C3: `tachi_skill(action='loadout')` is retired end-to-end. Slice B
    // guarded that the loadout response carried no mbit_card; slice C removes
    // the surface itself, so the guard becomes typed rejection of the action.
    let server = make_server();
    let result = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: Some("plan a dispatch policy change".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(50),
            skill_id: None,
            args: None,
        }))
        .await
        .expect_err("loadout must be typed-rejected post-#1690 C3");
    assert!(
        result.contains("Invalid action") && result.contains("'discover' or 'run'"),
        "loadout rejection must name the surviving set, got: {result}"
    );
}

#[tokio::test]
async fn c3_recommend_response_carries_no_mbit_card() {
    let server = make_server();
    let raw = crate::dispatch_profile::handle_dispatch_recommendation(
        &server,
        "fix a bug in the parser",
        None,
        50,
        &[],
    )
    .expect("recommendation succeeds");
    let payload: Value = serde_json::from_str(&raw).expect("recommendation JSON");
    assert!(
        payload.get("mbit_card").is_none(),
        "recommend response must not carry mbit_card: {payload}"
    );
    assert!(
        payload.get("recommended_profile").is_some(),
        "the recommendation seam itself stays: {payload}"
    );
}

#[tokio::test]
async fn c3_agents_profiles_carry_no_mbit_card() {
    let server = make_server();
    let raw = server
        .tachi_agents(Parameters(TachiAgentsParams {
            action: "profiles".to_string(),
            intent: None,
            task: None,
        }))
        .await
        .expect("agents profiles should succeed");
    let agents: Value = serde_json::from_str(&raw).expect("agents JSON");
    let profiles = agents["dispatch_profiles"]
        .as_array()
        .expect("dispatch_profiles array");
    assert!(!profiles.is_empty(), "profiles list is populated: {agents}");
    for profile in profiles {
        assert!(
            profile.get("mbit_card").is_none(),
            "verbose profile must not carry mbit_card: {profile}"
        );
        assert!(
            profile["skill_loadout"].is_object(),
            "static loadout content stays on the verbose profile: {profile}"
        );
        assert!(
            profile["evidence_contract"].is_object(),
            "static evidence contract stays on the verbose profile: {profile}"
        );
    }
}

#[tokio::test]
async fn c3_prompt_overlay_omits_mbit_card_evolution() {
    let server = make_server();
    // Seed a legacy profile/card overlay that carried the mbit-card risk
    // markers: on the pre-slice-B tree the overlay prompt renders them under
    // `- mbit_card_evolution:`; post-fix the block is gone regardless.
    server
        .with_global_store(|store| {
            store
                .set_state(
                    tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
                    "claude_plan",
                    &json!({
                        "kind": "profile_card_loadout_overlay",
                        "profile": "claude_plan",
                        "add_signature_skills": [],
                        "add_weak_against": ["plan_request"],
                        "demotion_targets": ["skill:superpowers-writing-plans"],
                        "source_proposal_ids": ["legacy-mbit-overlay"],
                    })
                    .to_string(),
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed legacy overlay");

    let mut params = dispatch_params(Some("claude"), "c3 mbit retirement prompt discriminator");
    params.profile = Some("claude_plan".to_string());
    params.stage = Some("plan".to_string());

    let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;

    assert!(prompt.contains("## Dispatch profile"), "{prompt}");
    assert!(prompt.contains("- skill_loadout:"), "{prompt}");
    assert!(prompt.contains("- evidence_contract:"), "{prompt}");
    assert!(
        !prompt.contains("- mbit_card_evolution:"),
        "the overlay prompt must not render the retired mbit_card_evolution block: {prompt}"
    );
    assert!(!prompt.contains("mbit_card"), "{prompt}");
}

/// #1690 B1 discriminator: the legacy `add_weak_against` overlay key is
/// RETIRED — rows seeded with it must be inert history, readable only via raw
/// state_kv inspection for audit, never projected into a live response. RED
/// pre-repair: the seeded weakness appears in the agents profile card's
/// top-level `weak_against` AND reaches recommend scoring as
/// `weak_against_signal:plan_request`; GREEN post-repair: neither surface
/// carries it.
#[tokio::test]
async fn c3_legacy_add_weak_against_overlay_is_inert() {
    let server = make_server();
    server
        .with_global_store(|store| {
            store
                .set_state(
                    tachi_dispatch::PROFILE_CARD_OVERLAY_NS,
                    "claude_plan",
                    &json!({
                        "kind": "profile_card_loadout_overlay",
                        "profile": "claude_plan",
                        "add_weak_against": ["plan_request"],
                        "source_proposal_ids": ["legacy-weak-against-overlay"],
                    })
                    .to_string(),
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed legacy add_weak_against overlay");

    // Agents profiles projection: the seeded weakness must not project into
    // the card's top-level `weak_against` (the static reviewed baseline stays).
    let raw_agents = server
        .tachi_agents(Parameters(TachiAgentsParams {
            action: "profiles".to_string(),
            intent: None,
            task: None,
        }))
        .await
        .expect("agents profiles should succeed");
    let agents: Value = serde_json::from_str(&raw_agents).expect("agents JSON");
    let claude_plan = agents["dispatch_profiles"]
        .as_array()
        .expect("dispatch_profiles array")
        .iter()
        .find(|profile| profile["name"] == json!("claude_plan"))
        .expect("claude_plan profile present");
    let weak_against = claude_plan["weak_against"]
        .as_array()
        .expect("weak_against array");
    assert!(
        !weak_against.iter().any(|entry| entry == &json!("plan_request")),
        "legacy add_weak_against must not project into the agents card: {claude_plan}"
    );
    assert_eq!(
        weak_against
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>(),
        vec!["direct_execution", "merge"],
        "the static reviewed weak_against baseline stays on the card: {claude_plan}"
    );

    // Recommend scoring: the seeded weakness must not reach the scored
    // reasons of a plan_request route (pre-repair it fires
    // `weak_against_signal:plan_request`).
    let raw_recommend = crate::dispatch_profile::handle_dispatch_recommendation(
        &server,
        "plan a dispatch policy change",
        None,
        50,
        &[],
    )
    .expect("recommendation succeeds");
    assert!(
        !raw_recommend.contains("weak_against_signal:plan_request"),
        "legacy add_weak_against must not reach recommend scoring: {raw_recommend}"
    );

    // Historical-artifact clause: the row is NOT deleted — it stays readable
    // via raw state_kv inspection for audit.
    let persisted = server
        .with_global_store_read(|store| {
            store
                .get_state_kv(tachi_dispatch::PROFILE_CARD_OVERLAY_NS, "claude_plan")
                .map_err(|e| e.to_string())
        })
        .expect("read state_kv");
    assert!(
        persisted.is_some(),
        "the retired add_weak_against overlay row stays readable via raw state_kv for audit"
    );
}

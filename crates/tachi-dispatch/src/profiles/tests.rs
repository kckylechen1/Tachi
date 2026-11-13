use super::*;
use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};
use tachi_params::TachiDispatchReason;

struct EnvGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, original }
    }

    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.original.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn params(profile: &str) -> TachiDispatchParams {
    TachiDispatchParams {
        staffing_reason: TachiDispatchReason::ExplicitUserRequest,
        agent: None,
        profile: Some(profile.to_string()),
        task: "dispatch test".to_string(),
        execution_level: None,
        cwd: None,
        env_id: None,
        unmanaged_cwd: None,
        skills: Vec::new(),
        context_query: None,
        model: None,
        timeout_secs: 5,
        permission_profile: None,
        allowed_tools: Vec::new(),
        completion_predicate: None,
        max_turns: None,
        sandbox: None,
        inject_tachi_mcp: None,
        inject_hub_mcps: None,
        command: Vec::new(),
        harness_transport: None,
        harness_server_url: None,
        project: None,
        stage: None,
        credential_profiles: Vec::new(),
        issue_ref: None,
        pr_ref: None,
        flow_id: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        verbose: None,
        inject_card: None,
        declared_file_scope: None,
    }
}

fn resolve_for_identity_test(params: &mut TachiDispatchParams) -> ResolvedDispatchProfile {
    resolve_and_apply_dispatch_profile(
        params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |_| false,
    )
    .expect("route resolves")
}

#[test]
fn dsh_staff_profile_selects_native_headless_without_command_model_or_credentials() {
    let mut request =
        StaffAssignmentRequest::new(TachiDispatchReason::ExplicitUserRequest, "compute");
    request.profile = Some("dsh_executor".to_string());
    let resolved = resolve_and_apply_staff_assignment_profile(
        &mut request,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |_| panic!("DSH must not probe OpenCode"),
    )
    .expect("DSH staff profile");
    assert_eq!(resolved.agent, "dsh");
    assert_eq!(resolved.host_adapter.as_deref(), Some("dsh"));
    assert_eq!(resolved.harness_transport.as_deref(), Some("dsh_headless"));
    assert!(resolved.launch_command.is_empty());
    assert!(resolved.selected_model.is_none());
    assert!(resolved.credential_profiles.is_empty());
    assert_eq!(resolved.mcp_access.inject_tachi_mcp, Some(false));
    assert_eq!(resolved.mcp_access.inject_hub_mcps, Some(false));
    request.worker = Some("opencode".to_string());
    assert!(resolve_and_apply_staff_assignment_profile(
        &mut request,
        |_| Ok(Vec::new()),
        |_| Ok(Vec::new()),
        |_| false,
    )
    .expect_err("DSH cannot silently become another carrier")
    .contains("worker override"));
}

fn receipt_key_paths(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (key, value) in object {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        out.insert(path.clone());
        receipt_key_paths(value, &path, out);
    }
}

#[test]
fn glm_profile_alias_resolves_to_current_model_card_profile() {
    let _lock = env_lock();
    let _env = EnvGuard::remove("TACHI_DISPATCH_GLM_CODING_MODEL");

    let profile = resolve_dispatch_profile("glm_impl").expect("glm profile");
    assert_eq!(profile.name, "glm_impl");
    assert_eq!(profile.role, "executor");

    let alias_profile = resolve_dispatch_profile("glm_51_impl").expect("compat alias");
    assert_eq!(alias_profile.name, "glm_impl");

    let mut params = params("glm_51_impl");
    let resolved = resolve_and_apply_dispatch_profile(
        &mut params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |_| false,
    )
    .expect("compat alias should resolve");

    assert_eq!(params.profile.as_deref(), Some("glm_impl"));
    assert_eq!(params.model.as_deref(), Some("zhipuai-coding-plan/glm-5.2"));
    assert_eq!(resolved.selected_profile.as_deref(), Some("glm_impl"));
    let receipt = &resolved.identity_receipt;
    assert_eq!(receipt.requested.profile.as_deref(), Some("glm_51_impl"));
    assert_eq!(receipt.planned.profile.as_deref(), Some("glm_impl"));
    assert_eq!(
        receipt.planned.model.as_deref(),
        Some("zhipuai-coding-plan/glm-5.2")
    );
    assert_eq!(receipt.planned.model_lineage_id, "zhipuai-coding-plan/glm");
    assert_eq!(
        receipt.observed.acknowledgement,
        crate::DispatchAcknowledgement::Unconfirmed
    );
    assert_eq!(receipt.observed.effective.backend, crate::UNKNOWN_IDENTITY);
    assert!(resolved.route_explanation.iter().any(|line| {
        line.contains("deprecated dispatch profile alias 'glm_51_impl'")
            && line.contains("glm_impl")
    }));
}

#[test]
fn same_lineage_override_is_planned_but_not_relabelled_as_observed() {
    let _lock = env_lock();
    let _env = EnvGuard::remove("TACHI_DISPATCH_GLM_CODING_MODEL");
    let mut params = params("glm_impl");
    params.model = Some("zhipuai-coding-plan/glm-5.2@2026-07-13".to_string());
    let resolved = resolve_and_apply_dispatch_profile(
        &mut params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |_| false,
    )
    .expect("same-lineage override");
    assert_eq!(
        resolved.identity_receipt.requested.model.as_deref(),
        Some("zhipuai-coding-plan/glm-5.2@2026-07-13")
    );
    assert_eq!(
        resolved.identity_receipt.planned.model,
        resolved.identity_receipt.requested.model
    );
    assert_eq!(
        resolved.identity_receipt.observed.acknowledgement,
        crate::DispatchAcknowledgement::Unconfirmed
    );
}

#[test]
fn cross_lineage_model_override_fails_closed() {
    let mut params = params("glm_impl");
    params.model = Some("openai/gpt-5.6".to_string());
    let error = resolve_and_apply_dispatch_profile(
        &mut params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |_| false,
    )
    .expect_err("cross-lineage override must fail closed");
    assert!(error.contains("without explicit profile authorization"));
}

#[test]
fn receipt_rejects_unauthorized_substitution_and_survives_replay() {
    let profile = resolve_dispatch_profile("glm_impl").expect("profile");
    let mut receipt = recommendation_identity_receipt(profile);
    let mut substitute = receipt.planned.clone();
    substitute.model_lineage_id = "openai".to_string();
    substitute.model = Some("openai/gpt-5.6".to_string());
    assert!(receipt
        .acknowledge(
            substitute,
            crate::DispatchAcknowledgement::Substituted,
            "carrier changed model".to_string(),
        )
        .is_err());

    let replay: crate::DispatchIdentityReceipt =
        serde_json::from_value(serde_json::to_value(&receipt).expect("serialize"))
            .expect("deserialize");
    assert_eq!(replay, receipt, "replay must preserve the frozen receipt");
}

#[test]
fn matching_acknowledgement_is_not_a_mismatch_and_wins_attribution() {
    let _lock = env_lock();
    let _env = EnvGuard::remove("TACHI_DISPATCH_GLM_CODING_MODEL");
    let profile = resolve_dispatch_profile("glm_impl").expect("profile");
    let mut receipt = recommendation_identity_receipt(profile);
    assert_eq!(
        receipt.attribution().0,
        receipt.planned,
        "an unconfirmed receipt attributes to the planned identity"
    );

    let mut observed = receipt.planned.clone();
    // A real carrier fills in provenance the planner could not know.
    observed.seat = "worker-3".to_string();
    observed.transport = "acp".to_string();
    observed.adapter_version = "1.2.3".to_string();
    observed.carrier_version = "0.9.0".to_string();
    receipt
        .acknowledge(
            observed,
            crate::DispatchAcknowledgement::Acknowledged,
            "carrier acknowledged launch".to_string(),
        )
        .expect("matching acknowledgement");
    assert!(
        !receipt.observed.mismatch,
        "provenance-only differences must not flag an identity mismatch"
    );
    assert_eq!(receipt.attribution().0, receipt.observed.effective);
}

#[test]
fn same_lineage_release_substitution_is_an_explicit_mismatch() {
    let _lock = env_lock();
    let _env = EnvGuard::remove("TACHI_DISPATCH_GLM_CODING_MODEL");
    let profile = resolve_dispatch_profile("glm_impl").expect("profile");
    let mut receipt = recommendation_identity_receipt(profile);
    let mut observed = receipt.planned.clone();
    observed.model = Some("zhipuai-coding-plan/glm-5.2@2026-07-13".to_string());
    let (release, provider_model, version) = crate::provider_model_parts(observed.model.as_deref());
    observed.concrete_model_release = release;
    observed.provider_model = provider_model;
    observed.provider_model_version = version;
    receipt
        .acknowledge(
            observed,
            crate::DispatchAcknowledgement::Substituted,
            "carrier pinned a release".to_string(),
        )
        .expect("same-lineage release substitution is allowed");
    assert!(
        receipt.observed.mismatch,
        "a release substitution must surface as an explicit mismatch"
    );
    assert_eq!(
        receipt.attribution().0.model.as_deref(),
        Some("zhipuai-coding-plan/glm-5.2@2026-07-13"),
        "attribution follows what executed, never the planned claim"
    );
}

#[test]
fn profile_without_model_accepts_same_family_override() {
    let mut params = params("claude_plan");
    params.model = Some("anthropic/claude-sonnet-5".to_string());
    let resolved = resolve_and_apply_dispatch_profile(
        &mut params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |_| false,
    )
    .expect("same-family override on a model-less profile must be allowed");
    assert_eq!(
        resolved.identity_receipt.planned.model.as_deref(),
        Some("anthropic/claude-sonnet-5")
    );
}

#[test]
fn profile_without_model_rejects_cross_family_override() {
    let mut params = params("claude_plan");
    params.model = Some("openai/gpt-5.6".to_string());
    let error = resolve_and_apply_dispatch_profile(
        &mut params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |_| false,
    )
    .expect_err("cross-family override must fail closed");
    assert!(error.contains("without explicit profile authorization"));
}

#[test]
fn malformed_lineages_fail_closed() {
    assert!(crate::lineages_compatible("anthropic/claude", "claude"));
    assert!(crate::lineages_compatible("claude", "claude"));
    assert!(!crate::lineages_compatible("a/b/claude", "claude"));
    assert!(!crate::lineages_compatible("a/b/c", "a/b/c"));
    assert!(!crate::lineages_compatible("", ""));
    assert!(!crate::lineages_compatible("", "claude"));
    assert!(!crate::lineages_compatible("/claude", "claude"));
    assert!(!crate::lineages_compatible("openai/gpt", "claude"));
    assert!(!crate::lineages_compatible("anthropic/claude", ""));
}

#[test]
fn provider_model_parts_separates_provider_release_and_version() {
    assert_eq!(
        crate::provider_model_parts(Some("zhipuai-coding-plan/glm-5.2@2026-07-13")),
        (
            "zhipuai-coding-plan/glm-5.2".to_string(),
            "glm-5.2".to_string(),
            "2026-07-13".to_string()
        )
    );
    assert_eq!(
        crate::provider_model_parts(Some("glm-5.2")),
        (
            "glm-5.2".to_string(),
            "glm-5.2".to_string(),
            crate::UNKNOWN_IDENTITY.to_string()
        )
    );
}

#[test]
fn every_resolution_entry_route_emits_the_full_identity_receipt_shape() {
    let mut profile = params("glm_impl");
    let mut profile_and_agent = params("glm_impl");
    profile_and_agent.agent = Some("opencode".to_string());
    let mut direct = params("glm_impl");
    direct.profile = None;
    direct.agent = Some("codex".to_string());
    direct.model = Some("openai/gpt-5.6".to_string());
    let mut custom = params("glm_impl");
    custom.profile = None;
    custom.agent = Some("custom".to_string());
    custom.model = Some("example/custom-1".to_string());
    let mut host_adapter = params("opencode_terra_write");

    let mut expected = None;
    for receipt in [
        resolve_for_identity_test(&mut profile).identity_receipt,
        resolve_for_identity_test(&mut profile_and_agent).identity_receipt,
        resolve_for_identity_test(&mut direct).identity_receipt,
        resolve_for_identity_test(&mut custom).identity_receipt,
        resolve_for_identity_test(&mut host_adapter).identity_receipt,
    ] {
        let mut keys = BTreeSet::new();
        receipt_key_paths(
            &serde_json::to_value(receipt).expect("receipt serializes"),
            "",
            &mut keys,
        );
        if let Some(expected) = &expected {
            assert_eq!(
                &keys, expected,
                "every entry route must expose every receipt key"
            );
        } else {
            expected = Some(keys);
        }
    }
}

#[test]
fn glm_model_registry_supports_env_override_and_profile_json_metadata() {
    let _lock = env_lock();
    let _env = EnvGuard::set("TACHI_DISPATCH_GLM_CODING_MODEL", "zhipuai/glm-5.2-custom");

    let profile = resolve_dispatch_profile("glm_impl").expect("legacy model-card profile");
    let card = profile_json(profile);

    assert_eq!(card["model"], json!("zhipuai/glm-5.2-custom"));
    assert_eq!(card["model_alias"], json!("glm_coding"));
    assert_eq!(card["model_card"]["vendor"], json!("glm"));
    assert_eq!(
        card["model_card"]["default_model"],
        json!("zhipuai-coding-plan/glm-5.2")
    );
    assert_eq!(card["deprecated_aliases"], json!(["glm_51_impl"]));

    let glm_card = profile_json(resolve_dispatch_profile("glm_impl").unwrap());
    assert_eq!(glm_card["name"], json!("glm_impl"));
    assert_eq!(glm_card["model"], json!("zhipuai/glm-5.2-custom"));
    assert_eq!(glm_card["model_card"]["role"], json!("executor"));
    assert_eq!(glm_card["deprecated_aliases"], json!(["glm_51_impl"]));
    assert_eq!(
        glm_card["deprecation"]["aliases"][0]["replacement"],
        json!("glm_impl")
    );
}

#[test]
fn opencode_profile_command_materializes_registry_model() {
    let _lock = env_lock();
    let _env = EnvGuard::set("TACHI_DISPATCH_GLM_CODING_MODEL", "zhipuai/glm-5.2[1m]");
    let mut params = params("glm_impl");

    resolve_and_apply_dispatch_profile(
        &mut params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |_| false,
    )
    .expect("operator model-card profile should resolve");

    assert_eq!(params.model.as_deref(), Some("zhipuai/glm-5.2[1m]"));
    assert!(params
        .command
        .windows(2)
        .any(|pair| pair == ["--model", "zhipuai/glm-5.2[1m]"]));
}

#[test]
fn staff_native_profile_freezes_model_transport_turns_and_has_no_fallback() {
    for profile in DISPATCH_PROFILES
        .iter()
        .filter(|profile| profile.native_session.is_some())
    {
        let mut request =
            StaffAssignmentRequest::new(TachiDispatchReason::ExplicitUserRequest, "review");
        request.profile = Some(profile.name.into());
        let resolved = resolve_and_apply_staff_assignment_profile(
            &mut request,
            |_| Ok(Vec::new()),
            |_| Ok(Vec::new()),
            |_| panic!("no legacy transport probe"),
        )
        .unwrap();
        assert_eq!(resolved.agent, profile.backend);
        assert_eq!(resolved.selected_model.as_deref(), profile.model);
        assert_eq!(resolved.harness_transport.as_deref(), Some("acp_native"));
        assert!(resolved.launch_command.is_empty());
        assert!(resolved.fallback_chain.is_empty());
        assert_eq!(
            resolved.identity_receipt.observed.acknowledgement,
            crate::DispatchAcknowledgement::Unconfirmed
        );
        request.worker = Some(
            if profile.backend == "codex" {
                "opencode"
            } else {
                "codex"
            }
            .into(),
        );
        assert!(resolve_and_apply_staff_assignment_profile(
            &mut request,
            |_| Ok(Vec::new()),
            |_| Ok(Vec::new()),
            |_| false
        )
        .unwrap_err()
        .contains("worker override"));
    }
}

#[test]
fn staff_no_profile_and_retired_profile_do_not_select_replacement_models() {
    let mut request =
        StaffAssignmentRequest::new(TachiDispatchReason::ExplicitUserRequest, "review");
    request.worker = Some("codex".into());
    let resolve = |request: &mut StaffAssignmentRequest| {
        resolve_and_apply_staff_assignment_profile(
            request,
            |_| Ok(Vec::new()),
            |_| Ok(Vec::new()),
            |_| false,
        )
    };
    assert!(resolve(&mut request)
        .unwrap_err()
        .contains("requires a native Session profile"));
    request.profile = Some("codex_53_fast".into());
    let error = resolve(&mut request).unwrap_err();
    assert!(error.contains("retired") && error.contains("codex_sol_review"));
}

#[test]
fn direct_native_profile_rejects_model_and_transport_override() {
    let mut request = params("codex_sol_review");
    request.model = Some("gpt-6-astra".into());
    assert!(resolve_and_apply_dispatch_profile(
        &mut request,
        |_| Ok(Vec::new()),
        |_| Ok(Vec::new()),
        |_| false
    )
    .unwrap_err()
    .contains("remove launch overrides"));
    request.model = None;
    request.harness_transport = Some("cli".into());
    assert!(resolve_and_apply_dispatch_profile(
        &mut request,
        |_| Ok(Vec::new()),
        |_| Ok(Vec::new()),
        |_| false
    )
    .is_err());
}

//! Effective-authority contract compilation at the dispatch entry point
//! (#894 S2d).
//!
//! This is where the pure compiler in `tachi_dispatch::authority` is bound to a
//! real dispatch: it runs immediately after profile resolution and the env
//! binding gate, and **before** the run directory exists, before prompt
//! assembly, before the V2 plan stage's `ClaudePool` call, before credential
//! materialization, and before any backend is chosen. That ordering is the
//! whole contract:
//!
//! - an omitted `sandbox` resolves from the *effective profile* (so
//!   `codex_55_review` compiles to `read-only`, where before it silently fell
//!   through to codex's `workspace-write` default);
//! - a read-only level that no kill-test-certified provider can enforce is
//!   refused here, with a receipt — not after a run directory and a set of
//!   materialized credentials already exist on disk. As shipped, exactly one
//!   provider is certified: **codex/cli at `read-only`, on the binary version
//!   named in `certifications/codex-cli.toml`** (codex-cli 0.144.1, by an
//!   executed kill-test). The installed binary's version is probed *here*, before
//!   the gate, and a codex that the receipt does not name — including a codex we
//!   cannot version — fails closed exactly like an uncertified provider. Every
//!   other shell-capable read-only lane (claude, grok, kimi, custom/opencode,
//!   codex-over-acpx) is still refused: no primitive, no receipt. Refusing beats
//!   pretending;
//! - the operator bypass (`permission_profile=full|verify`) is reconciled with
//!   the profile ceiling here too — it claims `danger-full-access`, so it cannot
//!   be smuggled past a review profile by also passing an explicit `sandbox`;
//! - the compiled level, not a vendor default, is what the launcher receives:
//!   `params.sandbox` is *overwritten* with the compiled value;
//! - skills that need workspace writes are dropped from `params.skills` under a
//!   read-only contract (a mounted skill is an input, not a permission).
//!
//! It replaces the narrower `validate_dispatch_sandbox_at_entry` gate from
//! #894 S0 (which only rejected sandbox values a backend could not honor). The
//! per-builder `reject_unsupported_sandbox` / `validate_codex_sandbox` calls in
//! `tachi-dispatch`'s launch builders stay as defense-in-depth.

use super::*;
use tachi_dispatch::{
    compile_effective_contract, ContractInputs, DispatchLaunchParams, EffectiveContract,
    ProviderQualification, SkillRequest,
};

/// #1815 P1 authority issuance. The compiled contract first applies the
/// existing narrowing to the temporary legacy ingress; this established typed
/// grant then records the exact values P3/#1814 will consume directly.
pub(super) fn mint_execution_grant(
    params: &mut TachiDispatchParams,
    grant_id: impl Into<String>,
    env_resolution: &crate::exec_env_ops::EnvResolution,
) -> Result<tachi_params::ExecutionGrant, String> {
    // The running launch path historically reads the top-level MCP knobs.
    // Preserve that precedence in the grant without rewriting the independent
    // nested profile metadata consumed by prompt, receipt, and progress paths.
    let mut mcp_access = params.mcp_access.clone();
    if let Some(access) = mcp_access.as_mut() {
        access.inject_tachi_mcp = params.inject_tachi_mcp;
        access.inject_hub_mcps = params.inject_hub_mcps;
        access.allowed_mcp_servers = params.allowed_mcp_servers.clone();
    }
    let grant = tachi_params::ExecutionGrant {
        grant_id: grant_id.into(),
        env_id: env_resolution.env_id().map(str::to_string),
        unmanaged_cwd_allowed: matches!(
            env_resolution,
            crate::exec_env_ops::EnvResolution::Unmanaged { .. }
        ),
        allowed_cwd: env_resolution.cwd().map(std::path::PathBuf::from),
        credential_profiles: canonical_credential_profiles(&params.credential_profiles),
        mcp_access,
        allowed_tools: params.allowed_tools.clone(),
        permission_profile: params.permission_profile.clone(),
        sandbox: params.sandbox.clone(),
        max_turns: params.max_turns,
        timeout_secs: params.timeout_secs,
    };
    apply_grant_legacy_projection(params, &grant);
    assert_grant_legacy_projection(params, &grant, env_resolution)?;
    Ok(grant)
}

fn apply_grant_legacy_projection(
    params: &mut TachiDispatchParams,
    grant: &tachi_params::ExecutionGrant,
) {
    // #1815 P1 temporary projection, deleted by P3/#1814 once backend,
    // credential, and launch consumers take ExecutionGrant directly.
    if let Some(access) = grant.mcp_access.as_ref() {
        params.inject_tachi_mcp = access.inject_tachi_mcp;
        params.inject_hub_mcps = access.inject_hub_mcps;
        params.allowed_mcp_servers = access.allowed_mcp_servers.clone();
    }
    params.allowed_tools = grant.allowed_tools.clone();
    params.permission_profile = grant.permission_profile.clone();
    params.sandbox = grant.sandbox.clone();
    params.max_turns = grant.max_turns;
    params.timeout_secs = grant.timeout_secs;
}

/// The temporary flat projection is permitted only through P2/P3 and final
/// #1814. Reject a one-sided update rather than silently letting the launch
/// path and the server-owned grant describe different authority.
pub(super) fn assert_grant_legacy_projection(
    params: &TachiDispatchParams,
    grant: &tachi_params::ExecutionGrant,
    env_resolution: &crate::exec_env_ops::EnvResolution,
) -> Result<(), String> {
    let matches = grant.env_id == env_resolution.env_id().map(str::to_string)
        && grant.allowed_cwd == env_resolution.cwd().map(std::path::PathBuf::from)
        && grant.unmanaged_cwd_allowed
            == matches!(
                env_resolution,
                crate::exec_env_ops::EnvResolution::Unmanaged { .. }
            )
        && grant.credential_profiles == canonical_credential_profiles(&params.credential_profiles)
        && grant.allowed_tools == params.allowed_tools
        && grant.permission_profile == params.permission_profile
        && grant.sandbox == params.sandbox
        && grant.max_turns == params.max_turns
        && grant.timeout_secs == params.timeout_secs;
    let mcp_matches = match grant.mcp_access.as_ref() {
        Some(access) => {
            access.inject_tachi_mcp == params.inject_tachi_mcp
                && access.inject_hub_mcps == params.inject_hub_mcps
                && access.allowed_mcp_servers == params.allowed_mcp_servers
                && params.mcp_access.as_ref().is_some_and(|nested| {
                    access.allowed_facades == nested.allowed_facades
                        && access.github_read == nested.github_read
                        && access.write_actions == nested.write_actions
                        && access.issue_refs == nested.issue_refs
                        && access.pr_refs == nested.pr_refs
                        && access.fallback == nested.fallback
                })
        }
        None => {
            !params.inject_tachi_mcp.unwrap_or(false)
                && !params.inject_hub_mcps.unwrap_or(false)
                && params.allowed_mcp_servers.is_empty()
                && params.mcp_access.is_none()
        }
    };
    if matches && mcp_matches {
        Ok(())
    } else {
        Err("execution grant and legacy compatibility projection diverged".to_string())
    }
}

fn canonical_credential_profiles(raw: &[String]) -> Vec<String> {
    raw.iter().fold(Vec::new(), |mut canonical, profile| {
        let profile = profile.trim();
        if !profile.is_empty() && !canonical.iter().any(|seen| seen == profile) {
            canonical.push(profile.to_string());
        }
        canonical
    })
}

/// Compile the dispatch's effective authority contract and apply it to
/// `params`. Returns the contract (for the receipt) or a typed-error string.
///
/// `params` is mutated in exactly three ways, all derived from effective authority:
/// `sandbox` becomes the compiled level (or `None` for providers with no
/// sandbox primitive — never a vendor default), and `skills` becomes the
/// mounted subset; `permission_profile` records the admitted replay spelling.
///
/// `qualifications` and `backend_version` are parameters rather than the const +
/// a probe call, so the tests can pin each world explicitly: the certified binary
/// (codex-cli 0.144.1 — read-only lanes run), an *uncertified* codex version (a
/// codex upgrade — read-only lanes fail closed until somebody re-runs the
/// kill-test), and no codex at all (version unknown — fail closed). The
/// production caller passes [`tachi_dispatch::PROVIDER_QUALIFICATIONS`] and the
/// result of [`tachi_dispatch::probe_provider_version`].
///
/// `backend_version` is what the **installed** vendor binary reports right now.
/// It is load-bearing: `PROVIDER_QUALIFICATIONS`'s codex row cites a receipt for
/// one exact binary (`certifications/codex-cli.toml`), and `qualify_provider`
/// refuses to hand it back for any other version — including an unknown one.
/// That is the whole point of an evidence-based certification: it expires when
/// the evidence stops describing the thing you are about to run.
pub(super) fn compile_dispatch_contract(
    params: &mut TachiDispatchParams,
    request: &tachi_params::StaffAssignmentRequest,
    agent_norm: &str,
    harness_transport: &str,
    resolved_profile: &ResolvedDispatchProfile,
    qualifications: &[ProviderQualification],
    backend_version: Option<&str>,
) -> Result<EffectiveContract, String> {
    let admitted_permission_spelling = params.permission_profile.clone();
    let permission_profile = tachi_dispatch::resolve_permission_profile(&DispatchLaunchParams {
        cwd: params.cwd.clone(),
        model: params.model.clone(),
        permission_profile: params.permission_profile.clone(),
        allowed_tools: params.allowed_tools.clone(),
        max_turns: params.max_turns,
        sandbox: params.sandbox.clone(),
        command: params.command.clone(),
    })?;

    // Materialize the skill mount BEFORE compiling, so the compiler sees the
    // list the agent would actually get (profile skills, or the stage defaults
    // that `resolve_effective_skills` would have derived later). Writing them
    // back into `params.skills` is behavior-preserving: `resolve_effective_skills`
    // returns `params.skills` verbatim when it is non-empty, and computes the
    // same `auto_instruction` either way.
    if params.skills.is_empty() {
        let (effective_skills, _) = resolve_assignment_skills(request, &params.skills);
        params.skills = effective_skills;
    }
    let skills = params
        .skills
        .iter()
        .map(|id| SkillRequest::new(id.as_str()))
        .collect::<Vec<_>>();

    let profile = resolved_profile
        .selected_profile
        .as_deref()
        .and_then(tachi_dispatch::resolve_dispatch_profile);

    let contract = compile_effective_contract(&ContractInputs {
        backend: agent_norm,
        transport: harness_transport,
        backend_version,
        profile,
        requested_sandbox: params.sandbox.as_deref(),
        permission_profile,
        allowed_tools: &params.allowed_tools,
        skills: &skills,
        mcp_write_actions: resolved_profile.mcp_access.write_actions,
        mcp_github_read: resolved_profile.mcp_access.github_read,
        qualifications,
    })
    .map_err(|err| err.to_string())?;

    params.sandbox = contract.sandbox_arg.clone();
    params.skills = contract.mounted_skills.clone();
    // The typed grant records this admitted authority. Keep a successful
    // `verify` spelling replay-safe for the downstream launcher: it is an
    // accepted alias with a distinct headless opt-in, not a request to replay
    // as `full`. Omitted input projects to the explicit default spelling.
    params.permission_profile =
        admitted_permission_spelling.or_else(|| Some(permission_profile.as_str().to_string()));
    Ok(contract)
}

/// Receipt shape for `status.json` / the dispatch response. Everything a reader
/// needs to answer "who was actually stopping this agent from writing?".
pub(super) fn contract_receipt(contract: &EffectiveContract) -> Value {
    serde_json::to_value(contract).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch_profile::resolve_and_apply_dispatch_profile;
    use serde_json::json;
    use tachi_dispatch::{CODEX_CLI_RECEIPT, PROVIDER_QUALIFICATIONS};

    /// The binary the shipped receipt was issued for. A test that expects codex
    /// to *be* certified has to say which codex it is talking about — that is the
    /// certification gate, not ceremony: `qualify_provider` refuses the shipped
    /// row for any other version, and passing `None` here is how a box with no
    /// codex on it behaves.
    const CERTIFIED_CODEX_VERSION: Option<&str> = Some(CODEX_CLI_RECEIPT.vendor_version);

    /// The #878-B operator opt-in, scoped to one test (the sibling `tests`
    /// module's `EnvGuard` is private to it).
    struct FullPermissionOptIn {
        original: Option<std::ffi::OsString>,
    }

    impl FullPermissionOptIn {
        const KEY: &'static str = "TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE";

        fn set() -> Self {
            let original = std::env::var_os(Self::KEY);
            std::env::set_var(Self::KEY, "true");
            Self { original }
        }
    }

    impl Drop for FullPermissionOptIn {
        fn drop(&mut self) {
            match self.original.as_ref() {
                Some(value) => std::env::set_var(Self::KEY, value),
                None => std::env::remove_var(Self::KEY),
            }
        }
    }

    /// Build params + run the same profile resolution the entry point runs, so
    /// these tests exercise the real (profile -> contract -> params -> launcher)
    /// chain rather than a hand-assembled `ContractInputs`.
    fn resolve(value: Value) -> (TachiDispatchParams, ResolvedDispatchProfile) {
        let mut params: TachiDispatchParams =
            serde_json::from_value(value).expect("dispatch params");
        let resolved = resolve_and_apply_dispatch_profile(&mut params).expect("profile resolves");
        (params, resolved)
    }

    /// #894 S2d discriminating test ④ (server half): a review-profile dispatch
    /// that omits `sandbox` must compile to read-only and hand the launcher an
    /// explicit `--sandbox read-only`. Before this slice, `params.sandbox` was
    /// left `None` and `build_codex_launch` fell through to its
    /// `workspace-write` default — the exact leak #894 S2d closes. Asserting on
    /// the built argv (not just the contract) is what makes this discriminating:
    /// break the wiring and the command carries `workspace-write` again.
    #[test]
    fn omitted_sandbox_on_review_profile_reaches_codex_as_read_only() {
        let (mut params, resolved) = resolve(json!({
            "task": "review the diff",
            "staffing_reason": "explicit_user_request",
            "profile": "codex_55_review",
        }));
        assert_eq!(params.sandbox, None, "the caller omitted sandbox");

        let contract = compile_dispatch_contract(
            &mut params,
            "codex",
            "cli",
            &resolved,
            PROVIDER_QUALIFICATIONS,
            CERTIFIED_CODEX_VERSION,
        )
        .expect("review contract compiles");

        assert_eq!(
            params.sandbox.as_deref(),
            Some("read-only"),
            "an omitted sandbox must resolve from the effective profile, not the codex default"
        );
        assert_eq!(
            contract.workspace_authority,
            tachi_dispatch::WorkspaceAuthority::ReadOnly
        );
        assert!(matches!(
            contract.enforcement,
            tachi_dispatch::Enforcement::Enforced { .. }
        ));

        let cmd = build_codex_command(&params, "review", None).expect("codex command");
        let args = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sandbox", "read-only"]),
            "the launched codex command must carry the compiled read-only level: {args:?}"
        );
        assert!(
            !args
                .windows(2)
                .any(|pair| pair == ["--sandbox", "workspace-write"]),
            "the pre-#894-S2d workspace-write default must be gone: {args:?}"
        );

        // The dispatch receipt must carry the *evidence*, not just the verdict:
        // which receipt, for which binary version, from which kill-test. A reader
        // of `status.json` can then go and check it instead of trusting the word
        // "enforced".
        let receipt = contract_receipt(&contract);
        assert_eq!(receipt["workspace_authority"], json!("read-only"));
        assert_eq!(receipt["enforcement"]["mode"], json!("enforced"));
        assert_eq!(
            receipt["enforcement"]["receipt"],
            json!(CODEX_CLI_RECEIPT.id)
        );
        assert_eq!(
            receipt["enforcement"]["vendor_version"],
            json!(CODEX_CLI_RECEIPT.vendor_version)
        );
        assert_eq!(
            receipt["enforcement"]["certified_by"],
            json!(CODEX_CLI_RECEIPT.kill_test)
        );
        assert_eq!(receipt["network"], json!("restricted"));
    }

    /// Discriminating test ①: read-only profile + explicit `workspace-write` is
    /// a typed conflict, refused at the entry point.
    #[test]
    fn read_only_profile_plus_explicit_workspace_write_is_refused() {
        let (mut params, resolved) = resolve(json!({
            "task": "review the diff",
            "staffing_reason": "explicit_user_request",
            "profile": "codex_55_review",
            "sandbox": "workspace-write",
        }));
        let err = compile_dispatch_contract(
            &mut params,
            "codex",
            "cli",
            &resolved,
            PROVIDER_QUALIFICATIONS,
            CERTIFIED_CODEX_VERSION,
        )
        .expect_err("widening a read-only profile must be refused");
        assert!(err.contains("authority conflict"), "{err}");
        assert!(err.contains("codex_55_review"), "{err}");
        assert!(err.contains("never widened"), "{err}");
    }

    /// **Round-2 regression (the authority-escalation BUG), at the entry point.**
    /// An explicit `sandbox` value must not shadow the `permission_profile=full`
    /// bypass check: round 1 compiled this exact dispatch to a "read-only"
    /// contract and then launched codex with
    /// `--dangerously-bypass-approvals-and-sandbox`. It must be refused, and
    /// `params.sandbox` must not have been rewritten on the way out.
    #[test]
    fn explicit_sandbox_cannot_shadow_the_operator_bypass_check_at_the_entry_point() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // The operator opt-in is what makes `full` resolvable at all (#878-B).
        let _full = FullPermissionOptIn::set();

        let (mut params, resolved) = resolve(json!({
            "task": "review the diff",
            "staffing_reason": "explicit_user_request",
            "profile": "codex_55_review",
            "sandbox": "read-only",
            "permission_profile": "full",
        }));

        let err = compile_dispatch_contract(
            &mut params,
            "codex",
            "cli",
            &resolved,
            PROVIDER_QUALIFICATIONS,
            CERTIFIED_CODEX_VERSION,
        )
        .expect_err("a read-only profile must never compile to a sandbox bypass");
        assert!(err.contains("authority conflict"), "{err}");
        assert!(
            err.contains("danger-full-access"),
            "the receipt must name what the bypass actually claims: {err}"
        );
        assert!(err.contains("never widened"), "{err}");
        assert_eq!(
            params.sandbox.as_deref(),
            Some("read-only"),
            "a refused dispatch must not have had its params rewritten on the way out"
        );
    }

    /// Discriminating test ②: a skill that declares workspace-write intent is
    /// excluded from the mount under a read-only contract (with a reason) — a
    /// mounted skill is an input, not a permission. The exclusion is applied to
    /// `params.skills`, so it is the list the prompt/artifacts actually see.
    #[test]
    fn write_intent_skill_is_dropped_from_a_read_only_mount() {
        // `glm_impl` resolves through the OpenCode adapter, which reads
        // TACHI_OPENCODE_* env vars — take the same global lock the other
        // env-sensitive profile tests take.
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (mut params, resolved) = resolve(json!({
            "task": "review the diff",
            "staffing_reason": "explicit_user_request",
            "profile": "codex_55_review",
            "skills": [
                "skill:waza-check",
                "skill:superpowers-executing-plans",
                "skill:waza-write",
            ],
        }));

        let contract = compile_dispatch_contract(
            &mut params,
            "codex",
            "cli",
            &resolved,
            PROVIDER_QUALIFICATIONS,
            CERTIFIED_CODEX_VERSION,
        )
        .expect("review contract compiles");

        assert_eq!(
            params.skills,
            vec!["skill:waza-check".to_string()],
            "write-intent skills must not be mounted under a read-only contract"
        );
        let excluded = contract
            .excluded_skills
            .iter()
            .map(|s| s.skill_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            excluded,
            vec!["skill:superpowers-executing-plans", "skill:waza-write"]
        );
        assert!(contract.excluded_skills[0]
            .reason
            .contains("input, not a grant of authority"));

        // Same skills, executor profile: the contract permits writes, so
        // nothing is excluded — the exclusion tracks the contract, not the skill.
        let (mut exec_params, exec_resolved) = resolve(json!({
            "task": "land the patch",
            "staffing_reason": "explicit_user_request",
            "profile": "glm_impl",
            "skills": [
                "skill:waza-check",
                "skill:superpowers-executing-plans",
                "skill:waza-write",
            ],
        }));
        let exec_contract = compile_dispatch_contract(
            &mut exec_params,
            "custom",
            "cli",
            &exec_resolved,
            PROVIDER_QUALIFICATIONS,
            CERTIFIED_CODEX_VERSION,
        )
        .expect("executor contract compiles");
        assert!(exec_contract.excluded_skills.is_empty());
        assert_eq!(exec_params.skills.len(), 3);
    }

    /// **Round-2 fix (invariant 4 was decorative).** A read-only *explore* lane on
    /// opencode/custom runs shell unattended and nothing enforces its read-only
    /// claim — round 1 let it through as "advisory". A read-only claim with no
    /// enforcer and an unattended shell behind it is not a weaker isolation
    /// story, it is no isolation story: refused pre-spawn, with a receipt.
    ///
    /// This is a deliberate behavior change with product blast radius — it is the
    /// owner-frozen invariant 4 + 5 posture ("refusing beats pretending"), and it
    /// is what forces the kill-test to actually be run.
    #[test]
    fn shell_capable_read_only_lane_on_an_uncertified_backend_is_refused() {
        // `deepseek_explore` resolves through the OpenCode adapter.
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (mut params, resolved) = resolve(json!({
            "task": "map the repo",
            "staffing_reason": "explicit_user_request",
            "profile": "deepseek_explore",
        }));
        let err = compile_dispatch_contract(
            &mut params,
            "custom",
            "cli",
            &resolved,
            PROVIDER_QUALIFICATIONS,
            CERTIFIED_CODEX_VERSION,
        )
        .expect_err("an unattended read-only lane needs a certified enforcer");
        assert!(err.contains("not kill-test certified"), "{err}");
        assert!(err.contains("read-only"), "{err}");
        assert!(err.contains("fail-closed"), "{err}");
    }

    /// **The version gate, at the entry point.** The codex review lane runs
    /// because ONE binary was kill-tested; it must stop running the moment the
    /// binary underneath it is not that one. A codex upgrade (or a box where
    /// `codex --version` cannot be read at all) therefore refuses the lane
    /// pre-spawn, with a receipt naming the gap and the way out — rather than
    /// inheriting yesterday's evidence for today's binary.
    ///
    /// This is the shipped fail-closed posture, pinned so nobody discovers it by
    /// surprise on upgrade day. Re-run the kill-test, issue a new receipt, and the
    /// lane comes back.
    #[test]
    fn a_codex_the_receipt_does_not_name_refuses_the_read_only_lane() {
        for (world, version, expected) in [
            (
                "codex upgraded past the certified binary",
                Some("0.145.0"),
                "does not carry across vendor versions",
            ),
            (
                "codex version cannot be determined (not installed / --version failed)",
                None,
                "an unknown version is not a certified version",
            ),
        ] {
            let (mut params, resolved) = resolve(json!({
                "task": "review the diff",
                "staffing_reason": "explicit_user_request",
                "profile": "codex_55_review",
            }));
            let err = compile_dispatch_contract(
                &mut params,
                "codex",
                "cli",
                &resolved,
                PROVIDER_QUALIFICATIONS,
                version,
            )
            .expect_err("an uncertified binary must not run a read-only lane");
            assert!(err.contains("not kill-test certified"), "[{world}] {err}");
            assert!(err.contains(expected), "[{world}] {err}");
            assert!(err.contains("fail-closed"), "[{world}] {err}");
            assert_eq!(
                params.sandbox, None,
                "[{world}] a refused dispatch must not have had its params rewritten"
            );
        }
    }

    /// A write-level dispatch on a provider with no sandbox primitive still gets
    /// NO fabricated vendor flag, and its enforcement is recorded as advisory —
    /// stated out loud in the receipt instead of being passed off as isolation.
    /// (Write levels make no isolation claim, so they are not refused.)
    #[test]
    fn primitive_less_backend_gets_no_sandbox_flag_and_an_advisory_receipt() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (mut params, resolved) = resolve(json!({
            "task": "land the patch",
            "staffing_reason": "explicit_user_request",
            "profile": "opencode_builder",
        }));
        let contract = compile_dispatch_contract(
            &mut params,
            "custom",
            "cli",
            &resolved,
            PROVIDER_QUALIFICATIONS,
            CERTIFIED_CODEX_VERSION,
        )
        .expect("builder contract compiles");

        assert_eq!(
            params.sandbox, None,
            "custom/opencode has no --sandbox knob"
        );
        assert_eq!(
            contract.workspace_authority,
            tachi_dispatch::WorkspaceAuthority::WorkspaceWrite
        );
        let receipt = contract_receipt(&contract);
        assert_eq!(receipt["enforcement"]["mode"], json!("advisory"));
        assert!(receipt["enforcement"]["reason"]
            .as_str()
            .expect("reason")
            .contains("NOT machine-enforced"));
    }
}

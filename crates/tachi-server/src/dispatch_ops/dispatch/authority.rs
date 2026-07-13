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
//!   materialized credentials already exist on disk;
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
    SkillRequest, PROVIDER_QUALIFICATIONS,
};

/// Compile the dispatch's effective authority contract and apply it to
/// `params`. Returns the contract (for the receipt) or a typed-error string.
///
/// `params` is mutated in exactly two ways, both of which can only *narrow*:
/// `sandbox` becomes the compiled level (or `None` for providers with no
/// sandbox primitive — never a vendor default), and `skills` becomes the
/// mounted subset.
///
/// Provider version is passed as `None`: the shipped qualification table's only
/// row (codex/cli) is `VersionScope::Any`, so no version probe is needed. If a
/// future row is narrowed to `VersionScope::AtLeast(..)`, an unknown version
/// fails CLOSED (`qualify_provider` refuses), which is the correct direction —
/// it will surface immediately as a refused dispatch, not as a silent downgrade,
/// and a `codex --version` probe can be added here at that point.
pub(super) fn compile_dispatch_contract(
    params: &mut TachiDispatchParams,
    agent_norm: &str,
    harness_transport: &str,
    resolved_profile: &ResolvedDispatchProfile,
) -> Result<EffectiveContract, String> {
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
        let (effective_skills, _) = resolve_effective_skills(params);
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
        backend_version: None,
        profile,
        requested_sandbox: params.sandbox.as_deref(),
        permission_profile,
        allowed_tools: &params.allowed_tools,
        skills: &skills,
        mcp_write_actions: resolved_profile.mcp_access.write_actions,
        mcp_github_read: resolved_profile.mcp_access.github_read,
        qualifications: PROVIDER_QUALIFICATIONS,
    })
    .map_err(|err| err.to_string())?;

    params.sandbox = contract.sandbox_arg.clone();
    params.skills = contract.mounted_skills.clone();
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
            "profile": "codex_55_review",
        }));
        assert_eq!(params.sandbox, None, "the caller omitted sandbox");

        let contract = compile_dispatch_contract(&mut params, "codex", "cli", &resolved)
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

        let receipt = contract_receipt(&contract);
        assert_eq!(receipt["workspace_authority"], json!("read-only"));
        assert_eq!(receipt["enforcement"]["mode"], json!("enforced"));
    }

    /// Discriminating test ①: read-only profile + explicit `workspace-write` is
    /// a typed conflict, refused at the entry point.
    #[test]
    fn read_only_profile_plus_explicit_workspace_write_is_refused() {
        let (mut params, resolved) = resolve(json!({
            "task": "review the diff",
            "profile": "codex_55_review",
            "sandbox": "workspace-write",
        }));
        let err = compile_dispatch_contract(&mut params, "codex", "cli", &resolved)
            .expect_err("widening a read-only profile must be refused");
        assert!(err.contains("authority conflict"), "{err}");
        assert!(err.contains("codex_55_review"), "{err}");
        assert!(err.contains("never widened"), "{err}");
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
            "profile": "codex_55_review",
            "skills": [
                "skill:waza-check",
                "skill:superpowers-executing-plans",
                "skill:waza-write",
            ],
        }));

        let contract = compile_dispatch_contract(&mut params, "codex", "cli", &resolved)
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
            "profile": "glm_impl",
            "skills": [
                "skill:waza-check",
                "skill:superpowers-executing-plans",
                "skill:waza-write",
            ],
        }));
        let exec_contract =
            compile_dispatch_contract(&mut exec_params, "custom", "cli", &exec_resolved)
                .expect("executor contract compiles");
        assert!(exec_contract.excluded_skills.is_empty());
        assert_eq!(exec_params.skills.len(), 3);
    }

    /// A provider with no sandbox primitive gets NO fabricated vendor flag, and
    /// its (profile-derived) read-only level is recorded as advisory — stated
    /// out loud in the receipt instead of being passed off as isolation.
    #[test]
    fn primitive_less_backend_gets_no_sandbox_flag_and_an_advisory_receipt() {
        // `deepseek_explore` also resolves through the OpenCode adapter.
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (mut params, resolved) = resolve(json!({
            "task": "map the repo",
            "profile": "deepseek_explore",
        }));
        let contract = compile_dispatch_contract(&mut params, "custom", "cli", &resolved)
            .expect("explore contract compiles");

        assert_eq!(
            params.sandbox, None,
            "custom/opencode has no --sandbox knob"
        );
        assert_eq!(
            contract.workspace_authority,
            tachi_dispatch::WorkspaceAuthority::ReadOnly
        );
        let receipt = contract_receipt(&contract);
        assert_eq!(receipt["enforcement"]["mode"], json!("advisory"));
        assert!(receipt["enforcement"]["reason"]
            .as_str()
            .expect("reason")
            .contains("NOT machine-enforced"));
    }
}

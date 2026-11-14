use crate::native_skill_ids::{
    CODING_REFACTOR_CHECKLIST, CODING_TEST_STRATEGY, SUPERPOWER_EXECUTING_PLANS,
    SUPERPOWER_REQUESTING_CODE_REVIEW, SUPERPOWER_VERIFICATION_BEFORE_COMPLETION, WAZA_CHECK,
    WAZA_TACHI,
};
// Historical CLI certification tests retain their original policy inputs.
// These synthetic rows are not selectable by the production profile registry.
fn resolve_dispatch_profile(name: &str) -> Option<&'static DispatchProfileDef> {
    const LEGACY: &[DispatchProfileDef] = &[
        DispatchProfileDef {
            name: "codex_55_review",
            display_name: "Codex Senior Reviewer",
            backend: "codex",
            role: "senior_reviewer",
            stage: Some("review"),
            model: None,
            model_alias: None,
            native_session: None,
            allow_cross_lineage_override: false,
            tool_profile: "standard",
            inject_tachi_mcp: false,
            github_read: true,
            write_actions: false,
            allowed_facades: &["tachi_memory", "tachi_event", "tachi_wiki", "tachi_task"],
            allowed_mcp_servers: &[],
            credential_profiles: &[],
            common_skills: &[
                SUPERPOWER_REQUESTING_CODE_REVIEW,
                SUPERPOWER_VERIFICATION_BEFORE_COMPLETION,
                WAZA_CHECK,
            ],
            signature_skills: &[CODING_REFACTOR_CHECKLIST],
            passive_traits: &["strict_on_missing_tests", "schema_boundary_sense"],
            forbidden_skills: &["large_rewrite", "merge_without_evidence"],
            evidence_required: &["findings_by_severity", "file_refs", "verification_advice"],
            strong_against: &[
                "schema_migration",
                "dispatch_refactor",
                "eval_ledger_changes",
            ],
            weak_against: &["low_risk_docs", "copyedit"],
        },
        DispatchProfileDef {
            name: "codex_53_fast",
            display_name: "Codex Fast Checker",
            backend: "codex",
            role: "fast_checker",
            stage: Some("review_light"),
            model: None,
            model_alias: None,
            native_session: None,
            allow_cross_lineage_override: false,
            tool_profile: "observe",
            inject_tachi_mcp: false,
            github_read: false,
            write_actions: false,
            allowed_facades: &["tachi_memory", "tachi_event"],
            allowed_mcp_servers: &[],
            credential_profiles: &[],
            common_skills: &[WAZA_CHECK],
            signature_skills: &[SUPERPOWER_VERIFICATION_BEFORE_COMPLETION],
            passive_traits: &["fast_sanity_only", "flag_uncertainty"],
            forbidden_skills: &["deep_architecture_review", "write_actions"],
            evidence_required: &["summary", "risk_flags"],
            strong_against: &["quick_sanity", "low_risk_review"],
            weak_against: &["schema_migration", "security_review"],
        },
        DispatchProfileDef {
            name: "opencode_builder",
            display_name: "OpenCode Credentialed Builder",
            backend: "opencode",
            role: "executor",
            stage: Some("execute"),
            model: None,
            model_alias: Some(crate::GLM_CODING_MODEL_ALIAS),
            native_session: None,
            allow_cross_lineage_override: false,
            tool_profile: "delegate",
            inject_tachi_mcp: false,
            github_read: false,
            write_actions: true,
            allowed_facades: &["tachi_memory", "tachi_event", "tachi_task"],
            allowed_mcp_servers: &[],
            credential_profiles: &["opencode_shared"],
            common_skills: &[SUPERPOWER_EXECUTING_PLANS],
            signature_skills: &[WAZA_TACHI, CODING_TEST_STRATEGY],
            passive_traits: &[
                "bounded_diff",
                "tests_required",
                "credentialed_opencode_config",
                // tachi#1184: see the sibling `glm_impl` profile's comment above
                // — same contract gap, same fix, both write-capable executor
                // profiles.
                "build_through_oz_or_declared_shared_target",
            ],
            forbidden_skills: &[
                "unbounded_redesign",
                "silent_test_workaround",
                "self_managed_cargo_target_dir",
            ],
            evidence_required: &["diff", "tests_run", "files_changed"],
            strong_against: &["bounded_patch", "implementation", "credentialed_dispatch"],
            weak_against: &["ambiguous_architecture", "unbounded_refactor"],
        },
    ];
    LEGACY
        .iter()
        .find(|profile| profile.name == name)
        .or_else(|| crate::resolve_dispatch_profile(name))
}
use super::*;
use crate::certification::CertificationResult;

/// The kill-test's own source, read at compile time — the ratchet
/// (`every_certified_row_is_backed_by_a_passing_executed_receipt`) checks
/// that the receipt's kill-test really exists and really contains the
/// function the receipt says was run.
const KILL_TEST_SOURCE: &str = include_str!("../../tests/codex_sandbox_kill_test.rs");

/// The version the shipped receipt was issued for. Every test below that
/// expects codex/cli to *be* certified must say so out loud by pinning it:
/// certification is scoped to one binary, and a test that forgets which
/// binary it is talking about is testing nothing.
const CERTIFIED_VERSION: &str = CODEX_CLI_RECEIPT.vendor_version;

/// Inputs with **no version determined** — the fail-closed world (codex not
/// on PATH, `--version` failed). The shipped row is certified, but a
/// certification is scoped to a binary, and this dispatch cannot say which
/// binary it has.
fn inputs<'a>(
    backend: &'a str,
    profile: Option<&'a DispatchProfileDef>,
    skills: &'a [SkillRequest],
) -> ContractInputs<'a> {
    ContractInputs {
        backend,
        transport: "cli",
        backend_version: None,
        codex_read_only_supported: false,
        native_sandbox_supported: false,
        host_os: CODEX_CLI_RECEIPT.host_os,
        profile,
        requested_sandbox: None,
        permission_profile: PermissionProfile::Default,
        allowed_tools: &[],
        skills,
        mcp_write_actions: None,
        mcp_github_read: None,
        qualifications: PROVIDER_QUALIFICATIONS,
    }
}

#[test]
fn dsh_executor_is_advisory_and_cannot_inherit_read_only_qualification() {
    let executor = crate::resolve_dispatch_profile("dsh_executor").expect("native DSH profile");
    let contract = compile_effective_contract(&inputs("dsh", Some(executor), &[]))
        .expect("workspace-write advisory execution");
    assert_eq!(
        contract.workspace_authority,
        WorkspaceAuthority::WorkspaceWrite
    );
    assert!(contract.tool_authority.unattended_shell);
    assert!(matches!(contract.enforcement, Enforcement::Advisory { .. }));
    let readonly = resolve_dispatch_profile("codex_55_review").expect("read-only ceiling");
    assert!(matches!(
        compile_effective_contract(&inputs("dsh", Some(readonly), &[])),
        Err(ContractError::ProviderNotQualified { .. })
    ));
}

/// The certified world, on the **shipped** table: the installed codex is the
/// exact binary the kill-test was executed against, so read-only lanes
/// compile. (Round 2 had to fake this with a synthetic `CERTIFIED_CODEX`
/// table because nothing was certified; it is real now, and the tests
/// exercise the table that actually ships.)
fn certified_inputs<'a>(
    backend: &'a str,
    profile: Option<&'a DispatchProfileDef>,
    skills: &'a [SkillRequest],
) -> ContractInputs<'a> {
    ContractInputs {
        backend_version: Some(CERTIFIED_VERSION),
        ..inputs(backend, profile, skills)
    }
}

#[test]
fn current_codex_capability_admits_read_only_without_reusing_old_certification() {
    let profile = resolve_dispatch_profile("codex_55_review").unwrap();
    let mut input = inputs("codex", Some(profile), &[]);
    input.backend_version = Some("0.166.0");
    input.codex_read_only_supported = true;
    let contract = compile_effective_contract(&input).unwrap();
    assert_eq!(contract.sandbox_arg.as_deref(), Some("read-only"));
    assert!(
        matches!(contract.enforcement, Enforcement::VendorSandbox { ref vendor_version, .. } if vendor_version == "0.166.0")
    );
    assert_eq!(contract.network, NetworkAuthority::ProviderDefault);
    for transport in ["acpx", "acp-native", "opencode_serve"] {
        input.transport = transport;
        assert!(compile_effective_contract(&input).is_err());
    }
    input.transport = "cli";
    input.codex_read_only_supported = false;
    assert!(compile_effective_contract(&input).is_err());
    input.codex_read_only_supported = true;
    input.backend_version = None;
    assert!(compile_effective_contract(&input).is_err());
    input.backend_version = Some("0.166.0");
    input.requested_sandbox = Some("workspace-write");
    assert!(matches!(
        compile_effective_contract(&input),
        Err(ContractError::AuthorityConflict { .. })
    ));
}

#[test]
fn installed_capability_does_not_override_a_failed_kill_test() {
    static FAILED: CertificationReceipt = CertificationReceipt {
        vendor_version: "0.166.0",
        result: CertificationResult::Fail,
        ..CODEX_CLI_RECEIPT
    };
    let table = [ProviderQualification {
        backend: "codex",
        transport: TransportKind::Cli,
        certification: Certification::KillTested { receipt: &FAILED },
    }];
    let mut input = inputs("codex", resolve_dispatch_profile("codex_55_review"), &[]);
    input.backend_version = Some("0.166.0");
    input.codex_read_only_supported = true;
    input.qualifications = &table;
    assert!(compile_effective_contract(&input).is_err());
}

// ── Invariant 1/2: authority is monotone on EVERY path ───────────────────

/// Discriminating test ①: read-only profile + explicit workspace-write is a
/// TYPE CONFLICT, not a compatible override.
#[test]
fn read_only_profile_plus_explicit_workspace_write_is_a_typed_conflict() {
    let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
    let skills = Vec::new();
    let mut input = certified_inputs("codex", Some(profile), &skills);
    input.requested_sandbox = Some("workspace-write");

    let err = compile_effective_contract(&input)
        .expect_err("widening a read-only profile must be rejected");
    assert_eq!(err.code(), "authority_conflict");
    match &err {
        ContractError::AuthorityConflict {
            profile,
            ceiling,
            ceiling_source,
            requested,
            source,
        } => {
            assert_eq!(profile, "codex_55_review");
            assert_eq!(*ceiling, WorkspaceAuthority::ReadOnly);
            assert_eq!(*ceiling_source, CEILING_PROFILE);
            assert_eq!(*requested, WorkspaceAuthority::WorkspaceWrite);
            assert_eq!(*source, CALLER_SANDBOX);
        }
        other => panic!("wrong error variant: {other:?}"),
    }
    let text = err.to_string();
    assert!(text.contains("never widened"), "{text}");
}

/// **Round-2 regression, the reason this slice exists.** The operator bypass
/// (`permission_profile=full|verify`) is a claim for `danger-full-access`, and
/// it must be reconciled with the ceiling on EVERY path.
///
/// Round 1 sat the `Full -> DangerFullAccess` promotion inside the
/// `requested_sandbox: None` arm, so an explicit `sandbox` value SHADOWED the
/// ceiling check entirely. A `codex_55_review` (read-only) dispatch carrying
/// both `sandbox=read-only` and `permission_profile=full` compiled to
/// `Ok(Enforcement::Bypass)` — a contract that *said* read-only while
/// launching codex with `--dangerously-bypass-approvals-and-sandbox`. Leg 2
/// below is that exact escalation; it must be an `authority_conflict`, and
/// under no ceiling may a bypass ever come back as `Bypass`.
///
/// (Round 1 also shipped this test asserting `glm_impl + Full => Ok(Bypass)`,
/// which was self-contradictory — an executor ceiling is `workspace-write`,
/// and a bypass claims more than that — and consequently RED. Leg 3 is the
/// corrected assertion.)
#[test]
fn operator_bypass_cannot_widen_any_profile_ceiling_on_any_path() {
    let review = resolve_dispatch_profile("codex_55_review").expect("profile");
    let executor = resolve_dispatch_profile("glm_impl").expect("profile");
    let skills = Vec::new();

    // Leg 1: read-only profile, sandbox omitted (the only path round 1 checked).
    let mut omitted = certified_inputs("codex", Some(review), &skills);
    omitted.permission_profile = PermissionProfile::Full;

    // Leg 2: read-only profile + an EXPLICIT sandbox value — round 1's
    // escalation. The explicit value must not shadow the bypass check.
    let mut shadowed = certified_inputs("codex", Some(review), &skills);
    shadowed.permission_profile = PermissionProfile::Full;
    shadowed.requested_sandbox = Some("read-only");

    // Leg 3: executor profile. Its ceiling is workspace-write; a bypass claims
    // danger-full-access, which is still widening.
    let mut executor_bypass = certified_inputs("codex", Some(executor), &skills);
    executor_bypass.permission_profile = PermissionProfile::Full;

    for (leg, input, ceiling) in [
        (
            "read-only profile, sandbox omitted",
            omitted,
            WorkspaceAuthority::ReadOnly,
        ),
        (
            "read-only profile + explicit sandbox=read-only",
            shadowed,
            WorkspaceAuthority::ReadOnly,
        ),
        (
            "executor profile, sandbox omitted",
            executor_bypass,
            WorkspaceAuthority::WorkspaceWrite,
        ),
    ] {
        let err = match compile_effective_contract(&input) {
            Ok(contract) => panic!(
                "[{leg}] the bypass claims danger-full-access above a '{}' ceiling and must not compile, got {:?}",
                ceiling.as_str(),
                contract.enforcement
            ),
            Err(err) => err,
        };
        assert_eq!(err.code(), "authority_conflict", "[{leg}] {err}");
        match &err {
            ContractError::AuthorityConflict {
                ceiling: got,
                requested,
                source,
                ..
            } => {
                assert_eq!(*got, ceiling, "[{leg}]");
                assert_eq!(
                    *requested,
                    WorkspaceAuthority::DangerFullAccess,
                    "[{leg}] the bypass claims danger-full-access, whatever else was asked for"
                );
                assert_eq!(*source, OPERATOR_BYPASS, "[{leg}]");
            }
            other => panic!("[{leg}] wrong error variant: {other:?}"),
        }
    }
}

/// The caller's own narrowing request is a ceiling too: a dispatch that asks
/// for `read-only` cannot be handed a bypass that gives it everything, even
/// with no profile in play.
#[test]
fn operator_bypass_cannot_widen_an_explicit_caller_narrowing() {
    let skills = Vec::new();
    let mut input = certified_inputs("codex", None, &skills);
    input.requested_sandbox = Some("read-only");
    input.permission_profile = PermissionProfile::Full;

    let err = compile_effective_contract(&input)
        .expect_err("a bypass may not widen the caller's own narrowing");
    assert_eq!(err.code(), "authority_conflict");
    match &err {
        ContractError::AuthorityConflict {
            ceiling,
            ceiling_source,
            source,
            ..
        } => {
            assert_eq!(*ceiling, WorkspaceAuthority::ReadOnly);
            assert_eq!(
                *ceiling_source, CALLER_SANDBOX,
                "the ceiling here came from the caller, not from a profile"
            );
            assert_eq!(*source, OPERATOR_BYPASS);
        }
        other => panic!("wrong error variant: {other:?}"),
    }
}

/// The #878-B escape hatch still exists — but only where nothing has declared
/// a narrower intent: a profile-less dispatch that asked for no sandbox. That
/// is the ONLY shape whose ceiling is `danger-full-access`.
#[test]
fn profile_less_operator_bypass_is_still_the_878b_escape_hatch() {
    let skills = Vec::new();
    let mut input = certified_inputs("codex", None, &skills);
    input.permission_profile = PermissionProfile::Full;

    let contract = compile_effective_contract(&input).expect("the operator hatch still opens");
    assert_eq!(
        contract.workspace_authority,
        WorkspaceAuthority::DangerFullAccess,
        "a bypassed launch has full access — the receipt must say so, not claim the level the caller wished for"
    );
    assert!(matches!(contract.enforcement, Enforcement::Bypass { .. }));
    assert_eq!(
        contract.sandbox_arg, None,
        "a bypassed launch must not also carry a --sandbox flag"
    );
    assert_eq!(contract.network, NetworkAuthority::ProviderDefault);
}

/// Narrowing is always allowed: a write profile may ask for read-only.
#[test]
fn narrowing_below_the_profile_ceiling_is_allowed() {
    let profile = resolve_dispatch_profile("glm_impl").expect("profile");
    let skills = Vec::new();
    let mut input = certified_inputs("codex", Some(profile), &skills);
    input.requested_sandbox = Some("read-only");
    let contract = compile_effective_contract(&input).expect("narrowing must be allowed");
    assert_eq!(
        contract.workspace_authority,
        WorkspaceAuthority::ReadOnly,
        "caller narrowed below the profile ceiling"
    );
    assert_eq!(contract.sandbox_arg.as_deref(), Some("read-only"));
}

#[test]
fn danger_full_access_request_cannot_exceed_a_profile_ceiling() {
    let profile = resolve_dispatch_profile("glm_impl").expect("profile");
    let skills = Vec::new();
    let mut input = certified_inputs("codex", Some(profile), &skills);
    input.requested_sandbox = Some("danger-full-access");
    let err = compile_effective_contract(&input).expect_err("no profile grants full access");
    assert_eq!(err.code(), "authority_conflict");
}

// ── Invariant 3: an omitted sandbox resolves from the profile ────────────

/// Discriminating test ④: an omitted sandbox on a review profile compiles to
/// read-only — NOT codex's workspace-write default (the #894 S2d gap). Stated
/// on the certified binary, because the same dispatch on a codex the receipt
/// does not name is refused outright (see
/// `read_only_lane_is_refused_when_the_installed_codex_is_not_the_certified_one`).
#[test]
fn omitted_sandbox_on_review_profile_resolves_to_read_only() {
    let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
    let skills = Vec::new();
    let input = certified_inputs("codex", Some(profile), &skills);

    let contract = compile_effective_contract(&input).expect("review contract compiles");
    assert_eq!(
        contract.workspace_authority,
        WorkspaceAuthority::ReadOnly,
        "an omitted sandbox must resolve from the effective profile, not the codex default"
    );
    assert_eq!(
        contract.sandbox_arg.as_deref(),
        Some("read-only"),
        "the launcher must receive the compiled level explicitly"
    );
    assert!(matches!(contract.enforcement, Enforcement::Enforced { .. }));
    assert_eq!(contract.network, NetworkAuthority::Restricted);
}

/// An executor profile still gets workspace-write when sandbox is omitted —
/// the fix narrows review lanes, it does not break implementers.
///
/// And note what it is NOT: even on the certified binary, a workspace-write
/// contract is **advisory**. The kill-test only ever exercised `read-only`,
/// so `covers` says read-only, and codex's `--sandbox workspace-write` gets
/// passed as defense-in-depth while the receipt states plainly that nobody
/// has watched it contain anything. Certifying the level we tested and only
/// the level we tested is the whole point of invariant 5.
#[test]
fn omitted_sandbox_on_executor_profile_stays_workspace_write_and_is_only_advisory() {
    let profile = resolve_dispatch_profile("glm_impl").expect("profile");
    let skills = Vec::new();
    let input = certified_inputs("codex", Some(profile), &skills);
    let contract = compile_effective_contract(&input).expect("executor contract compiles");
    assert_eq!(
        contract.workspace_authority,
        WorkspaceAuthority::WorkspaceWrite
    );
    assert_eq!(contract.sandbox_arg.as_deref(), Some("workspace-write"));
    match &contract.enforcement {
        Enforcement::Advisory { reason } => {
            assert!(
                reason.contains("certifies [read-only]"),
                "the receipt must say which level was actually exercised: {reason}"
            );
            assert!(reason.contains("NOT machine-enforced"), "{reason}");
            assert!(
                reason.contains("defense in depth"),
                "the flag is still passed; the receipt says so: {reason}"
            );
        }
        other => {
            panic!("a level no kill-test exercised must not be claimed as enforced: {other:?}")
        }
    }
    assert_eq!(
        contract.network,
        NetworkAuthority::ProviderDefault,
        "no enforcer, no network claim"
    );
}

/// Profile-less codex dispatch keeps the legacy workspace-write default —
/// there is no profile to derive intent from.
#[test]
fn profile_less_dispatch_keeps_legacy_workspace_write_default() {
    let skills = Vec::new();
    let input = inputs("codex", None, &skills);
    let contract = compile_effective_contract(&input).expect("bare codex contract compiles");
    assert_eq!(
        contract.workspace_authority,
        WorkspaceAuthority::WorkspaceWrite
    );
    assert_eq!(contract.sandbox_arg.as_deref(), Some("workspace-write"));
}

// ── Invariant 4: a shell-capable read-only lane needs a certified provider ─

/// **Round-2 fix (invariant 4 was decorative).** The provider-qualification
/// gate must fire on the DERIVED path too, not only when the caller typed a
/// sandbox value. codex over acpx/native-ACP has no primitive (the transport,
/// not the backend name, decides) — and codex `exec` always runs shell
/// unattended, so its profile-derived read-only level is refused pre-spawn
/// rather than quietly downgraded to "advisory", which is what round 1 did.
#[test]
fn derived_read_only_on_an_uncertified_provider_is_refused_not_downgraded() {
    let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
    let skills = Vec::new();

    // Explicit request → refused, labelled by transport (#894 S0 wording).
    let mut explicit = certified_inputs("codex", Some(profile), &skills);
    explicit.transport = "acpx";
    explicit.requested_sandbox = Some("read-only");
    let err = compile_effective_contract(&explicit)
        .expect_err("acpx has no sandbox primitive")
        .to_string();
    assert!(err.contains("acpx"), "{err}");

    // Derived level, nothing typed by the caller → STILL refused: same claim,
    // same missing enforcer.
    let mut derived = certified_inputs("codex", Some(profile), &skills);
    derived.transport = "acpx";
    let err = compile_effective_contract(&derived)
        .expect_err("a derived read-only level on an uncertified provider must be refused");
    assert_eq!(err.code(), "provider_not_qualified");
    let text = err.to_string();
    assert!(text.contains("acpx"), "{text}");
    assert!(text.contains("not kill-test certified"), "{text}");
    assert!(text.contains("shell-capable"), "{text}");
}

/// The scope of invariant 4 is exactly "can this agent run shell unattended".
/// A read-only lane that cannot (claude with the default permission profile:
/// every tool call goes through an approval gate) is allowed to run advisory —
/// and the receipt says out loud that nothing machine-enforces it. Flip the
/// same profile to an allowlist containing `Bash` and it becomes a
/// shell-capable read-only lane on an uncertified provider: refused.
#[test]
fn advisory_read_only_is_only_for_lanes_that_cannot_run_shell_unattended() {
    let profile = resolve_dispatch_profile("claude_plan").expect("profile");
    let skills = Vec::new();

    let gated = inputs("claude", Some(profile), &skills);
    let contract = compile_effective_contract(&gated).expect("a gated read-only lane compiles");
    assert_eq!(contract.workspace_authority, WorkspaceAuthority::ReadOnly);
    assert!(!contract.tool_authority.unattended_shell);
    match &contract.enforcement {
        Enforcement::Advisory { reason } => {
            assert!(reason.contains("NOT machine-enforced"), "{reason}");
            assert!(reason.contains("no sandbox primitive"), "{reason}");
        }
        other => panic!("expected an advisory receipt, got {other:?}"),
    }
    assert_eq!(contract.sandbox_arg, None, "no flag may be fabricated");

    let tools = vec!["Bash".to_string()];
    let mut shell_capable = inputs("claude", Some(profile), &skills);
    shell_capable.permission_profile = PermissionProfile::Allowlist;
    shell_capable.allowed_tools = &tools;
    let err = compile_effective_contract(&shell_capable)
        .expect_err("a read-only lane that can run shell unattended needs a certified enforcer");
    assert_eq!(err.code(), "provider_not_qualified");
}

/// A read-only REQUEST to a backend with no sandbox primitive is refused
/// pre-spawn, keeping the #894 S0 receipt wording and adding the missing
/// qualification.
#[test]
fn read_only_request_to_primitive_less_backend_is_refused() {
    for backend in ["claude", "grok", "kimi", "custom"] {
        let skills = Vec::new();
        let mut input = inputs(backend, None, &skills);
        input.requested_sandbox = Some("read-only");
        let err = compile_effective_contract(&input).unwrap_err().to_string();
        assert!(err.contains(backend), "receipt must name backend: {err}");
        assert!(err.contains("has no sandbox concept"), "{err}");
        assert!(err.contains("fail-closed"), "{err}");
        assert!(
            err.contains("no kill-test-certified sandbox enforcement"),
            "{err}"
        );
    }
}

// ── Invariant 5: certification means kill-tested ─────────────────────────

/// A synthetic receipt for a provider nobody ever kill-tested — used to build
/// tables that *look* certified so the gate has something to refuse.
const UNVERIFIED_ROW: &[ProviderQualification] = &[ProviderQualification {
    backend: "codex",
    transport: TransportKind::Cli,
    certification: Certification::Unverified {
        reason: "synthetic: the kill-test was never executed",
    },
}];

/// Discriminating test ③: a provider that HAS the vendor flag but is not
/// kill-test certified must be refused — whether the table has no row for it
/// at all, or has a row nobody ever ran, or has a *passing* receipt that was
/// issued for a different binary than the one installed. Vendor flag
/// validation is not provider qualification, and a certification for another
/// version is not a certification for this one.
#[test]
fn a_valid_vendor_flag_is_not_a_certification() {
    // The flag itself is valid...
    assert!(validate_codex_sandbox("read-only").is_ok());
    // ...and codex/cli does have a primitive...
    assert!(provider_has_sandbox_primitive("codex", TransportKind::Cli));

    let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
    let skills = Vec::new();
    for (world, table, version, expected) in [
        (
            "an empty table certifies nobody",
            &[] as &[ProviderQualification],
            Some(CERTIFIED_VERSION),
            "no qualification entry",
        ),
        (
            "a row that names no executed run certifies nobody",
            UNVERIFIED_ROW,
            Some(CERTIFIED_VERSION),
            "NOT certified",
        ),
        (
            "the shipped receipt was issued for another binary version",
            PROVIDER_QUALIFICATIONS,
            Some("0.145.0"),
            "does not carry across vendor versions",
        ),
        (
            "the installed version cannot be determined at all",
            PROVIDER_QUALIFICATIONS,
            None,
            "an unknown version is not a certified version",
        ),
    ] {
        let mut input = inputs("codex", Some(profile), &skills);
        input.qualifications = table;
        input.backend_version = version;

        let err = match compile_effective_contract(&input) {
            Ok(contract) => panic!(
                "[{world}] an uncertified provider must be refused pre-spawn, got {:?}",
                contract.enforcement
            ),
            Err(err) => err,
        };
        assert_eq!(err.code(), "provider_not_qualified", "[{world}]");
        let text = err.to_string();
        assert!(
            text.contains("not kill-test certified") && text.contains("read-only"),
            "[{world}] {text}"
        );
        assert!(
            text.contains(expected),
            "[{world}] the receipt must say WHY: {text}"
        );
    }
}

/// **The runtime version gate, end to end.** The shipped row IS certified —
/// and it still refuses the review lane on a codex the kill-test never saw.
/// A conformance result is a property of a binary, not of a brand.
#[test]
fn read_only_lane_is_refused_when_the_installed_codex_is_not_the_certified_one() {
    let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
    let skills = Vec::new();

    // The certified binary → the lane runs.
    let contract = compile_effective_contract(&certified_inputs("codex", Some(profile), &skills))
        .expect("the certified binary runs the review lane");
    match &contract.enforcement {
        Enforcement::Enforced {
            receipt,
            vendor_version,
            certified_by,
            ..
        } => {
            assert_eq!(*receipt, CODEX_CLI_RECEIPT.id);
            assert_eq!(*vendor_version, CERTIFIED_VERSION);
            assert_eq!(*certified_by, CODEX_CLI_RECEIPT.kill_test);
        }
        other => panic!("the certified binary must be Enforced, got {other:?}"),
    }

    // One patch bump → refused, with a receipt that names the gap.
    let mut upgraded = certified_inputs("codex", Some(profile), &skills);
    upgraded.backend_version = Some("0.144.2");
    let err = compile_effective_contract(&upgraded)
        .expect_err("an uncertified codex version must fail closed");
    assert_eq!(err.code(), "provider_not_qualified");
    let text = err.to_string();
    assert!(text.contains("0.144.2"), "{text}");
    assert!(text.contains("re-run the kill-test"), "{text}");

    // Version unknown (no codex on PATH) → refused too.
    let err = compile_effective_contract(&inputs("codex", Some(profile), &skills))
        .expect_err("an unknown codex version must fail closed");
    assert_eq!(err.code(), "provider_not_qualified");
    assert!(
        err.to_string()
            .contains("an unknown version is not a certified version"),
        "{err}"
    );
}

/// **The ratchet, rewritten for receipts.** Round 2's version said "a
/// `KillTested` row may not point at an `#[ignore]`d test" — which forced the
/// certifying test into the ordinary suite, where a 60s real-binary probe
/// cannot live. The truer statement is this one: a row may claim `KillTested`
/// only if it carries a receipt of an execution that **passed**, names a
/// kill-test that **exists** and contains the function it says was run, and
/// certifies **at least one level** over a **non-empty matrix**. The
/// `#[ignore]` is fine; the fabrication is not.
#[test]
fn every_certified_row_is_backed_by_a_passing_executed_receipt() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");

    for entry in PROVIDER_QUALIFICATIONS {
        let Certification::KillTested { receipt } = entry.certification else {
            continue;
        };
        let id = receipt.id;
        assert_eq!(
            receipt.result,
            CertificationResult::Pass,
            "'{id}' certifies a run that did not pass"
        );
        assert_eq!(
            receipt.backend, entry.backend,
            "'{id}' attests another backend"
        );
        assert_eq!(
            receipt.transport, entry.transport,
            "'{id}' attests another transport"
        );
        assert!(
            !receipt.covers.is_empty() && !receipt.matrix.is_empty(),
            "'{id}' certifies nothing: it must name the levels it exercised and the mutations it saw refused"
        );
        assert!(
            !receipt.vendor_version.trim().is_empty(),
            "'{id}' must name the binary version it exercised — a version-less certification is a blank cheque"
        );

        let path = repo_root.join(receipt.kill_test);
        assert!(
            path.exists(),
            "'{id}' claims certification by '{}', which does not exist",
            receipt.kill_test
        );
        let source = std::fs::read_to_string(&path).expect("kill-test source");
        assert!(
            source.contains(&format!("fn {}", receipt.kill_test_fn)),
            "'{id}' says '{}' ran, but '{}' contains no such test",
            receipt.kill_test_fn,
            receipt.kill_test
        );

        let receipt_file = repo_root.join(receipt.source_file);
        assert!(
            receipt_file.exists(),
            "'{id}' has no checked-in artifact of record at '{}' — an audit nobody can read is not an audit",
            receipt.source_file
        );
    }
}

/// The kill-test that certifies codex is allowed — required, even — to stay
/// `#[ignore]`d: it needs a real binary, real credentials and ~60s. Pinned so
/// nobody "fixes" it into the ordinary suite, where it would either fail on a
/// codex-less box or, far worse, go green without exercising anything.
#[test]
fn the_certifying_kill_test_stays_out_of_the_ordinary_suite() {
    assert!(
        KILL_TEST_SOURCE.contains("#[ignore"),
        "the real-binary kill-test must stay #[ignore]d; certification is an out-of-band event with a receipt (invariant 5)"
    );
}

#[test]
fn shipped_table_certifies_codex_cli_read_only_and_nothing_else() {
    assert_eq!(PROVIDER_QUALIFICATIONS.len(), 1);
    let entry = &PROVIDER_QUALIFICATIONS[0];
    assert_eq!(entry.backend, "codex");
    assert_eq!(entry.transport, TransportKind::Cli);
    assert_eq!(
        entry.certification.receipt().map(|r| r.id),
        Some(CODEX_CLI_RECEIPT.id)
    );

    // codex/cli, certified version, read-only → the one thing that qualifies.
    assert!(qualify_provider(
        PROVIDER_QUALIFICATIONS,
        "codex",
        TransportKind::Cli,
        Some(CERTIFIED_VERSION),
        CODEX_CLI_RECEIPT.host_os,
        WorkspaceAuthority::ReadOnly
    )
    .is_ok());

    // Every other backend: no row, no certification.
    for backend in ["claude", "grok", "kimi", "custom", "opencode"] {
        assert!(
            qualify_provider(
                PROVIDER_QUALIFICATIONS,
                backend,
                TransportKind::Cli,
                Some(CERTIFIED_VERSION),
                CODEX_CLI_RECEIPT.host_os,
                WorkspaceAuthority::ReadOnly
            )
            .is_err(),
            "'{backend}' must not be certified to enforce read-only"
        );
    }

    // codex over any other transport: the flag never reaches the child.
    for transport in [
        TransportKind::Acpx,
        TransportKind::AcpNative,
        TransportKind::HarnessServe,
    ] {
        assert!(
            qualify_provider(
                PROVIDER_QUALIFICATIONS,
                "codex",
                transport,
                Some(CERTIFIED_VERSION),
                CODEX_CLI_RECEIPT.host_os,
                WorkspaceAuthority::ReadOnly
            )
            .is_err(),
            "codex/{} has no sandbox primitive and must not be certified",
            transport.as_str()
        );
    }

    // Levels the run never exercised: never certified, on any version.
    for level in [
        WorkspaceAuthority::WorkspaceWrite,
        WorkspaceAuthority::DangerFullAccess,
    ] {
        let err = qualify_provider(
            PROVIDER_QUALIFICATIONS,
            "codex",
            TransportKind::Cli,
            Some(CERTIFIED_VERSION),
            CODEX_CLI_RECEIPT.host_os,
            level,
        )
        .expect_err("only the exercised level is certified");
        assert!(err.contains("certifies [read-only]"), "{err}");
    }
}

#[test]
fn sandbox_receipt_is_scoped_to_host_os_in_both_directions() {
    const LINUX_RECEIPT: CertificationReceipt = CertificationReceipt {
        id: "synthetic-linux-host-scope",
        host_os: "linux",
        host_os_version: "synthetic",
        ..CODEX_CLI_RECEIPT
    };
    const LINUX_TABLE: &[ProviderQualification] = &[ProviderQualification {
        backend: "codex",
        transport: TransportKind::Cli,
        certification: Certification::KillTested {
            receipt: &LINUX_RECEIPT,
        },
    }];
    let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
    for (table, certified_host) in [(PROVIDER_QUALIFICATIONS, "macos"), (LINUX_TABLE, "linux")] {
        for host in ["macos", "linux", "windows", "unknown", ""] {
            let qualification = qualify_provider(
                table,
                "codex",
                TransportKind::Cli,
                Some(CERTIFIED_VERSION),
                host,
                WorkspaceAuthority::ReadOnly,
            );
            let mut input = certified_inputs("codex", Some(profile), &[]);
            input.qualifications = table;
            input.host_os = host;
            let contract = compile_effective_contract(&input);
            if host == certified_host {
                qualification.expect("matching host remains qualified");
                assert!(matches!(
                    contract.unwrap().enforcement,
                    Enforcement::Enforced { .. }
                ));
            } else {
                let error = qualification.expect_err("another host cannot inherit certification");
                assert!(error.contains("host OS"), "{error}");
                assert!(error.contains(certified_host), "{error}");
                let error = contract.expect_err("read-only dispatch must refuse before launch");
                assert_eq!(error.code(), "provider_not_qualified");
                assert!(error.to_string().contains("host OS"), "{error}");
            }
        }
    }
}

/// An `Unverified` row is never handed back, no matter how well it matches:
/// same backend, same transport, level asked for, version known — and still
/// refused, because nobody has watched it enforce anything.
#[test]
fn an_unverified_row_never_qualifies() {
    let err = qualify_provider(
        UNVERIFIED_ROW,
        "codex",
        TransportKind::Cli,
        Some("9.9.9"),
        CODEX_CLI_RECEIPT.host_os,
        WorkspaceAuthority::ReadOnly,
    )
    .expect_err("an uncertified row must not qualify");
    assert!(err.contains("NOT certified"), "{err}");
    assert!(err.contains("never executed"), "{err}");
}

/// A receipt that records a FAILED run is expressible on purpose — a
/// regression is worth checking in — and it must certify exactly nothing.
#[test]
fn a_failed_receipt_never_qualifies() {
    // Spelled out rather than a functional update of CODEX_CLI_RECEIPT:
    // struct-update syntax is not available in a const initializer.
    const FAILED: CertificationReceipt = CertificationReceipt {
        id: "synthetic-failed-run",
        source_file: "crates/tachi-dispatch/certifications/codex-cli.toml",
        backend: "codex",
        transport: TransportKind::Cli,
        vendor_binary: "codex-cli",
        vendor_version: CODEX_CLI_RECEIPT.vendor_version,
        host_os: "macos",
        host_os_version: "26.5.1",
        kill_test: CODEX_CLI_RECEIPT.kill_test,
        kill_test_fn: CODEX_CLI_RECEIPT.kill_test_fn,
        result: CertificationResult::Fail,
        executed_at: "2026-07-13",
        executed_by: "synthetic",
        duration_secs: "0.0",
        executed_on_commit: "0000000000000000000000000000000000000000",
        kill_test_source_blob: "0000000000000000000000000000000000000000",
        covers: &[WorkspaceAuthority::ReadOnly],
        matrix: &["create"],
    };
    const TABLE: &[ProviderQualification] = &[ProviderQualification {
        backend: "codex",
        transport: TransportKind::Cli,
        certification: Certification::KillTested { receipt: &FAILED },
    }];
    let err = qualify_provider(
        TABLE,
        "codex",
        TransportKind::Cli,
        Some(CERTIFIED_VERSION),
        CODEX_CLI_RECEIPT.host_os,
        WorkspaceAuthority::ReadOnly,
    )
    .expect_err("a failed kill-test certifies nothing");
    assert!(err.contains("FAILED kill-test run"), "{err}");
}

// ── Invariant 6: a mounted skill is an input, not a permission ───────────

/// Discriminating test ②: a skill that needs writes is EXCLUDED (with a
/// reason) under a read-only contract — it cannot widen it.
#[test]
fn write_intent_skill_is_excluded_under_a_read_only_contract() {
    let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
    let skills = vec![
        SkillRequest::new(crate::native_skill_ids::WAZA_CHECK),
        SkillRequest::new(crate::native_skill_ids::SUPERPOWER_EXECUTING_PLANS),
        SkillRequest::new(crate::native_skill_ids::WAZA_WRITE),
    ];
    let input = certified_inputs("codex", Some(profile), &skills);

    let contract = compile_effective_contract(&input).expect("contract compiles");
    assert_eq!(contract.workspace_authority, WorkspaceAuthority::ReadOnly);
    assert_eq!(
        contract.mounted_skills,
        vec![crate::native_skill_ids::WAZA_CHECK.to_string()],
        "only the read-only skill stays mounted"
    );
    let excluded = contract
        .excluded_skills
        .iter()
        .map(|s| s.skill_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        excluded,
        vec![
            crate::native_skill_ids::SUPERPOWER_EXECUTING_PLANS,
            crate::native_skill_ids::WAZA_WRITE
        ]
    );
    for skill in &contract.excluded_skills {
        assert!(
            skill.reason.contains("input, not a grant of authority"),
            "exclusion must carry a reason: {}",
            skill.reason
        );
    }
    // The same skills stay mounted when the contract permits writes.
    let exec_profile = resolve_dispatch_profile("glm_impl").expect("profile");
    let write_contract =
        compile_effective_contract(&certified_inputs("codex", Some(exec_profile), &skills))
            .expect("contract compiles");
    assert_eq!(write_contract.mounted_skills.len(), 3);
    assert!(write_contract.excluded_skills.is_empty());
}

// ── Plumbing ────────────────────────────────────────────────────────────

#[test]
fn blank_and_unknown_sandbox_values_are_typed_errors() {
    let skills = Vec::new();
    for blank in ["", "   ", "\t\n"] {
        let mut input = inputs("codex", None, &skills);
        input.requested_sandbox = Some(blank);
        let err = compile_effective_contract(&input).expect_err("blank is malformed");
        assert_eq!(err.code(), "invalid_sandbox");
        assert!(err.to_string().contains("blank"), "{err}");
    }
    let mut input = inputs("codex", None, &skills);
    input.requested_sandbox = Some("workspace-writ");
    let err = compile_effective_contract(&input).expect_err("typo is rejected");
    assert_eq!(err.code(), "invalid_sandbox");
}

#[test]
fn tool_authority_is_a_narrowing_and_of_profile_and_mcp_loadout() {
    let profile = resolve_dispatch_profile("glm_impl").expect("profile");
    let skills = Vec::new();
    let mut input = inputs("codex", Some(profile), &skills);
    input.mcp_write_actions = Some(false);
    input.mcp_github_read = Some(true);
    let contract = compile_effective_contract(&input).expect("contract compiles");
    assert!(
        !contract.tool_authority.write_actions,
        "MCP loadout may narrow the profile grant"
    );
    assert!(
        !contract.tool_authority.github_read,
        "profile github_read=false wins over an MCP loadout that asks for true"
    );
    assert!(contract.tool_authority.unattended_shell);
}

#[test]
fn every_shipped_profile_compiles_to_the_expected_ceiling() {
    for (name, expected) in [
        ("claude_plan", WorkspaceAuthority::ReadOnly),
        ("codex_astra_review", WorkspaceAuthority::ReadOnly),
        ("codex_sol_review", WorkspaceAuthority::ReadOnly),
        ("codex_terra_write", WorkspaceAuthority::WorkspaceWrite),
        ("opencode_sol_review", WorkspaceAuthority::ReadOnly),
        ("opencode_astra_review", WorkspaceAuthority::ReadOnly),
        ("kimi_arch", WorkspaceAuthority::ReadOnly),
        ("kimi_ux", WorkspaceAuthority::ReadOnly),
        ("deepseek_explore", WorkspaceAuthority::ReadOnly),
        ("glm_impl", WorkspaceAuthority::WorkspaceWrite),
        ("opencode_terra_write", WorkspaceAuthority::WorkspaceWrite),
    ] {
        let profile = crate::resolve_dispatch_profile(name).expect("profile");
        assert_eq!(
            profile_ceiling(Some(profile)),
            expected,
            "profile '{name}' ceiling"
        );
    }
}

#[test]
fn transport_kind_matches_the_server_side_alias_lists() {
    for alias in ["acpx", "acp", " ACP "] {
        assert_eq!(transport_kind(alias), TransportKind::Acpx, "{alias}");
    }
    for alias in [
        "acp-native",
        "acp_native",
        "native-acp",
        "native_acp",
        "acp-rs",
        "acp_rs",
    ] {
        assert_eq!(transport_kind(alias), TransportKind::AcpNative, "{alias}");
    }
    for alias in ["serve", "opencode_serve", "server"] {
        assert_eq!(
            transport_kind(alias),
            TransportKind::HarnessServe,
            "{alias}"
        );
    }
    assert_eq!(transport_kind("cli"), TransportKind::Cli);
    assert_eq!(transport_kind(""), TransportKind::Cli);
}

#[test]
fn native_profiles_require_actual_server_boundary_and_fixed_transport() {
    for name in [
        "codex_sol_review",
        "codex_terra_write",
        "opencode_astra_review",
        "opencode_terra_write",
    ] {
        let profile = crate::resolve_dispatch_profile(name).unwrap();
        let mut input = inputs(profile.backend, Some(profile), &[]);
        input.transport = "acp_native";
        input.host_os = "linux";
        assert!(matches!(
            compile_effective_contract(&input),
            Err(ContractError::ProviderNotQualified { .. })
        ));
        input.native_sandbox_supported = true;
        let contract = compile_effective_contract(&input).expect("server-enforced native grant");
        assert!(matches!(
            contract.enforcement,
            Enforcement::ServerSandbox { .. }
        ));
        assert_eq!(contract.workspace_authority, profile_ceiling(Some(profile)));
        for transport in ["cli", "acpx", "opencode_serve"] {
            input.transport = transport;
            assert!(
                compile_effective_contract(&input).is_err(),
                "native profile cannot downgrade to {transport}"
            );
        }
        input.transport = "acp_native";
        input.host_os = "windows";
        assert!(compile_effective_contract(&input).is_err());
    }
}

#[test]
fn native_read_only_profile_rejects_write_and_bypass_claims() {
    let profile = crate::resolve_dispatch_profile("codex_sol_review").unwrap();
    let mut input = inputs("codex", Some(profile), &[]);
    input.transport = "acp_native";
    input.host_os = "linux";
    input.native_sandbox_supported = true;
    input.requested_sandbox = Some("workspace-write");
    assert!(matches!(
        compile_effective_contract(&input),
        Err(ContractError::AuthorityConflict { .. })
    ));
    input.requested_sandbox = None;
    input.permission_profile = PermissionProfile::Full;
    assert!(matches!(
        compile_effective_contract(&input),
        Err(ContractError::AuthorityConflict { .. })
    ));
}

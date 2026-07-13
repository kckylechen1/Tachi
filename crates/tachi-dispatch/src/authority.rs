//! Effective-authority contract compiler + provider qualification (#894 S2d).
//!
//! Owner-ratified HYBRID design (sol codex-e0255): Tachi does **not** build
//! macOS isolation primitives. It owns three things and nothing else:
//!
//! 1. a **normalized contract** — profile + requested sandbox + skills + MCP
//!    loadout compiled into one typed [`EffectiveContract`];
//! 2. **provider qualification** — which `backend x transport x version` has
//!    been proven, by a real-process kill-test, to actually *enforce* a level;
//! 3. **fail-closed routing + receipts** — a level that nothing can enforce is
//!    refused *before spawn*, and whatever we do run carries a receipt saying
//!    exactly who enforces it (or that nobody does).
//!
//! The frozen invariants this module implements:
//!
//! 1. **Authority is monotone.** Profile, skills, MCP loadout, memory,
//!    credentials, caller override and fallback routing may only *preserve or
//!    narrow* the compiled authority — never widen it.
//! 2. A read-only profile plus an explicit `workspace-write` request is a
//!    **type conflict** ([`ContractError::AuthorityConflict`]), not a
//!    compatible override.
//! 3. An **omitted** sandbox resolves from the *effective profile* (before the
//!    backend is chosen), not from a backend default — `codex_55_review` with
//!    no `sandbox` argument compiles to `read-only`, where before #894 S2d it
//!    silently fell through to codex's `workspace-write` default.
//! 4. A shell-capable read-only dispatch may only start on a **qualified**
//!    provider; unknown/uncertified providers are refused **pre-spawn** (no run
//!    directory, no credential materialization), not left to fail after spawn.
//! 5. **Vendor flag validation is not provider qualification.** `codex
//!    --sandbox read-only` being a *valid flag* ([`crate::validate_codex_sandbox`])
//!    says nothing about enforcement; only [`PROVIDER_QUALIFICATIONS`], whose
//!    entries are backed by a real-process kill-test, does.
//! 6. **A mounted skill is an input, not a permission.** A skill that declares
//!    workspace-write intent cannot widen a read-only contract — it is
//!    *excluded* from the mount, with a reason in the receipt.

use crate::launcher::{reject_unsupported_sandbox, validate_codex_sandbox, PermissionProfile};
use crate::profiles::DispatchProfileDef;
use serde::Serialize;
use std::fmt;

// ─── Transport ───────────────────────────────────────────────────────────────

/// Transport classes a dispatch can travel on. Single-sourced here (the
/// `tachi-server` predicates delegate to this) so the alias lists cannot drift
/// between the sandbox gate and the launch path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// Vendor CLI subprocess — the only transport where a vendor sandbox flag
    /// can reach the child process at all.
    Cli,
    Acpx,
    AcpNative,
    /// `opencode serve` attach.
    HarnessServe,
}

pub fn transport_kind(transport: &str) -> TransportKind {
    match transport.trim().to_ascii_lowercase().as_str() {
        "acpx" | "acp" => TransportKind::Acpx,
        "acp-native" | "acp_native" | "native-acp" | "native_acp" | "acp-rs" | "acp_rs" => {
            TransportKind::AcpNative
        }
        "serve" | "opencode_serve" | "server" => TransportKind::HarnessServe,
        _ => TransportKind::Cli,
    }
}

impl TransportKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Acpx => "acpx",
            Self::AcpNative => "acp-native",
            Self::HarnessServe => "opencode_serve",
        }
    }
}

// ─── Authority lattice ───────────────────────────────────────────────────────

/// Workspace authority, ordered: `ReadOnly < WorkspaceWrite < DangerFullAccess`.
/// The derived `Ord` IS the lattice — every narrowing check in this module is a
/// comparison against it, so variant order is load-bearing, not cosmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspaceAuthority {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl WorkspaceAuthority {
    /// The codex `--sandbox` policy value for this level. This is the *only*
    /// place the enum is turned back into a vendor flag.
    pub fn as_codex_sandbox(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    pub fn as_str(self) -> &'static str {
        self.as_codex_sandbox()
    }

    /// Parse a caller-supplied sandbox request. Blank/whitespace-only input is
    /// malformed (NOT equivalent to omitting the field — that distinction is
    /// #894 S0's and is preserved verbatim here).
    pub fn parse_request(raw: &str) -> Result<Self, ContractError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ContractError::InvalidSandbox(
                "invalid sandbox value: blank/whitespace-only sandbox request is malformed input, not equivalent to omitting it (fail-closed, #894 S0)"
                    .to_string(),
            ));
        }
        let value = validate_codex_sandbox(trimmed).map_err(ContractError::InvalidSandbox)?;
        Ok(match value {
            "read-only" => Self::ReadOnly,
            "workspace-write" => Self::WorkspaceWrite,
            _ => Self::DangerFullAccess,
        })
    }
}

/// Network reach of the compiled contract. Tachi does not *set* network policy
/// — it only reports the truth: a certified vendor sandbox denies network by
/// default; everything else is whatever the provider does on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAuthority {
    /// Restricted by the enforcing vendor sandbox.
    Restricted,
    /// Not restricted by us and not provably restricted by anyone else.
    ProviderDefault,
}

/// Non-filesystem authority carried by the contract. Every field is an AND of
/// the profile's grant with the MCP loadout's grant — narrowing only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ToolAuthority {
    /// The dispatch can run tools/shell unattended (no human in the approval
    /// loop). This is what makes a read-only contract load-bearing enough to
    /// require a *qualified* enforcer (invariant 4).
    pub unattended_shell: bool,
    pub github_read: bool,
    /// GitHub/MCP write actions (issue/PR mutation), not filesystem writes.
    pub write_actions: bool,
}

/// Who — if anyone — actually stops a write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Enforcement {
    /// A kill-test-certified vendor sandbox enforces this level.
    Enforced {
        provider: String,
        certified_by: &'static str,
    },
    /// Nothing machine-enforces this level on this provider: the contract is
    /// prompt/permission-level only. Stated out loud in the receipt rather than
    /// implied by silence — an advisory read-only lane is NOT isolation.
    Advisory { reason: String },
    /// Operator escape hatch: `permission_profile=full|verify` with the env
    /// opt-in bypasses the vendor sandbox entirely (pre-existing #878-B
    /// behavior). Never reachable from a read-only profile — that combination
    /// is an [`ContractError::AuthorityConflict`].
    Bypass { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExcludedSkill {
    pub skill_id: String,
    pub reason: String,
}

/// A skill the caller/profile asked to mount, with the only thing the compiler
/// needs to know about it: whether its instructions require workspace writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRequest {
    pub id: String,
    pub requires_workspace_write: bool,
}

impl SkillRequest {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        let requires_workspace_write = native_skill_requires_workspace_write(&id);
        Self {
            id,
            requires_workspace_write,
        }
    }
}

/// Native skills whose contract text tells the agent to edit files / land
/// branches. Mounting one under a read-only contract is a category error: the
/// skill would be instructing an agent to do something the enforcer will refuse,
/// so it is excluded with a reason instead of being silently mounted (or,
/// worse, silently widening the contract).
pub fn native_skill_requires_workspace_write(skill_id: &str) -> bool {
    matches!(
        skill_id,
        crate::native_skill_ids::SUPERPOWER_EXECUTING_PLANS
            | crate::native_skill_ids::SUPERPOWER_FINISHING_BRANCH
            | crate::native_skill_ids::WAZA_WRITE
    )
}

/// The compiled, typed authority contract for one dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveContract {
    pub workspace_authority: WorkspaceAuthority,
    pub network: NetworkAuthority,
    pub tool_authority: ToolAuthority,
    pub enforcement: Enforcement,
    /// The vendor sandbox flag value to hand the launcher, if the provider has
    /// a sandbox primitive at all. `None` means "do not pass a sandbox flag"
    /// (the provider has no primitive) — never "use the vendor default".
    pub sandbox_arg: Option<String>,
    pub mounted_skills: Vec<String>,
    pub excluded_skills: Vec<ExcludedSkill>,
    /// Human-readable derivation trail, stamped into the dispatch receipt.
    pub explanation: Vec<String>,
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// Something tried to *widen* the profile's authority. Invariant 1/2.
    AuthorityConflict {
        profile: String,
        profile_authority: WorkspaceAuthority,
        requested: WorkspaceAuthority,
        source: &'static str,
    },
    /// Malformed or unknown sandbox value.
    InvalidSandbox(String),
    /// The provider has no sandbox primitive at all (#894 S0 wording preserved).
    UnsupportedSandbox(String),
    /// The provider has the flag but no kill-test certification for this level
    /// (invariant 5). Refused pre-spawn.
    ProviderNotQualified {
        provider: String,
        version: Option<String>,
        level: WorkspaceAuthority,
        reason: String,
    },
    /// `resolve_permission_profile` rejected the request.
    PermissionProfile(String),
}

impl ContractError {
    /// Stable machine code for receipts/ledger rows.
    pub fn code(&self) -> &'static str {
        match self {
            Self::AuthorityConflict { .. } => "authority_conflict",
            Self::InvalidSandbox(_) => "invalid_sandbox",
            Self::UnsupportedSandbox(_) => "unsupported_sandbox",
            Self::ProviderNotQualified { .. } => "provider_not_qualified",
            Self::PermissionProfile(_) => "permission_profile",
        }
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorityConflict {
                profile,
                profile_authority,
                requested,
                source,
            } => write!(
                f,
                "authority conflict: profile '{profile}' compiles to workspace authority '{}', but {source} asks for '{}'; authority may only be preserved or narrowed, never widened — this is a type conflict, not a compatible override (fail-closed, #894 S2d)",
                profile_authority.as_str(),
                requested.as_str()
            ),
            Self::InvalidSandbox(msg) | Self::UnsupportedSandbox(msg) => write!(f, "{msg}"),
            Self::ProviderNotQualified {
                provider,
                version,
                level,
                reason,
            } => write!(
                f,
                "permission receipt: provider '{provider}'{} is not kill-test certified to enforce workspace authority '{}' ({reason}); a valid vendor flag is not provider qualification — refusing before spawn rather than pretending to isolate (fail-closed, #894 S2d)",
                version
                    .as_deref()
                    .map(|v| format!(" version '{v}'"))
                    .unwrap_or_else(|| " (version unknown)".to_string()),
                level.as_str()
            ),
            Self::PermissionProfile(msg) => write!(f, "{msg}"),
        }
    }
}

impl From<ContractError> for String {
    fn from(err: ContractError) -> Self {
        err.to_string()
    }
}

// ─── Provider qualification ──────────────────────────────────────────────────

/// Which `backend x transport` pairs even *have* a sandbox primitive — i.e. can
/// accept a sandbox flag that reaches the child process. Having a primitive is
/// necessary but NOT sufficient to enforce a level; see
/// [`PROVIDER_QUALIFICATIONS`] (invariant 5).
pub const SANDBOX_PRIMITIVE_PROVIDERS: &[(&str, TransportKind)] = &[("codex", TransportKind::Cli)];

pub fn provider_has_sandbox_primitive(backend: &str, transport: TransportKind) -> bool {
    SANDBOX_PRIMITIVE_PROVIDERS
        .iter()
        .any(|(name, kind)| *name == backend && *kind == transport)
}

/// Version scope of a qualification entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionScope {
    /// Every version of this provider is certified (no version-specific
    /// regression is known).
    Any,
    /// Certified only from this version up. A dispatch whose provider version
    /// cannot be determined fails CLOSED against an `AtLeast` scope — an
    /// unknown version is not an old-enough version.
    AtLeast(&'static str),
}

/// One certified provider: this `backend x transport x version` was observed,
/// by a real-process kill-test, to actually refuse every mutation at the listed
/// levels.
#[derive(Debug, Clone, Copy)]
pub struct ProviderQualification {
    pub backend: &'static str,
    pub transport: TransportKind,
    pub versions: VersionScope,
    pub enforces: &'static [WorkspaceAuthority],
    /// The kill-test that certifies this row. Re-run it to re-certify.
    pub certified_by: &'static str,
}

/// The qualification table. **codex CLI is the only entry** — it is the only
/// backend Tachi dispatches that ships a real sandbox primitive (owner-ratified,
/// sol codex-e0255). Every other backend/transport is uncertified by definition,
/// so a read-only *request* to it is refused pre-spawn.
///
/// `certified_by` points at the real-binary kill-test
/// (`crates/tachi-dispatch/tests/codex_sandbox_kill_test.rs`), which is
/// `#[ignore]`d because it needs a real `codex` binary on PATH and writes to a
/// throwaway worktree — CI cannot run it unattended. Re-certification is a
/// manual step, documented on the test itself.
pub const PROVIDER_QUALIFICATIONS: &[ProviderQualification] = &[ProviderQualification {
    backend: "codex",
    transport: TransportKind::Cli,
    versions: VersionScope::Any,
    enforces: &[
        WorkspaceAuthority::ReadOnly,
        WorkspaceAuthority::WorkspaceWrite,
    ],
    certified_by: "crates/tachi-dispatch/tests/codex_sandbox_kill_test.rs",
}];

/// Parse a dotted version into comparable numeric components. String ordering
/// is NOT version ordering ("0.9" > "0.10" lexically) — this is compared
/// numerically on purpose.
fn version_components(raw: &str) -> Option<Vec<u64>> {
    let cleaned = raw.trim().trim_start_matches('v');
    let head = cleaned
        .split(|c: char| c == '-' || c == '+' || c == ' ')
        .next()?;
    let parts = head
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

fn version_at_least(actual: &str, minimum: &str) -> bool {
    let (Some(actual), Some(minimum)) = (version_components(actual), version_components(minimum))
    else {
        return false;
    };
    let len = actual.len().max(minimum.len());
    for idx in 0..len {
        let a = actual.get(idx).copied().unwrap_or(0);
        let m = minimum.get(idx).copied().unwrap_or(0);
        match a.cmp(&m) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    true
}

/// Look up whether `backend x transport x version` is certified to enforce
/// `level`. `table` is a parameter (not the const) so the qualification policy
/// itself is testable against synthetic tables.
pub fn qualify_provider<'a>(
    table: &'a [ProviderQualification],
    backend: &str,
    transport: TransportKind,
    version: Option<&str>,
    level: WorkspaceAuthority,
) -> Result<&'a ProviderQualification, String> {
    // Starts as the no-entry-at-all reason; each rejected candidate replaces it
    // with the specific reason it was rejected.
    let mut last_reason = format!(
        "no qualification entry for '{backend}/{}'",
        transport.as_str()
    );
    for entry in table
        .iter()
        .filter(|entry| entry.backend == backend && entry.transport == transport)
    {
        match entry.versions {
            VersionScope::Any => {}
            VersionScope::AtLeast(minimum) => match version {
                None => {
                    last_reason = format!(
                        "qualification requires version >= {minimum} but the provider version could not be determined; an unknown version is not an old-enough version"
                    );
                    continue;
                }
                Some(actual) if !version_at_least(actual, minimum) => {
                    last_reason =
                        format!("qualification requires version >= {minimum}, got '{actual}'");
                    continue;
                }
                Some(_) => {}
            },
        }
        if !entry.enforces.contains(&level) {
            last_reason = format!(
                "certified for [{}], not '{}'",
                entry
                    .enforces
                    .iter()
                    .map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                level.as_str()
            );
            continue;
        }
        return Ok(entry);
    }
    Err(last_reason)
}

// ─── The compiler ────────────────────────────────────────────────────────────

/// Everything the compiler is allowed to look at. Assembled by the dispatch
/// entry point *before* the run directory, staging, credentials or the backend
/// choice exist — that ordering is the whole point (invariants 3 and 4).
pub struct ContractInputs<'a> {
    pub backend: &'a str,
    pub transport: &'a str,
    /// Provider version, when the caller could determine it. `None` is honest
    /// ignorance and fails closed against version-scoped qualification entries.
    pub backend_version: Option<&'a str>,
    pub profile: Option<&'a DispatchProfileDef>,
    /// The caller's explicit `sandbox` argument, if any.
    pub requested_sandbox: Option<&'a str>,
    pub permission_profile: PermissionProfile,
    pub allowed_tools: &'a [String],
    pub skills: &'a [SkillRequest],
    /// From the resolved MCP loadout (`DispatchMcpAccessParams`).
    pub mcp_write_actions: Option<bool>,
    pub mcp_github_read: Option<bool>,
    pub qualifications: &'a [ProviderQualification],
}

/// The authority ceiling a profile grants: the *most* any downstream input may
/// end up with. `write_actions` is the profile's declared mutation intent;
/// `role == "executor"` is belt-and-braces so a future executor profile that
/// leaves `write_actions` false (it only means "no GitHub writes") is not
/// silently squeezed to read-only.
fn profile_ceiling(profile: Option<&DispatchProfileDef>) -> WorkspaceAuthority {
    match profile {
        Some(profile) if profile.write_actions || profile.role == "executor" => {
            WorkspaceAuthority::WorkspaceWrite
        }
        Some(_) => WorkspaceAuthority::ReadOnly,
        // No profile: there is no declared intent to narrow from, so the ceiling
        // stays where it has always been (a bare `agent='codex'` dispatch can
        // still be handed `danger-full-access` explicitly). The *default* below
        // is a separate, lower value.
        None => WorkspaceAuthority::DangerFullAccess,
    }
}

/// The authority a dispatch gets when the caller omits `sandbox`. THIS is the
/// #894 S2d default fix: it is derived from the effective profile, not from the
/// backend. Only a profile-less dispatch keeps the legacy `workspace-write`
/// default (there is no profile to derive intent from).
fn profile_default(profile: Option<&DispatchProfileDef>) -> WorkspaceAuthority {
    match profile {
        Some(_) => profile_ceiling(profile),
        None => WorkspaceAuthority::WorkspaceWrite,
    }
}

/// Does this dispatch run tools unattended? codex `exec` and the custom/opencode
/// subprocess backends always do; the permission-gated CLIs only do so with the
/// `full`/`verify` bypass or an allowlist that includes an execution/edit tool.
fn unattended_shell(
    backend: &str,
    permission_profile: PermissionProfile,
    allowed_tools: &[String],
) -> bool {
    if matches!(backend, "codex" | "custom" | "opencode") {
        return true;
    }
    match permission_profile {
        PermissionProfile::Full => true,
        PermissionProfile::Allowlist => allowed_tools.iter().any(|tool| {
            let tool = tool.to_ascii_lowercase();
            tool.starts_with("bash")
                || tool.starts_with("write")
                || tool.starts_with("edit")
                || tool.starts_with("multiedit")
                || tool.starts_with("notebookedit")
        }),
        PermissionProfile::Default => false,
    }
}

/// Compile profile + requested sandbox + skills + MCP loadout into one typed
/// contract, or a typed error. Pure: no filesystem, no environment, no spawn.
pub fn compile_effective_contract(
    inputs: &ContractInputs<'_>,
) -> Result<EffectiveContract, ContractError> {
    let transport = transport_kind(inputs.transport);
    let has_primitive = provider_has_sandbox_primitive(inputs.backend, transport);
    let provider_label = match transport {
        TransportKind::Acpx => "acpx".to_string(),
        TransportKind::AcpNative => "acp-native".to_string(),
        _ => inputs.backend.to_string(),
    };
    let profile_name = inputs
        .profile
        .map(|p| p.name.to_string())
        .unwrap_or_else(|| "<none>".to_string());
    let ceiling = profile_ceiling(inputs.profile);
    let default = profile_default(inputs.profile);
    let mut explanation = Vec::new();

    // 1. A provider with no sandbox primitive cannot honor ANY explicit sandbox
    //    request — reject with the #894 S0 receipt (blank-vs-absent distinction
    //    included), now also naming the missing qualification.
    if let Some(raw) = inputs.requested_sandbox.filter(|_| !has_primitive) {
        let base = reject_unsupported_sandbox(&provider_label, Some(raw))
            .expect_err("reject_unsupported_sandbox always rejects Some(..)");
        return Err(ContractError::UnsupportedSandbox(format!(
            "{base}; no kill-test-certified sandbox enforcement exists for provider '{provider_label}/{}' (#894 S2d)",
            transport.as_str()
        )));
    }

    // 2. Resolve the requested authority. Explicit request > operator bypass >
    //    profile-derived default. Only the first two can conflict with the
    //    ceiling; the derived default is clamped by construction.
    let (requested, request_source): (WorkspaceAuthority, Option<&'static str>) =
        match inputs.requested_sandbox {
            Some(raw) => (
                WorkspaceAuthority::parse_request(raw)?,
                Some("caller sandbox override"),
            ),
            None if has_primitive && inputs.permission_profile == PermissionProfile::Full => (
                WorkspaceAuthority::DangerFullAccess,
                Some("permission_profile 'full'/'verify' sandbox bypass"),
            ),
            None => (default, None),
        };

    if let Some(source) = request_source {
        if requested > ceiling {
            return Err(ContractError::AuthorityConflict {
                profile: profile_name,
                profile_authority: ceiling,
                requested,
                source,
            });
        }
        explanation.push(format!(
            "{source} requested '{}' (profile '{profile_name}' ceiling '{}')",
            requested.as_str(),
            ceiling.as_str()
        ));
    } else {
        explanation.push(format!(
            "sandbox omitted; resolved from effective profile '{profile_name}' to '{}' (not the backend default) (#894 S2d)",
            requested.as_str()
        ));
    }

    // Monotonicity is belt-and-braces here: `requested` is already <= ceiling on
    // every path above. Clamping again makes the invariant unconditional rather
    // than a property of the branches.
    let workspace_authority = requested.min(ceiling);

    // 3. Tool authority (narrowing AND of profile and MCP loadout).
    let profile_write_actions = inputs.profile.map(|p| p.write_actions).unwrap_or(true);
    let profile_github_read = inputs.profile.map(|p| p.github_read).unwrap_or(true);
    let tool_authority = ToolAuthority {
        unattended_shell: unattended_shell(
            inputs.backend,
            inputs.permission_profile,
            inputs.allowed_tools,
        ),
        github_read: profile_github_read && inputs.mcp_github_read.unwrap_or(true),
        write_actions: profile_write_actions && inputs.mcp_write_actions.unwrap_or(true),
    };

    // 4. Enforcement: who actually stops a write?
    let enforcement = if has_primitive {
        if inputs.permission_profile == PermissionProfile::Full {
            // The env-gated operator escape hatch (#878-B): codex is launched
            // with --dangerously-bypass-approvals-and-sandbox, so no sandbox
            // enforces anything. Unreachable from a read-only profile — the
            // ceiling check above already rejected that combination.
            Enforcement::Bypass {
                reason: format!(
                    "permission_profile '{}' bypasses the codex sandbox entirely (requires the TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE / TACHI_DISPATCH_VERIFY_HEADLESS operator opt-in, #878-B)",
                    inputs.permission_profile.as_str()
                ),
            }
        } else {
            let entry = qualify_provider(
                inputs.qualifications,
                inputs.backend,
                transport,
                inputs.backend_version,
                workspace_authority,
            )
            .map_err(|reason| ContractError::ProviderNotQualified {
                provider: format!("{provider_label}/{}", transport.as_str()),
                version: inputs.backend_version.map(str::to_string),
                level: workspace_authority,
                reason,
            })?;
            explanation.push(format!(
                "provider '{provider_label}/{}' is certified to enforce '{}' by {}",
                transport.as_str(),
                workspace_authority.as_str(),
                entry.certified_by
            ));
            Enforcement::Enforced {
                provider: format!("{provider_label}/{}", transport.as_str()),
                certified_by: entry.certified_by,
            }
        }
    } else {
        // No primitive at all. An explicit request already failed above; what is
        // left is a profile-DERIVED level on a provider that cannot enforce it.
        // We do not silently claim isolation: the contract is advisory and says
        // so in the receipt.
        let reason = format!(
            "provider '{provider_label}/{}' has no sandbox primitive; workspace authority '{}' is advisory (prompt/permission-level) and is NOT machine-enforced (#894 S2d)",
            transport.as_str(),
            workspace_authority.as_str()
        );
        explanation.push(reason.clone());
        Enforcement::Advisory { reason }
    };

    let network = match (&enforcement, workspace_authority) {
        (Enforcement::Enforced { .. }, WorkspaceAuthority::DangerFullAccess) => {
            NetworkAuthority::ProviderDefault
        }
        (Enforcement::Enforced { .. }, _) => NetworkAuthority::Restricted,
        _ => NetworkAuthority::ProviderDefault,
    };

    // 5. Skills are inputs, not permissions (invariant 6): a skill that needs
    //    workspace writes cannot widen a read-only contract, so it is excluded
    //    from the mount with a reason.
    let mut mounted_skills = Vec::new();
    let mut excluded_skills = Vec::new();
    for skill in inputs.skills {
        if skill.requires_workspace_write && workspace_authority == WorkspaceAuthority::ReadOnly {
            excluded_skills.push(ExcludedSkill {
                skill_id: skill.id.clone(),
                reason: format!(
                    "skill '{}' declares workspace-write intent, but the effective contract is read-only; a mounted skill is an input, not a grant of authority — excluded rather than allowed to widen the contract (#894 S2d)",
                    skill.id
                ),
            });
        } else {
            mounted_skills.push(skill.id.clone());
        }
    }
    if !excluded_skills.is_empty() {
        explanation.push(format!(
            "excluded {} write-intent skill(s) under a read-only contract: {}",
            excluded_skills.len(),
            excluded_skills
                .iter()
                .map(|s| s.skill_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // 6. The vendor flag: only providers with a primitive get one, and it is
    //    always the COMPILED level — never an omitted-argument vendor default.
    let sandbox_arg = if has_primitive && inputs.permission_profile != PermissionProfile::Full {
        Some(workspace_authority.as_codex_sandbox().to_string())
    } else {
        None
    };

    Ok(EffectiveContract {
        workspace_authority,
        network,
        tool_authority,
        enforcement,
        sandbox_arg,
        mounted_skills,
        excluded_skills,
        explanation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::resolve_dispatch_profile;

    fn inputs<'a>(
        backend: &'a str,
        profile: Option<&'a DispatchProfileDef>,
        skills: &'a [SkillRequest],
    ) -> ContractInputs<'a> {
        ContractInputs {
            backend,
            transport: "cli",
            backend_version: None,
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

    /// Discriminating test ①: read-only profile + explicit workspace-write is a
    /// TYPE CONFLICT, not a compatible override.
    #[test]
    fn read_only_profile_plus_explicit_workspace_write_is_a_typed_conflict() {
        let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
        let skills = Vec::new();
        let mut input = inputs("codex", Some(profile), &skills);
        input.requested_sandbox = Some("workspace-write");

        let err = compile_effective_contract(&input)
            .expect_err("widening a read-only profile must be rejected");
        assert_eq!(err.code(), "authority_conflict");
        match &err {
            ContractError::AuthorityConflict {
                profile,
                profile_authority,
                requested,
                ..
            } => {
                assert_eq!(profile, "codex_55_review");
                assert_eq!(*profile_authority, WorkspaceAuthority::ReadOnly);
                assert_eq!(*requested, WorkspaceAuthority::WorkspaceWrite);
            }
            other => panic!("wrong error variant: {other:?}"),
        }
        let text = err.to_string();
        assert!(text.contains("never widened"), "{text}");
    }

    /// Narrowing is always allowed: a write profile may ask for read-only.
    #[test]
    fn narrowing_below_the_profile_ceiling_is_allowed() {
        let profile = resolve_dispatch_profile("glm_impl").expect("profile");
        let skills = Vec::new();
        let mut input = inputs("codex", Some(profile), &skills);
        input.requested_sandbox = Some("read-only");
        let contract = compile_effective_contract(&input).expect("narrowing must be allowed");
        assert_eq!(
            contract.workspace_authority,
            WorkspaceAuthority::ReadOnly,
            "caller narrowed below the profile ceiling"
        );
        assert_eq!(contract.sandbox_arg.as_deref(), Some("read-only"));
    }

    /// Discriminating test ④: an omitted sandbox on a review profile compiles to
    /// read-only — NOT codex's workspace-write default (the #894 S2d gap).
    #[test]
    fn omitted_sandbox_on_review_profile_resolves_to_read_only() {
        let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
        let skills = Vec::new();
        let input = inputs("codex", Some(profile), &skills);

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
    #[test]
    fn omitted_sandbox_on_executor_profile_stays_workspace_write() {
        let profile = resolve_dispatch_profile("glm_impl").expect("profile");
        let skills = Vec::new();
        let input = inputs("codex", Some(profile), &skills);
        let contract = compile_effective_contract(&input).expect("executor contract compiles");
        assert_eq!(
            contract.workspace_authority,
            WorkspaceAuthority::WorkspaceWrite
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
        let input = inputs("codex", Some(profile), &skills);

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
            compile_effective_contract(&inputs("codex", Some(exec_profile), &skills))
                .expect("contract compiles");
        assert_eq!(write_contract.mounted_skills.len(), 3);
        assert!(write_contract.excluded_skills.is_empty());
    }

    /// Discriminating test ③ (pure half): a provider that HAS the vendor flag
    /// but is not kill-test certified must be refused. Vendor flag validation is
    /// not provider qualification (invariant 5).
    #[test]
    fn uncertified_provider_with_a_valid_vendor_flag_is_refused() {
        // The flag itself is valid...
        assert!(validate_codex_sandbox("read-only").is_ok());
        // ...and codex/cli does have a primitive...
        assert!(provider_has_sandbox_primitive("codex", TransportKind::Cli));
        // ...but an empty qualification table certifies nobody.
        let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
        let skills = Vec::new();
        let mut input = inputs("codex", Some(profile), &skills);
        input.qualifications = &[];

        let err = compile_effective_contract(&input)
            .expect_err("an uncertified provider must be refused pre-spawn");
        assert_eq!(err.code(), "provider_not_qualified");
        let text = err.to_string();
        assert!(
            text.contains("not kill-test certified") && text.contains("read-only"),
            "{text}"
        );
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

    /// codex over acpx/native-ACP has no primitive either: the transport, not
    /// the backend name, decides.
    #[test]
    fn codex_over_acp_transport_has_no_primitive_and_is_advisory_when_derived() {
        let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
        let skills = Vec::new();
        let mut input = inputs("codex", Some(profile), &skills);
        input.transport = "acpx";

        // Explicit request → refused, labelled by transport.
        let mut explicit = ContractInputs {
            requested_sandbox: Some("read-only"),
            ..inputs("codex", Some(profile), &skills)
        };
        explicit.transport = "acpx";
        let err = compile_effective_contract(&explicit)
            .unwrap_err()
            .to_string();
        assert!(err.contains("acpx"), "{err}");

        // Derived level → advisory, and NO vendor flag is fabricated.
        let contract = compile_effective_contract(&input).expect("derived contract compiles");
        assert_eq!(contract.workspace_authority, WorkspaceAuthority::ReadOnly);
        assert!(matches!(contract.enforcement, Enforcement::Advisory { .. }));
        assert_eq!(contract.sandbox_arg, None);
        assert_eq!(contract.network, NetworkAuthority::ProviderDefault);
    }

    /// The operator escape hatch (`permission_profile=full|verify`) can never be
    /// used to widen a read-only profile.
    #[test]
    fn full_permission_profile_cannot_widen_a_read_only_profile() {
        let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
        let skills = Vec::new();
        let mut input = inputs("codex", Some(profile), &skills);
        input.permission_profile = PermissionProfile::Full;

        let err = compile_effective_contract(&input)
            .expect_err("a read-only profile must not be bypassable");
        assert_eq!(err.code(), "authority_conflict");

        // On an executor profile the hatch still works (pre-existing #878-B
        // verification lane), and is recorded as a bypass, not as enforcement.
        let exec = resolve_dispatch_profile("glm_impl").expect("profile");
        let mut ok = inputs("codex", Some(exec), &skills);
        ok.permission_profile = PermissionProfile::Full;
        let contract = compile_effective_contract(&ok).expect("executor bypass still allowed");
        assert!(matches!(contract.enforcement, Enforcement::Bypass { .. }));
        assert_eq!(
            contract.sandbox_arg, None,
            "a bypassed launch must not also carry a --sandbox flag"
        );
    }

    #[test]
    fn danger_full_access_request_cannot_exceed_a_profile_ceiling() {
        let profile = resolve_dispatch_profile("glm_impl").expect("profile");
        let skills = Vec::new();
        let mut input = inputs("codex", Some(profile), &skills);
        input.requested_sandbox = Some("danger-full-access");
        let err = compile_effective_contract(&input).expect_err("no profile grants full access");
        assert_eq!(err.code(), "authority_conflict");
    }

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
            ("codex_55_review", WorkspaceAuthority::ReadOnly),
            ("codex_53_fast", WorkspaceAuthority::ReadOnly),
            ("kimi_arch", WorkspaceAuthority::ReadOnly),
            ("kimi_ux", WorkspaceAuthority::ReadOnly),
            ("deepseek_explore", WorkspaceAuthority::ReadOnly),
            ("glm_impl", WorkspaceAuthority::WorkspaceWrite),
            ("opencode_builder", WorkspaceAuthority::WorkspaceWrite),
        ] {
            let profile = resolve_dispatch_profile(name).expect("profile");
            assert_eq!(
                profile_ceiling(Some(profile)),
                expected,
                "profile '{name}' ceiling"
            );
        }
    }

    #[test]
    fn version_scoped_qualification_fails_closed_on_unknown_versions() {
        const TABLE: &[ProviderQualification] = &[ProviderQualification {
            backend: "codex",
            transport: TransportKind::Cli,
            versions: VersionScope::AtLeast("0.50.0"),
            enforces: &[WorkspaceAuthority::ReadOnly],
            certified_by: "synthetic",
        }];

        // Unknown version → refused (an unknown version is not an old-enough one).
        let err = qualify_provider(
            TABLE,
            "codex",
            TransportKind::Cli,
            None,
            WorkspaceAuthority::ReadOnly,
        )
        .expect_err("unknown version must fail closed");
        assert!(err.contains("could not be determined"), "{err}");

        // Too old → refused. Note 0.9.0 < 0.50.0 numerically even though it is
        // greater as a string — the comparison must not be lexicographic.
        assert!(qualify_provider(
            TABLE,
            "codex",
            TransportKind::Cli,
            Some("0.9.0"),
            WorkspaceAuthority::ReadOnly
        )
        .is_err());

        // New enough → certified.
        assert!(qualify_provider(
            TABLE,
            "codex",
            TransportKind::Cli,
            Some("0.50.1"),
            WorkspaceAuthority::ReadOnly
        )
        .is_ok());

        // Certified for read-only only.
        assert!(qualify_provider(
            TABLE,
            "codex",
            TransportKind::Cli,
            Some("0.51.0"),
            WorkspaceAuthority::WorkspaceWrite
        )
        .is_err());
    }

    #[test]
    fn shipped_qualification_table_certifies_only_codex_cli() {
        assert_eq!(PROVIDER_QUALIFICATIONS.len(), 1);
        let entry = &PROVIDER_QUALIFICATIONS[0];
        assert_eq!(entry.backend, "codex");
        assert_eq!(entry.transport, TransportKind::Cli);
        assert!(entry.certified_by.contains("codex_sandbox_kill_test"));
        for backend in ["claude", "grok", "kimi", "custom", "opencode"] {
            assert!(
                qualify_provider(
                    PROVIDER_QUALIFICATIONS,
                    backend,
                    TransportKind::Cli,
                    None,
                    WorkspaceAuthority::ReadOnly
                )
                .is_err(),
                "'{backend}' must not be certified to enforce read-only"
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
}

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
//!    credentials, caller override, *operator bypass* and fallback routing may
//!    only *preserve or narrow* the compiled authority — never widen it. Every
//!    party that has a say files a **claim**; the compiler takes the meet (min)
//!    of the claims against the profile's ceiling, on **every** path.
//! 2. A claim that sits above the running ceiling is a **type conflict**
//!    ([`ContractError::AuthorityConflict`]), not a compatible override. That
//!    covers a read-only profile plus an explicit `workspace-write` request —
//!    and equally the `permission_profile=full|verify` **bypass**, which is a
//!    claim for `danger-full-access` on every backend (codex
//!    `--dangerously-bypass-approvals-and-sandbox`, claude
//!    `--dangerously-skip-permissions`, grok `bypassPermissions`, kimi `-y`) and
//!    is therefore reconciled with the ceiling whether or not the caller also
//!    passed an explicit `sandbox` value.
//! 3. An **omitted** sandbox resolves from the *effective profile* (before the
//!    backend is chosen), not from a backend default — `codex_55_review` with
//!    no `sandbox` argument compiles to `read-only`, where before #894 S2d it
//!    silently fell through to codex's `workspace-write` default.
//! 4. A read-only dispatch that can run **shell unattended**
//!    ([`ToolAuthority::unattended_shell`]) may only start on a **certified**
//!    provider; unknown/uncertified providers are refused **pre-spawn** (no run
//!    directory, no credential materialization), not left to fail after spawn.
//!    This gate runs on every path — explicit request, profile-derived default,
//!    or fallback — not only when the caller typed a sandbox value.
//! 5. **Vendor flag validation is not provider qualification, and a named
//!    kill-test is not an executed one.** `codex --sandbox read-only` being a
//!    *valid flag* ([`crate::validate_codex_sandbox`]) says nothing about
//!    enforcement; neither does a [`ProviderQualification`] row that merely
//!    *points at* a kill-test. Only [`Certification::KillTested`] — a row whose
//!    kill-test actually runs in the suite — certifies anything. Today the codex
//!    kill-test is `#[ignore]`d, so the shipped table certifies **nobody** and
//!    read-only dispatches fail closed. Refusing beats pretending.
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
    /// implied by silence — an advisory read-only lane is NOT isolation. Only
    /// reachable for a level whose enforcement is not load-bearing (a write
    /// level), or for a read-only lane that cannot run shell unattended;
    /// otherwise invariant 4 refuses the dispatch instead.
    Advisory { reason: String },
    /// Operator escape hatch: `permission_profile=full|verify` with the env
    /// opt-in launches the backend with its bypass flag, so nothing enforces
    /// anything (pre-existing #878-B behavior). Only reachable when the
    /// dispatch's ceiling *is* `danger-full-access` — i.e. a profile-less
    /// dispatch that also asked for no narrower sandbox. Any profile (review or
    /// executor) or any explicit narrower `sandbox` value caps the ceiling below
    /// `danger-full-access`, and the bypass claim is then an
    /// [`ContractError::AuthorityConflict`].
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
    /// A claim tried to *widen* the running ceiling. Invariant 1/2.
    AuthorityConflict {
        profile: String,
        /// The ceiling the claim ran into. It starts at the profile's grant and
        /// is narrowed further by any explicit caller `sandbox` — so a later
        /// claim (the operator bypass) is checked against the *narrowed* value,
        /// not just against the profile.
        ceiling: WorkspaceAuthority,
        /// Which input set that ceiling ([`CEILING_PROFILE`] / [`CALLER_SANDBOX`]).
        ceiling_source: &'static str,
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
                ceiling,
                ceiling_source,
                requested,
                source,
            } => write!(
                f,
                "authority conflict: profile '{profile}' dispatch has an effective workspace-authority ceiling of '{}' (from {ceiling_source}), but {source} asks for '{}'; authority may only be preserved or narrowed, never widened — this is a type conflict, not a compatible override (fail-closed, #894 S2d)",
                ceiling.as_str(),
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
                "permission receipt: provider '{provider}'{} is not kill-test certified to enforce workspace authority '{}' ({reason}); a valid vendor flag is not provider qualification, and a shell-capable read-only dispatch may only start on a certified provider — refusing before spawn rather than pretending to isolate (fail-closed, #894 S2d)",
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

/// Why we believe — or explicitly do not believe — that a row enforces its
/// levels. Certification is a statement about an **execution**, not about a
/// flag and not about a *reference* to a test (invariant 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certification {
    /// A real-process kill-test ran and observed this provider refuse every
    /// mutation in its matrix. `test` is a repo-relative path, and it must be a
    /// test the ordinary suite actually executes — see
    /// `certification_is_coupled_to_the_kill_tests_execution_state`, which fails
    /// if a `KillTested` row points at an `#[ignore]`d test.
    KillTested { test: &'static str },
    /// The row exists (the provider has the flag, and we know which levels a
    /// kill-test *would* cover) but nobody has watched it refuse anything.
    /// Fails closed: `qualify_provider` never returns an `Unverified` row, so a
    /// shell-capable read-only dispatch to it is refused pre-spawn.
    Unverified { reason: &'static str },
}

impl Certification {
    /// The certifying kill-test, or `None` when the row is not certified.
    pub fn kill_test(self) -> Option<&'static str> {
        match self {
            Self::KillTested { test } => Some(test),
            Self::Unverified { .. } => None,
        }
    }
}

/// One row of the qualification table: `backend x transport x version`, the
/// levels its kill-test matrix covers, and whether that kill-test has actually
/// been executed.
#[derive(Debug, Clone, Copy)]
pub struct ProviderQualification {
    pub backend: &'static str,
    pub transport: TransportKind,
    pub versions: VersionScope,
    /// The levels this row's kill-test matrix covers. A level that is not listed
    /// is never certified — and the levels that *are* listed only count when
    /// `certification` is [`Certification::KillTested`].
    pub covers: &'static [WorkspaceAuthority],
    pub certification: Certification,
}

/// The qualification table. **codex CLI is the only entry** — it is the only
/// backend Tachi dispatches that ships a real sandbox primitive (owner-ratified,
/// sol codex-e0255) — and that entry is **`Unverified`**: its kill-test
/// (`crates/tachi-dispatch/tests/codex_sandbox_kill_test.rs`) is `#[ignore]`d
/// and has never been executed against a real `codex` binary.
///
/// So today this table certifies **nobody**, and every shell-capable read-only
/// dispatch — codex included — is refused pre-spawn with a receipt saying the
/// provider is not kill-test certified. That is the intended fail-closed posture
/// (owner-frozen invariant 5: *refusing beats pretending*), and it is the
/// forcing function for certification.
///
/// **To certify codex/cli** (the only way to make read-only lanes dispatchable
/// again):
///
/// 1. run the kill-test against a real binary —
///    `cargo test -p tachi-dispatch --test codex_sandbox_kill_test -- --ignored --nocapture`;
/// 2. if (and only if) every mutation in the matrix was refused, drop the
///    `#[ignore]` from the test so the ordinary suite keeps re-certifying it, and
/// 3. flip this row to
///    `Certification::KillTested { test: "crates/tachi-dispatch/tests/codex_sandbox_kill_test.rs" }`.
///
/// Doing (3) without (2) is a lie the unit test
/// `certification_is_coupled_to_the_kill_tests_execution_state` refuses to let
/// you tell.
pub const PROVIDER_QUALIFICATIONS: &[ProviderQualification] = &[ProviderQualification {
    backend: "codex",
    transport: TransportKind::Cli,
    versions: VersionScope::Any,
    covers: &[
        WorkspaceAuthority::ReadOnly,
        WorkspaceAuthority::WorkspaceWrite,
    ],
    certification: Certification::Unverified {
        reason: "its kill-test (crates/tachi-dispatch/tests/codex_sandbox_kill_test.rs) is #[ignore]d and has never been executed against a real codex binary — nobody has yet watched this provider refuse a single write",
    },
}];

/// Parse a dotted version into comparable numeric components. String ordering
/// is NOT version ordering ("0.9" > "0.10" lexically) — this is compared
/// numerically on purpose.
fn version_components(raw: &str) -> Option<Vec<u64>> {
    let cleaned = raw.trim().trim_start_matches('v');
    let head = cleaned.split(['-', '+', ' ']).next()?;
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
///
/// Never returns a [`Certification::Unverified`] row: an uncertified row is a
/// row nobody has watched enforce anything, which is the same thing as no row at
/// all — only with a better receipt.
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
        if !entry.covers.contains(&level) {
            last_reason = format!(
                "the kill-test matrix for this provider covers [{}], not '{}'",
                entry
                    .covers
                    .iter()
                    .map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                level.as_str()
            );
            continue;
        }
        // Invariant 5: the row existing is not the row being certified.
        if let Certification::Unverified { reason } = entry.certification {
            last_reason = format!(
                "a qualification row exists for '{backend}/{}' and its matrix covers '{}', but the row is NOT certified: {reason}",
                transport.as_str(),
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

/// Ceiling/claim sources, named once so the receipt wording and the tests cannot
/// drift apart.
pub const CEILING_PROFILE: &str = "the profile's declared authority";
pub const CALLER_SANDBOX: &str = "the caller's explicit sandbox request";
pub const OPERATOR_BYPASS: &str = "the permission_profile 'full'/'verify' sandbox bypass";

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

    // 2. Compile the workspace authority. Every party that has a say files a
    //    CLAIM against a running CEILING that starts at the profile's grant and
    //    only ever narrows; a claim above the ceiling is a typed conflict, and
    //    the compiled level is the meet (min) of the claims. Ordering is
    //    ceiling-first, claims-second, min-last — deliberately, so no claim can
    //    reach the launcher without having been reconciled with the ceiling.
    //
    //    Round 1 shipped an escalation here: the `permission_profile=full` →
    //    danger-full-access promotion lived inside the `requested_sandbox: None`
    //    arm, so an explicit `sandbox` value SHADOWED it and the bypass was never
    //    checked against the ceiling at all. `codex_55_review` (read-only) +
    //    `sandbox=read-only` + `permission_profile=full` compiled to a contract
    //    that *said* read-only while launching codex with
    //    `--dangerously-bypass-approvals-and-sandbox`. The bypass is now a claim
    //    like any other, checked on EVERY path, and it is not gated on
    //    `has_primitive` either: `full` bypasses permissions on every backend
    //    (claude `--dangerously-skip-permissions`, grok `bypassPermissions`,
    //    kimi `-y`), not just on the one with a sandbox flag.
    let bypass_claimed = inputs.permission_profile == PermissionProfile::Full;
    let mut ceiling = ceiling;
    let mut ceiling_source: &'static str = CEILING_PROFILE;
    let mut claimed: Option<WorkspaceAuthority> = None;

    if let Some(raw) = inputs.requested_sandbox {
        let level = WorkspaceAuthority::parse_request(raw)?;
        if level > ceiling {
            return Err(ContractError::AuthorityConflict {
                profile: profile_name,
                ceiling,
                ceiling_source,
                requested: level,
                source: CALLER_SANDBOX,
            });
        }
        explanation.push(format!(
            "{CALLER_SANDBOX} claims '{}' (profile '{profile_name}' ceiling '{}')",
            level.as_str(),
            ceiling.as_str()
        ));
        // A caller who asks for LESS authority has narrowed the contract: that
        // request is now the ceiling for every later claim (the operator bypass
        // included). Nothing downstream may hand back what the caller declined.
        ceiling = level;
        ceiling_source = CALLER_SANDBOX;
        claimed = Some(level);
    }

    if bypass_claimed {
        // The bypass launches the backend with its "skip every check" flag: the
        // authority it claims is danger-full-access, whatever else was asked for.
        let level = WorkspaceAuthority::DangerFullAccess;
        if level > ceiling {
            return Err(ContractError::AuthorityConflict {
                profile: profile_name,
                ceiling,
                ceiling_source,
                requested: level,
                source: OPERATOR_BYPASS,
            });
        }
        explanation.push(format!(
            "{OPERATOR_BYPASS} claims '{}', which the ceiling '{}' (from {ceiling_source}) permits",
            level.as_str(),
            ceiling.as_str()
        ));
        claimed = Some(level);
    }

    let workspace_authority = match claimed {
        // Belt-and-braces: every claim above is already <= ceiling. The min makes
        // monotonicity unconditional rather than a property of the branches.
        Some(level) => level.min(ceiling),
        None => {
            let level = default.min(ceiling);
            explanation.push(format!(
                "sandbox omitted; resolved from effective profile '{profile_name}' to '{}' (not the backend default) (#894 S2d)",
                level.as_str()
            ));
            level
        }
    };

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

    // 4. Enforcement: who actually stops a write? The qualification gate runs on
    //    EVERY path that reaches here — explicit request, operator bypass, or a
    //    profile-derived default. Round 1 only ran it for providers with a
    //    sandbox primitive, so an omitted `sandbox` that compiled to read-only on
    //    an uncertified provider sailed through as "advisory" and invariant 4 was
    //    decorative.
    let provider_id = format!("{provider_label}/{}", transport.as_str());
    let enforcement = if bypass_claimed {
        // The env-gated operator escape hatch (#878-B): the backend is launched
        // with its bypass flag, so nothing enforces anything. Only reachable when
        // the ceiling *is* danger-full-access — the claim check above rejected
        // every narrower ceiling.
        Enforcement::Bypass {
            reason: format!(
                "permission_profile '{}' bypasses the backend's own permission/sandbox enforcement entirely (requires the TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE / TACHI_DISPATCH_VERIFY_HEADLESS operator opt-in, #878-B); nothing machine-enforces workspace authority '{}'",
                inputs.permission_profile.as_str(),
                workspace_authority.as_str()
            ),
        }
    } else {
        match qualify_provider(
            inputs.qualifications,
            inputs.backend,
            transport,
            inputs.backend_version,
            workspace_authority,
        ) {
            Ok(entry) => {
                let certified_by = entry
                    .certification
                    .kill_test()
                    .expect("qualify_provider never returns an uncertified row");
                explanation.push(format!(
                    "provider '{provider_id}' is kill-test certified to enforce '{}' by {certified_by}",
                    workspace_authority.as_str()
                ));
                Enforcement::Enforced {
                    provider: provider_id.clone(),
                    certified_by,
                }
            }
            Err(reason) => {
                // Invariant 4: read-only is the one level whose entire value IS
                // its enforcement, and an agent that can run shell unattended is
                // the one that will actually test it. That combination on an
                // uncertified provider is refused pre-spawn — no run directory,
                // no credentials, no pretending.
                if workspace_authority == WorkspaceAuthority::ReadOnly
                    && tool_authority.unattended_shell
                {
                    return Err(ContractError::ProviderNotQualified {
                        provider: provider_id,
                        version: inputs.backend_version.map(str::to_string),
                        level: workspace_authority,
                        reason,
                    });
                }
                // Everything else (a write level, or a read-only lane that cannot
                // run shell unattended) is allowed to proceed — but we do not
                // silently claim isolation we do not have: the contract is
                // advisory and says so in the receipt.
                let reason = format!(
                    "provider '{provider_id}' is not kill-test certified to enforce workspace authority '{}' ({reason}); the contract is advisory (prompt/permission-level) and is NOT machine-enforced{} (#894 S2d)",
                    workspace_authority.as_str(),
                    if has_primitive {
                        " — the vendor sandbox flag is still passed, as defense in depth, but nobody has watched it hold"
                    } else {
                        " (this provider has no sandbox primitive at all)"
                    }
                );
                explanation.push(reason.clone());
                Enforcement::Advisory { reason }
            }
        }
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
    //    always the COMPILED level — never an omitted-argument vendor default. A
    //    bypassed launch gets none (the launcher passes the bypass flag instead;
    //    passing both would be incoherent).
    let sandbox_arg = if has_primitive && !bypass_claimed {
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

    /// The kill-test's own source, read at compile time. The certification
    /// ratchet (`certification_is_coupled_to_the_kill_tests_execution_state`)
    /// reads the `#[ignore]` attribute out of it, so "is this provider certified"
    /// is answered by the test's *execution state*, not by a `&'static str` that
    /// merely points at it.
    const KILL_TEST_SOURCE: &str = include_str!("../tests/codex_sandbox_kill_test.rs");
    const KILL_TEST_PATH: &str = "crates/tachi-dispatch/tests/codex_sandbox_kill_test.rs";

    /// A synthetic table that certifies codex/cli — i.e. exactly what
    /// [`PROVIDER_QUALIFICATIONS`] becomes on the day somebody actually runs the
    /// kill-test. The shipped table is `Unverified`, so every read-only case
    /// below has to declare which world it is testing: `CERTIFIED_CODEX` is the
    /// post-certification world, `PROVIDER_QUALIFICATIONS` is today's
    /// fail-closed reality.
    const CERTIFIED_CODEX: &[ProviderQualification] = &[ProviderQualification {
        backend: "codex",
        transport: TransportKind::Cli,
        versions: VersionScope::Any,
        covers: &[
            WorkspaceAuthority::ReadOnly,
            WorkspaceAuthority::WorkspaceWrite,
        ],
        certification: Certification::KillTested {
            test: KILL_TEST_PATH,
        },
    }];

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

    /// Same, but in the post-certification world: codex/cli is kill-tested, so
    /// read-only lanes compile instead of failing closed.
    fn certified_inputs<'a>(
        backend: &'a str,
        profile: Option<&'a DispatchProfileDef>,
        skills: &'a [SkillRequest],
    ) -> ContractInputs<'a> {
        ContractInputs {
            qualifications: CERTIFIED_CODEX,
            ..inputs(backend, profile, skills)
        }
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
    /// ceiling check entirely: `codex_55_review` (read-only) + `sandbox=read-only`
    /// + `permission_profile=full` compiled to `Ok(Enforcement::Bypass)` — a
    /// contract that *said* read-only while launching codex with
    /// `--dangerously-bypass-approvals-and-sandbox`. Leg 2 below is that exact
    /// escalation; it must be an `authority_conflict`, and under no ceiling may a
    /// bypass ever come back as `Bypass`.
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
    /// read-only — NOT codex's workspace-write default (the #894 S2d gap).
    /// Stated in the post-certification world, because today the same dispatch is
    /// refused outright (see
    /// `read_only_lane_is_refused_while_the_shipped_table_certifies_nobody`).
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
    /// the fix narrows review lanes, it does not break implementers. It is also
    /// NOT refused by the uncertified shipped table: a write level makes no
    /// isolation claim worth certifying, so it compiles as advisory.
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
        assert_eq!(contract.sandbox_arg.as_deref(), Some("workspace-write"));
        assert!(
            matches!(contract.enforcement, Enforcement::Advisory { .. }),
            "the shipped table certifies nobody, so nothing is claimed to be enforced: {:?}",
            contract.enforcement
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
        let err = compile_effective_contract(&shell_capable).expect_err(
            "a read-only lane that can run shell unattended needs a certified enforcer",
        );
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

    /// Discriminating test ③: a provider that HAS the vendor flag but is not
    /// kill-test certified must be refused — whether the table has no row for it
    /// at all, or has a row that merely *names* a kill-test nobody ran. Vendor
    /// flag validation is not provider qualification, and a named test is not an
    /// executed one.
    #[test]
    fn a_valid_vendor_flag_is_not_a_certification() {
        // The flag itself is valid...
        assert!(validate_codex_sandbox("read-only").is_ok());
        // ...and codex/cli does have a primitive...
        assert!(provider_has_sandbox_primitive("codex", TransportKind::Cli));

        let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
        let skills = Vec::new();
        for (world, table) in [
            (
                "an empty table certifies nobody",
                &[] as &[ProviderQualification],
            ),
            (
                "the SHIPPED table's codex row is Unverified — its kill-test has never run",
                PROVIDER_QUALIFICATIONS,
            ),
        ] {
            let mut input = inputs("codex", Some(profile), &skills);
            input.qualifications = table;

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
        }
    }

    /// Today's reality, asserted so nobody has to guess: with the shipped table
    /// the review lane does not run at all. Refusing beats pretending — and this
    /// is the forcing function for actually running the kill-test.
    #[test]
    fn read_only_lane_is_refused_while_the_shipped_table_certifies_nobody() {
        let profile = resolve_dispatch_profile("codex_55_review").expect("profile");
        let skills = Vec::new();
        let err = compile_effective_contract(&inputs("codex", Some(profile), &skills))
            .expect_err("codex/cli is Unverified today");
        assert_eq!(err.code(), "provider_not_qualified");
        assert!(
            err.to_string().contains("has never been executed"),
            "the receipt must say WHY the provider is uncertified: {err}"
        );
    }

    /// The ratchet: a `KillTested` row may only name a kill-test the ordinary
    /// suite actually executes. Flip a row to `KillTested` while its test is
    /// still `#[ignore]`d — i.e. claim a certification nobody ran — and this
    /// fails. Un-ignore the test and it passes, because then the suite itself is
    /// the certification.
    #[test]
    fn certification_is_coupled_to_the_kill_tests_execution_state() {
        fn is_ignored(source: &str) -> bool {
            source
                .lines()
                .any(|line| line.trim_start().starts_with("#[ignore"))
        }

        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");

        for entry in PROVIDER_QUALIFICATIONS {
            let Certification::KillTested { test } = entry.certification else {
                continue;
            };
            let path = repo_root.join(test);
            assert!(
                path.exists(),
                "'{}/{}' claims certification by '{test}', which does not exist",
                entry.backend,
                entry.transport.as_str()
            );
            let source = std::fs::read_to_string(&path).expect("kill-test source");
            assert!(
                !is_ignored(&source),
                "'{}/{}' is marked KillTested, but '{test}' is #[ignore]d — it has never run, so the certification is a claim, not evidence (invariant 5)",
                entry.backend,
                entry.transport.as_str()
            );
        }

        // And the state of the world today: the codex kill-test IS ignored, so
        // NOTHING in the shipped table may be certified.
        if is_ignored(KILL_TEST_SOURCE) {
            assert!(
                PROVIDER_QUALIFICATIONS
                    .iter()
                    .all(|entry| entry.certification.kill_test().is_none()),
                "the codex kill-test is #[ignore]d; no shipped row may claim KillTested"
            );
        }
    }

    #[test]
    fn shipped_qualification_table_has_one_uncertified_codex_row() {
        assert_eq!(PROVIDER_QUALIFICATIONS.len(), 1);
        let entry = &PROVIDER_QUALIFICATIONS[0];
        assert_eq!(entry.backend, "codex");
        assert_eq!(entry.transport, TransportKind::Cli);
        assert!(
            matches!(entry.certification, Certification::Unverified { .. }),
            "the kill-test has never run: the row must not claim certification"
        );
        assert_eq!(entry.certification.kill_test(), None);

        // Nobody — codex included — qualifies out of the shipped table.
        for backend in ["codex", "claude", "grok", "kimi", "custom", "opencode"] {
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
    fn version_scoped_qualification_fails_closed_on_unknown_versions() {
        const TABLE: &[ProviderQualification] = &[ProviderQualification {
            backend: "codex",
            transport: TransportKind::Cli,
            versions: VersionScope::AtLeast("0.50.0"),
            covers: &[WorkspaceAuthority::ReadOnly],
            certification: Certification::KillTested { test: "synthetic" },
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

        // Covered for read-only only.
        assert!(qualify_provider(
            TABLE,
            "codex",
            TransportKind::Cli,
            Some("0.51.0"),
            WorkspaceAuthority::WorkspaceWrite
        )
        .is_err());
    }

    /// An `Unverified` row is never handed back, no matter how well it matches:
    /// same backend, same transport, same version scope, level covered — and
    /// still refused, because nobody has watched it enforce anything.
    #[test]
    fn an_unverified_row_never_qualifies() {
        const TABLE: &[ProviderQualification] = &[ProviderQualification {
            backend: "codex",
            transport: TransportKind::Cli,
            versions: VersionScope::Any,
            covers: &[WorkspaceAuthority::ReadOnly],
            certification: Certification::Unverified {
                reason: "synthetic: the kill-test was never executed",
            },
        }];
        let err = qualify_provider(
            TABLE,
            "codex",
            TransportKind::Cli,
            Some("9.9.9"),
            WorkspaceAuthority::ReadOnly,
        )
        .expect_err("an uncertified row must not qualify");
        assert!(err.contains("NOT certified"), "{err}");
        assert!(err.contains("never executed"), "{err}");
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

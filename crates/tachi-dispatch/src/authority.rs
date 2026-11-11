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
//! 4. A shell-capable read-only dispatch requires a vendor sandbox. For Codex
//!    CLI, the installed interface is probed before spawn; a CLI upgrade does
//!    not require editing a version allowlist. Other transports cannot inherit
//!    the CLI capability.
//! 5. Capability admission and historical certification are distinct. Only an
//!    exact matching executed receipt may claim `Enforced`; an installed CLI
//!    advertising the required sandbox interface is recorded as `VendorSandbox`.
//!    A known failed receipt for the installed version vetoes that fallback.
//! 6. **A mounted skill is an input, not a permission.** A skill that declares
//!    workspace-write intent cannot widen a read-only contract — it is
//!    *excluded* from the mount, with a reason in the receipt.

use crate::certification::{probe_backend_version, CertificationReceipt, CODEX_CLI_RECEIPT};
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
    /// A kill-test-certified vendor sandbox enforces this level. The receipt is
    /// named in full — id, the vendor version it was issued for, and the
    /// kill-test that produced it — so a reader of a dispatch receipt can go and
    /// check the evidence instead of taking "enforced" on faith.
    Enforced {
        provider: String,
        certified_by: &'static str,
        receipt: &'static str,
        vendor_version: &'static str,
    },
    /// The installed CLI advertises the sandbox interface used by the launcher.
    /// This records vendor enforcement without claiming an executed kill-test.
    VendorSandbox {
        provider: String,
        vendor_version: String,
    },
    /// Tachi installs a host filesystem boundary before the adapter starts.
    ServerSandbox { provider: String },
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

/// Why we believe — or explicitly do not believe — that a row enforces its
/// levels. Certification is a statement about an **execution**, not about a
/// flag and not about a *reference* to a test (invariant 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certification {
    /// A real-process kill-test ran, against a real vendor binary, and observed
    /// this provider refuse every mutation in its matrix — and the run was
    /// written down. The [`CertificationReceipt`] carries what was actually
    /// exercised: which binary version, which OS, which levels, which mutations.
    /// `qualify_provider` re-checks all of that against the dispatch at hand, so
    /// this variant is not a claim, it is a citation.
    KillTested {
        receipt: &'static CertificationReceipt,
    },
    /// Nobody has watched this provider refuse anything. Fails closed:
    /// `qualify_provider` never returns an `Unverified` row, so a shell-capable
    /// read-only dispatch to it is refused pre-spawn.
    Unverified { reason: &'static str },
}

impl Certification {
    /// The receipt backing this row, or `None` when nothing does.
    pub fn receipt(self) -> Option<&'static CertificationReceipt> {
        match self {
            Self::KillTested { receipt } => Some(receipt),
            Self::Unverified { .. } => None,
        }
    }

    /// The certifying kill-test, or `None` when the row is not certified.
    pub fn kill_test(self) -> Option<&'static str> {
        self.receipt().map(|receipt| receipt.kill_test)
    }
}

/// One row of the qualification table: which `backend x transport` has been
/// proven to enforce what — and by which executed kill-test. There is
/// deliberately no `versions` / `covers` field here: the scope of a
/// certification is a property of the *execution that happened*, so it is read
/// off the receipt and cannot drift away from it.
#[derive(Debug, Clone, Copy)]
pub struct ProviderQualification {
    pub backend: &'static str,
    pub transport: TransportKind,
    pub certification: Certification,
}

/// The qualification table. **codex CLI is the only entry** — it is the only
/// backend Tachi dispatches that ships a real sandbox primitive (owner-ratified,
/// sol codex-e0255) — and it is certified by an executed kill-test:
/// [`CODEX_CLI_RECEIPT`] / `certifications/codex-cli.toml`.
///
/// What that receipt does and does not buy, precisely:
///
/// * **codex-cli 0.144.1, macOS, `read-only`, over the ten-mutation matrix** —
///   certified. `qualify_provider` returns this row, the contract compiles to
///   `Enforcement::Enforced`, and the review/explore lanes run.
/// * Other versions/OS families cannot inherit this historical certification.
///   Current Codex CLI capability is admitted separately on macOS/Linux.
/// * **`workspace-write`** — not certified. The kill-test observed nothing about
///   what that level contains, so a codex workspace-write contract stays
///   *advisory* and says so in its receipt.
/// * **Every other backend/transport** (claude, grok, kimi, custom/opencode,
///   codex-over-acpx) — no sandbox primitive, no row, no certification: a
///   shell-capable read-only dispatch to any of them is refused pre-spawn.
///
/// Historical receipts remain exact-version evidence. Compatible CLI upgrades
/// use the separately probed vendor sandbox capability, not these old receipts.
pub const PROVIDER_QUALIFICATIONS: &[ProviderQualification] = &[ProviderQualification {
    backend: "codex",
    transport: TransportKind::Cli,
    certification: Certification::KillTested {
        receipt: &CODEX_CLI_RECEIPT,
    },
}];

/// The installed version of the vendor binary this dispatch would spawn, for the
/// pre-spawn certification gate. `None` — including "this provider has no sandbox
/// primitive, so there is nothing a version could certify" — fails closed at
/// [`qualify_provider`].
///
/// Only providers with a primitive are probed: spawning `claude --version` to
/// decide whether an *absent* sandbox is enforced would be pure cost.
pub fn probe_provider_version(backend: &str, transport: TransportKind) -> Option<String> {
    if !provider_has_sandbox_primitive(backend, transport) {
        return None;
    }
    probe_backend_version(backend)
}

/// Parse a dotted version into comparable numeric components. String ordering
/// is NOT version ordering ("0.9" > "0.10" lexically) — this is compared
/// numerically on purpose.
pub(crate) fn version_components(raw: &str) -> Option<Vec<u64>> {
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

/// Look up whether `backend x transport x version x host OS` is certified to enforce
/// `level`. `table` is a parameter (not the const) so the qualification policy
/// itself is testable against synthetic tables.
///
/// Every gate here is a question about an execution that happened:
///
/// 1. is there a row at all for this `backend x transport`?
/// 2. does it carry a **receipt** — did anyone ever watch it refuse a write? (an
///    [`Certification::Unverified`] row is never returned: a row nobody has
///    watched enforce anything is the same thing as no row at all, only with a
///    better error message);
/// 3. did that run **pass**, and does the receipt actually attest *this*
///    provider?
/// 4. did it exercise **this level**? (codex's receipt covers `read-only` only —
///    `workspace-write` containment was never probed, so it is never certified);
/// 5. does it attest the **installed binary**? `version` is what
///    [`probe_provider_version`] read out of `--version` moments ago. A version
///    the receipt does not name — including `None`, "we could not tell" — fails
///    closed. Provider conformance does not carry across vendor versions.
pub fn qualify_provider<'a>(
    table: &'a [ProviderQualification],
    backend: &str,
    transport: TransportKind,
    version: Option<&str>,
    host_os: &str,
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
        // (2) Invariant 5: the row existing is not the row being certified.
        let receipt = match entry.certification {
            Certification::KillTested { receipt } => receipt,
            Certification::Unverified { reason } => {
                last_reason = format!(
                    "a qualification row exists for '{backend}/{}', but the row is NOT certified: {reason}",
                    transport.as_str(),
                );
                continue;
            }
        };

        // (3) A receipt that failed, or that attests some other provider, is not
        //     evidence about this one.
        if !receipt.passed() {
            last_reason = format!(
                "the certification receipt '{}' for '{backend}/{}' records a FAILED kill-test run ({} on {} {}) — the provider was watched, and it did not hold",
                receipt.id,
                transport.as_str(),
                receipt.vendor_binary,
                receipt.host_os,
                receipt.executed_at
            );
            continue;
        }
        if receipt.backend != entry.backend || receipt.transport != entry.transport {
            last_reason = format!(
                "the receipt '{}' attests '{}/{}', not '{backend}/{}' — a certification is not transferable",
                receipt.id,
                receipt.backend,
                receipt.transport.as_str(),
                transport.as_str()
            );
            continue;
        }

        if receipt.host_os != host_os {
            last_reason = format!(
                "the certification receipt '{}' was executed on {}, not the current host OS {host_os}; sandbox evidence is not transferable across operating systems",
                receipt.id, receipt.host_os
            );
            continue;
        }

        // (4) Only the levels the run actually exercised.
        if !receipt.certifies_level(level) {
            last_reason = format!(
                "the executed kill-test ('{}') certifies [{}] on this provider, not '{}' — no run has observed it contain a '{}' contract",
                receipt.id,
                receipt
                    .covers
                    .iter()
                    .map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                level.as_str(),
                level.as_str()
            );
            continue;
        }

        // (5) Only the binary the run actually exercised.
        if !receipt.certifies_version(version) {
            last_reason = match version {
                Some(actual) => format!(
                    "the certification receipt '{}' was issued for {} {} on {}, but the installed binary reports '{actual}'; provider conformance does not carry across vendor versions — re-run the kill-test against {actual} and issue a new receipt ({})",
                    receipt.id,
                    receipt.vendor_binary,
                    receipt.vendor_version,
                    receipt.host_os,
                    receipt.source_file
                ),
                None => format!(
                    "the certification receipt '{}' is scoped to {} {}, and the installed binary's version could not be determined (not on PATH, or `--version` failed); an unknown version is not a certified version",
                    receipt.id, receipt.vendor_binary, receipt.vendor_version
                ),
            };
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
    /// Host-owned result of probing `codex exec --help`; never request input.
    pub codex_read_only_supported: bool,
    /// Result of the server-owned native confinement probe, never request input.
    pub native_sandbox_supported: bool,
    /// Actual execution host, supplied by the server rather than request input.
    pub host_os: &'a str,
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
    if matches!(backend, "codex" | "custom" | "opencode" | "dsh") {
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
    let native = inputs
        .profile
        .and_then(|profile| crate::native_session_profile(profile.name));
    if native.is_some_and(|profile| {
        profile.backend != inputs.backend || transport != TransportKind::AcpNative
    }) {
        return Err(ContractError::UnsupportedSandbox(
            "native Session profile requires its fixed backend and acp_native transport".into(),
        ));
    }
    let native_enforced = native.is_some()
        && inputs.native_sandbox_supported
        && matches!(inputs.host_os, "linux" | "macos");
    if native.is_some() && !native_enforced {
        return Err(ContractError::ProviderNotQualified {
            provider: "native-session/server-sandbox".into(),
            version: None,
            level: profile_ceiling(inputs.profile),
            reason: "server filesystem confinement is unavailable on this host".into(),
        });
    }
    let has_primitive =
        native_enforced || provider_has_sandbox_primitive(inputs.backend, transport);
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
    } else if native_enforced {
        explanation.push("server-owned filesystem boundary confines writes to private runtime and the admitted managed workspace; network and host read access are not isolated".into());
        Enforcement::ServerSandbox {
            provider: match inputs.host_os {
                "linux" => "bubblewrap",
                _ => "sandbox-exec",
            }
            .into(),
        }
    } else {
        match qualify_provider(
            inputs.qualifications,
            inputs.backend,
            transport,
            inputs.backend_version,
            inputs.host_os,
            workspace_authority,
        ) {
            Ok(entry) => {
                let receipt = entry
                    .certification
                    .receipt()
                    .expect("qualify_provider never returns an uncertified row");
                explanation.push(format!(
                    "provider '{provider_id}' is kill-test certified to enforce '{}' by receipt '{}' ({} {}, {}, executed {} — {})",
                    workspace_authority.as_str(),
                    receipt.id,
                    receipt.vendor_binary,
                    receipt.vendor_version,
                    receipt.host_os,
                    receipt.executed_at,
                    receipt.kill_test
                ));
                Enforcement::Enforced {
                    provider: provider_id.clone(),
                    certified_by: receipt.kill_test,
                    receipt: receipt.id,
                    vendor_version: receipt.vendor_version,
                }
            }
            Err(reason)
                if inputs.codex_read_only_supported
                    && inputs.backend == "codex"
                    && transport == TransportKind::Cli
                    && matches!(inputs.host_os, "macos" | "linux")
                    && workspace_authority == WorkspaceAuthority::ReadOnly
                    && inputs.backend_version.is_some_and(|v| !v.trim().is_empty())
                    && !inputs.qualifications.iter().any(|entry| {
                        entry.certification.receipt().is_some_and(|receipt| {
                            receipt.backend == inputs.backend
                                && receipt.transport == transport
                                && receipt.host_os == inputs.host_os
                                && receipt.certifies_version(inputs.backend_version)
                                && receipt.certifies_level(workspace_authority)
                                && !receipt.passed()
                        })
                    }) =>
            {
                explanation.push(format!(
                    "installed Codex CLI supports --sandbox read-only; using vendor sandbox without claiming historical certification ({reason})"
                ));
                Enforcement::VendorSandbox {
                    provider: provider_id.clone(),
                    vendor_version: inputs.backend_version.unwrap().to_string(),
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
mod tests;

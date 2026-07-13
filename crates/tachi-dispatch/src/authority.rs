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
//!    *points at* a kill-test. A row certifies a level **iff** it carries a
//!    [`CertificationReceipt`]: a passing, checked-in record of a real kill-test
//!    execution against a real vendor binary, and one whose `vendor_version`
//!    matches the binary that is **actually installed** ([`probe_backend_version`],
//!    run pre-spawn). Codex CLI 0.144.1 on macOS is certified for `read-only` by
//!    `certifications/codex-cli.toml`; every other version, every other level,
//!    and every other provider is not — and fails closed. Refusing beats
//!    pretending.
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
/// * **Any other codex version** — refused. The installed binary's `--version`
///   is probed pre-spawn ([`probe_provider_version`]); a mismatch, or a version
///   we cannot determine, fails closed. Vendor conformance does not carry across
///   versions; an unknown version is not a certified version.
/// * **`workspace-write`** — not certified. The kill-test observed nothing about
///   what that level contains, so a codex workspace-write contract stays
///   *advisory* and says so in its receipt.
/// * **Every other backend/transport** (claude, grok, kimi, custom/opencode,
///   codex-over-acpx) — no sandbox primitive, no row, no certification: a
///   shell-capable read-only dispatch to any of them is refused pre-spawn.
///
/// **To certify a new codex version** (the only way read-only lanes survive a
/// codex upgrade):
///
/// 1. `cargo test -p tachi-dispatch --test codex_sandbox_kill_test -- --ignored --nocapture`
///    against the new binary — it mints a receipt block on the way out;
/// 2. if (and only if) every mutation was refused, paste that block into
///    `crates/tachi-dispatch/certifications/codex-cli.toml` and update
///    [`CODEX_CLI_RECEIPT`] to match (`receipt_const_matches_the_checked_in_receipt_file`
///    fails the build if you update one and not the other).
///
/// Skipping (1) and just editing the version string is the one thing this design
/// exists to make hard: the receipt records the *blob* of the test that ran, who
/// ran it, when, and for how long.
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

/// Look up whether `backend x transport x version` is certified to enforce
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
    use crate::certification::CertificationResult;
    use crate::profiles::resolve_dispatch_profile;

    /// The kill-test's own source, read at compile time — the ratchet
    /// (`every_certified_row_is_backed_by_a_passing_executed_receipt`) checks
    /// that the receipt's kill-test really exists and really contains the
    /// function the receipt says was run.
    const KILL_TEST_SOURCE: &str = include_str!("../tests/codex_sandbox_kill_test.rs");

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
        let contract =
            compile_effective_contract(&certified_inputs("codex", Some(profile), &skills))
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
                level,
            )
            .expect_err("only the exercised level is certified");
            assert!(err.contains("certifies [read-only]"), "{err}");
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
